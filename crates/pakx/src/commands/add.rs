//! `pakx add <id>` — add a dependency to the manifest.
//!
//! Scope at v0.1: mutates `agents.yml` only. The resolve → install →
//! lockfile loop lives in `pakx install` (next subcommand) so the two
//! flows can be composed and tested independently.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, ValueEnum};
use pakx_core::manifest::{
    add_shorthand, read_from, write_to, AddOutcome, Dependencies, Manifest, PackageType,
};
use pakx_core::RegistrySource;
use pakx_registry_client::{CacheDir, OfficialMcpSource, RegistryClient, RegistryError};
use reqwest::Client;
use tracing::{debug, warn};

use crate::ui;

const MANIFEST_FILENAME: &str = "agents.yml";
const DEFAULT_MCP_BASE: &str = pakx_registry_client::OFFICIAL_MCP_BASE_URL;

/// CLI-facing copy of [`PackageType`] so clap can derive `ValueEnum`
/// (which we don't want to require on the core type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AddType {
    Skills,
    Mcp,
    Subagents,
    Prompts,
    Commands,
    Hooks,
}

impl AddType {
    const fn to_core(self) -> PackageType {
        match self {
            Self::Skills => PackageType::Skills,
            Self::Mcp => PackageType::Mcp,
            Self::Subagents => PackageType::Subagents,
            Self::Prompts => PackageType::Prompts,
            Self::Commands => PackageType::Commands,
            Self::Hooks => PackageType::Hooks,
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct AddArgs {
    /// Package id. Examples:
    ///   `io.github.modelcontextprotocol/server-filesystem`
    ///   `anthropics/skills/pdf`
    pub id: String,

    /// Override package-type inference.
    #[arg(short = 't', long = "type", value_enum)]
    pub kind: Option<AddType>,

    /// Skip the registry-validation network call.
    #[arg(long)]
    pub no_validate: bool,

    /// Operate on a manifest at a path other than `./agents.yml`.
    #[arg(long, hide = true)]
    pub manifest: Option<PathBuf>,

    /// Override the official MCP Registry base URL (testing).
    #[arg(long, hide = true)]
    pub mcp_base_url: Option<String>,
}

pub async fn run(args: AddArgs) -> Result<()> {
    let target = match args.manifest.clone() {
        Some(p) => p,
        None => env::current_dir()
            .context("cannot read current working directory")?
            .join(MANIFEST_FILENAME),
    };

    let kind = args
        .kind
        .map_or_else(|| infer_kind(&args.id), AddType::to_core);

    debug!(target: "pakx::add", id = %args.id, kind = ?kind, "resolved package kind");

    let mut manifest = load_or_init(&target)?;

    if !args.no_validate && kind == PackageType::Mcp {
        match validate_mcp(&args.id, args.mcp_base_url.as_deref()).await {
            Ok(version) => {
                eprintln!(
                    "{} {} v{} via official MCP Registry",
                    ui::glyph_ok_err(),
                    args.id,
                    version
                );
            }
            Err(e) => match e {
                RegistryError::NotFound { .. } => {
                    warn!(target: "pakx::add", id = %args.id, "not in official MCP Registry; adding anyway");
                    eprintln!(
                        "{} {} not in the official MCP Registry; adding to manifest anyway (use --no-validate to silence)",
                        ui::glyph_warn_err(),
                        args.id
                    );
                }
                other => bail!("registry validation failed: {other}"),
            },
        }
    }

    let outcome = add_shorthand(&mut manifest, kind, args.id.clone())
        .map_err(|e| anyhow!("invalid package id {:?}: {e}", args.id))?;

    match outcome {
        AddOutcome::AlreadyPresent => {
            eprintln!(
                "{} {} already declared under {}; manifest unchanged",
                ui::glyph_warn_err(),
                args.id,
                kind.as_str()
            );
            return Ok(());
        }
        AddOutcome::Added => {}
    }

    write_to(&target, &manifest).with_context(|| format!("write {}", target.display()))?;

    eprintln!(
        "{} added {} ({})",
        ui::glyph_ok_err(),
        ui::success_err(&args.id),
        kind.as_str(),
    );
    eprintln!("       run `pakx install` to apply");
    Ok(())
}

/// Heuristic: `<owner>/skills/<name>` (literal `/skills/` segment) reads
/// as a skill; otherwise default to MCP since most discoverable
/// packages today are MCP servers.
fn infer_kind(id: &str) -> PackageType {
    if id.contains("/skills/") {
        PackageType::Skills
    } else {
        PackageType::Mcp
    }
}

fn load_or_init(target: &Path) -> Result<Manifest> {
    if target.exists() {
        return read_from(target).map_err(|e| anyhow!(e));
    }
    let default_name = target
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("my-project")
        .to_owned();
    Ok(Manifest {
        name: default_name,
        version: "0.0.0".to_owned(),
        agents: None,
        dependencies: Dependencies::default(),
    })
}

async fn validate_mcp(id: &str, base_url_override: Option<&str>) -> Result<String, RegistryError> {
    let base = base_url_override.unwrap_or(DEFAULT_MCP_BASE);
    let cache_root = env::temp_dir().join("pakx-add-cache");
    let cache = CacheDir::with_root(&cache_root);
    let source = OfficialMcpSource::with_parts(Client::new(), base, cache);
    let client = RegistryClient::new().with_source(Box::new(source));
    let pkg = client.fetch(RegistrySource::OfficialMcp, id).await?;
    Ok(pkg.version)
}
