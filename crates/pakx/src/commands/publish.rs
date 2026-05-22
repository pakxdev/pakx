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
use crate::ui;

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

    let pb = ui::spinner("packing");
    let pack = pack_dir(&src, std::env::temp_dir().as_path())?;
    pb.finish_and_clear();
    eprintln!(
        "{} packed {} ({} bytes)",
        ui::glyph_ok_err(),
        ui::success_err(&format!("{}@{}", pack.manifest.name, pack.manifest.version)),
        pack.bytes.len()
    );

    if args.dry_run {
        eprintln!("{}", ui::dim_err("dry-run: skipping upload"));
        return Ok(());
    }

    let backend = PakxBackend::new(&args.registry);
    let pb = ui::spinner("creating package row");
    // Spec §2 / parent prompt §Publish-emit: omit `sponsors` from the
    // POST body when the manifest declares none. The registry treats an
    // absent field as "no change" but an explicit `[]` as "clear", so
    // omitting on empty avoids wiping sponsors on a republish from an
    // older manifest that hasn't been re-edited.
    let sponsors_payload =
        (!pack.manifest.sponsors.is_empty()).then_some(pack.manifest.sponsors.as_slice());
    let pkg = backend
        .create_package(
            &entry.token,
            &CreatePackageRequest {
                name: &pack.manifest.name,
                kind: &args.kind,
                description: args.description.as_deref(),
                sponsors: sponsors_payload,
            },
        )
        .await
        .map_err(map_backend_err)?;
    pb.finish_and_clear();
    eprintln!(
        "{} {} {} on {}",
        ui::glyph_ok_err(),
        if pkg.created { "created" } else { "reusing" },
        pkg.id,
        args.registry
    );

    let pb = ui::spinner("uploading tarball");
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
    pb.finish_and_clear();
    eprintln!(
        "{} uploaded {} v{} ({} bytes, sha256 {})",
        ui::glyph_ok_err(),
        ui::success_err(&upload.id),
        upload.version,
        upload.size_bytes,
        &upload.sha256[..16]
    );
    eprintln!(
        "{}",
        ui::success_err(&format!(
            "published {}/{}@{}",
            pkg.owner, pkg.name, upload.version
        ))
    );
    // Single dimmed next-step hint pointing at the public dashboard
    // listing. The URL shape `https://pakx.dev/p/pakx/<owner>/<name>`
    // matches the dashboard route — the trailing `pakx` segment is
    // the source tag, mirroring the federated-source key used in
    // `agents.lock`.
    eprintln!(
        "{}",
        ui::dim_err(&format!(
            "\u{2192} view: https://pakx.dev/p/pakx/{}/{}",
            pkg.owner, pkg.name
        ))
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
