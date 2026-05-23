//! `pakx publish [<path>]` — pack + upload to the pakx-registry.
//!
//! Two-step API contract:
//!   1. POST /api/v1/packages              { name, kind, description? }
//!      -> upserts the package row (owner is taken from the bearer token).
//!   2. PUT  /api/v1/packages/<owner>/<name>/<version>
//!      -> uploads the tarball bytes. Returns sha256 + signed URL.
//!
//! Output modes:
//!
//! - **Human (default).** Progress + warnings stream to stderr with the
//!   project's `[ok]` / `[warn]` glyph cadence; stdout stays silent.
//! - **`--json`.** Progress + warnings still go to stderr so CI logs
//!   keep the warning trail; stdout receives a **single**
//!   newline-terminated JSON object once the upload completes. Field
//!   names are a stable camelCase contract — `jq` consumers can pipe
//!   directly.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use pakx_core::{Credentials, DEFAULT_REGISTRY_URL};
use pakx_registry_client::{BackendError, CreatePackageRequest, PakxBackend};

use crate::pack::pack_dir;
use crate::registry_url::validate_base_url;
use crate::ui;

/// Closed set of package kinds the registry accepts. Constraining the
/// flag at clap-parse time means a typo (`--kind banan`) fails *before*
/// we pack the bundle + upload it — previously the wasted work
/// surfaced only as a registry-side 400 after the tarball round-trip.
/// Variant order + names mirror `pakx_core::manifest::PackageType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum PublishKind {
    Skills,
    Mcp,
    Subagents,
    Prompts,
    Commands,
    Hooks,
}

impl PublishKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Mcp => "mcp",
            Self::Subagents => "subagents",
            Self::Prompts => "prompts",
            Self::Commands => "commands",
            Self::Hooks => "hooks",
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct PublishArgs {
    /// Source directory. Defaults to cwd.
    pub source: Option<PathBuf>,

    /// Registry. Defaults to <https://registry.pakx.dev>.
    #[arg(short = 'r', long = "registry", default_value = DEFAULT_REGISTRY_URL)]
    pub registry: String,

    /// Package kind. Defaults to "skills" (the v0 use-case).
    ///
    /// Constrained to the six registry-known kinds (skills, mcp,
    /// subagents, prompts, commands, hooks) so a typo fails at flag
    /// parse — *before* we pack + upload — instead of bubbling up as
    /// a registry-side 400 after the tarball round-trip.
    #[arg(
        short = 'k',
        long = "kind",
        value_enum,
        default_value_t = PublishKind::Skills,
    )]
    pub kind: PublishKind,

    /// Optional one-line description.
    #[arg(short = 'd', long = "description")]
    pub description: Option<String>,

    /// Print what would happen but don't actually upload.
    #[arg(long)]
    pub dry_run: bool,

    /// Emit a single machine-readable JSON object on stdout describing
    /// the publish outcome. Progress lines + warnings still go to
    /// stderr. Field names are a stable contract for downstream
    /// pipelines (`registryUrl`, `tarballUrl`, `sha256`, ...).
    #[arg(long)]
    pub json: bool,

    #[arg(long, hide = true)]
    pub credentials_file: Option<PathBuf>,
}

#[allow(clippy::too_many_lines)] // linear flow; helpers would obscure shape
pub async fn run(args: PublishArgs) -> Result<()> {
    // Vet any user-supplied `--registry` BEFORE the credentials lookup
    // or any HTTP work. The publish flow sends the bearer token + the
    // tarball bytes; a userinfo-smuggled override would exfiltrate
    // both. Mirrors `pakx login` / `pakx install` discipline — the
    // single source of truth for the validator is
    // `crate::registry_url::validate_base_url`.
    if args.registry != DEFAULT_REGISTRY_URL {
        validate_base_url(&args.registry)?;
    }
    if args.json {
        // Keep stdout byte-clean for `--json | jq`. Spinners + progress
        // lines still color on stderr — only the machine-readable
        // payload route on stdout is flattened.
        ui::force_stdout_no_color();
    }
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
    // Warnings stream to stderr regardless of `--json` so CI logs always
    // surface the publisher hygiene hints (missing `description:`, etc.)
    // — the JSON payload also carries them so a `--json | jq .warnings`
    // pipeline doesn't need a separate stderr capture.
    for warning in &pack.warnings {
        eprintln!("{} {warning}", ui::glyph_warn_err());
    }
    eprintln!(
        "{} packed {} ({} bytes)",
        ui::glyph_ok_err(),
        ui::success_err(&format!("{}@{}", pack.manifest.name, pack.manifest.version)),
        pack.bytes.len()
    );

    if args.dry_run {
        // `--dry-run` short-circuits before the registry round-trip.
        // The JSON contract still applies: emit a stub object that
        // tooling can detect via `"ok": true` + `"dryRun": true` and
        // skip the assertion on `registryUrl` / `tarballUrl` /
        // `publishedAt`. Human mode keeps the v0 stderr hint.
        if args.json {
            let payload = serde_json::json!({
                "ok": true,
                "dryRun": true,
                "name": pack.manifest.name,
                "version": pack.manifest.version,
                "sizeBytes": pack.bytes.len(),
                "warnings": pack.warnings,
            });
            let line = serde_json::to_string(&payload).expect("serialize publish dry-run json");
            println!("{line}");
        } else {
            eprintln!("{}", ui::dim_err("dry-run: skipping upload"));
        }
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
                kind: args.kind.as_str(),
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

    if args.json {
        // Dashboard route — same shape as the human "→ view:" hint
        // below. Anchored on `https://pakx.dev` (the public dashboard)
        // independent of `args.registry`, which is the **API** base.
        let registry_url = format!(
            "https://pakx.dev/p/pakx/{}/{}/{}",
            pkg.owner, pkg.name, upload.version
        );
        let payload = serde_json::json!({
            "ok": true,
            "name": format!("{}/{}", pkg.owner, pkg.name),
            "version": upload.version,
            "sha256": upload.sha256,
            "sizeBytes": upload.size_bytes,
            "registryUrl": registry_url,
            "tarballUrl": upload.tarball_url,
            // `publishedAt` is part of the per-version detail endpoint
            // (see `pakx info --version`) but not the upload response,
            // so we emit `null` to keep the shape forward-compatible
            // — a future backend field would land here without
            // breaking jq pipelines that already key off it.
            "publishedAt": serde_json::Value::Null,
            "warnings": pack.warnings,
        });
        let line = serde_json::to_string(&payload).expect("serialize publish json");
        println!("{line}");
        return Ok(());
    }

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
