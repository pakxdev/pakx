//! `pakx publish [<path>]` — pack + upload to the pakx-registry.
//!
//! Two-step API contract:
//!   1. POST /api/v1/packages              { name, kind, description? }
//!      -> upserts the package row (owner is taken from the bearer token).
//!   2. PUT  /api/v1/packages/<owner>/<name>/<version>
//!      -> uploads the tarball bytes. Returns sha256 + signed URL.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use pakx_core::{Credentials, DEFAULT_REGISTRY_URL};
use pakx_registry_client::{BackendError, CreatePackageRequest, PakxBackend};

use crate::pack::pack_dir;

#[derive(Debug, Clone, Args)]
pub struct PublishArgs {
    /// Source directory. Defaults to cwd.
    pub source: Option<PathBuf>,

    /// Registry. Defaults to <https://registry.pakx.dev>.
    #[arg(short = 'r', long = "registry", default_value = DEFAULT_REGISTRY_URL)]
    pub registry: String,

    /// Package kind. Defaults to "skills" (the v0 use-case).
    #[arg(short = 'k', long = "kind", default_value = "skills")]
    pub kind: String,

    /// Optional one-line description.
    #[arg(short = 'd', long = "description")]
    pub description: Option<String>,

    /// Print what would happen but don't actually upload.
    #[arg(long)]
    pub dry_run: bool,

    #[arg(long, hide = true)]
    pub credentials_file: Option<PathBuf>,
}

pub async fn run(args: PublishArgs) -> Result<()> {
    let src = args.source.clone().unwrap_or_else(|| PathBuf::from("."));
    let creds_path = match args.credentials_file.clone() {
        Some(p) => p,
        None => Credentials::default_path().context("resolve credentials path")?,
    };
    let creds = Credentials::read_from(&creds_path).context("read credentials")?;
    let entry = creds
        .get(&args.registry)
        .ok_or_else(|| anyhow::anyhow!("not logged in to {} — run `pakx login`", args.registry))?;

    let pack = pack_dir(&src, std::env::temp_dir().as_path())?;
    eprintln!(
        "packed {}@{} ({} bytes)",
        pack.manifest.name,
        pack.manifest.version,
        pack.bytes.len()
    );

    if args.dry_run {
        eprintln!("dry-run: skipping upload");
        return Ok(());
    }

    let backend = PakxBackend::new(&args.registry);
    let pkg = backend
        .create_package(
            &entry.token,
            &CreatePackageRequest {
                name: &pack.manifest.name,
                kind: &args.kind,
                description: args.description.as_deref(),
            },
        )
        .await
        .map_err(map_backend_err)?;
    eprintln!(
        "{} {} on {}",
        if pkg.created { "created" } else { "reusing" },
        pkg.id,
        args.registry
    );

    let upload = backend
        .upload_version(
            &entry.token,
            &pkg.owner,
            &pkg.name,
            &pack.manifest.version,
            pack.bytes,
        )
        .await
        .map_err(map_backend_err)?;
    eprintln!(
        "uploaded {} v{} ({} bytes, sha256 {})",
        upload.id,
        upload.version,
        upload.size_bytes,
        &upload.sha256[..16]
    );
    Ok(())
}

fn map_backend_err(e: BackendError) -> anyhow::Error {
    match e {
        BackendError::Unauthorized => {
            anyhow::anyhow!("registry rejected the token — run `pakx login` again")
        }
        BackendError::Forbidden => {
            anyhow::anyhow!("you don't own this package — pick a different name")
        }
        BackendError::Conflict { message } => {
            anyhow::anyhow!("version already published: {message}")
        }
        BackendError::NotFound => anyhow::anyhow!("package not found on registry"),
        other => anyhow::anyhow!(other),
    }
}
