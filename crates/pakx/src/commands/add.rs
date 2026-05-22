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

use crate::redact::{project_root_for, redact_path};
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
    /// Either the package id, or — when a second positional follows —
    /// the kind (`skills`, `mcp`, `subagents`, `prompts`, `commands`,
    /// `hooks`). Two-positional form lets users type `pakx add mcp foo/bar`
    /// the way every other package manager works; the single-positional
    /// form is preserved unchanged.
    pub id_or_kind: String,

    /// Optional second positional: the package id when the first
    /// positional is a `<kind>` token.
    pub id: Option<String>,

    /// Override package-type inference. Accepts the same kinds as the
    /// two-positional form.
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

/// Resolved positional/flag combination after parsing the dual-form
/// argument shape. Held as a struct to keep the run-time validation in
/// one place and to make `run()` linear.
struct ResolvedArgs {
    id: String,
    /// Kind explicitly chosen by the user, via either the
    /// `<kind> <id>` two-positional form or the `-t` / `--type` flag.
    /// `None` falls through to [`infer_kind`].
    explicit_kind: Option<AddType>,
}

/// Resolve the dual-positional form into a single `(id, kind?)` pair.
/// Returns an error if the user supplied a `<kind>` positional AND
/// `-t`/`--type` (ambiguous), or if the first positional in two-
/// positional form is not a valid kind token.
fn resolve_positional_form(args: &AddArgs) -> Result<ResolvedArgs> {
    match args.id.as_ref() {
        // Two positionals: first must be a kind.
        Some(id) => {
            let kind = parse_kind_token(&args.id_or_kind).ok_or_else(|| {
                anyhow!(
                    "first positional '{}' is not a valid kind; expected one of \
                     skills|mcp|subagents|prompts|commands|hooks, or use a single positional '<id>'",
                    args.id_or_kind
                )
            })?;
            if args.kind.is_some() {
                bail!(
                    "kind specified twice \u{2014} use either `<kind> <id>` positional or `-t/--type`, not both"
                );
            }
            Ok(ResolvedArgs {
                id: id.clone(),
                explicit_kind: Some(kind),
            })
        }
        // Single positional: treat as id, keep existing -t / infer path.
        None => Ok(ResolvedArgs {
            id: args.id_or_kind.clone(),
            explicit_kind: args.kind,
        }),
    }
}

/// Parse a kind token (`skills`, `mcp`, ...) for the two-positional
/// form. Case-sensitive lowercase to match the documented surface and
/// the `AddType`/`PackageType` serde representation.
fn parse_kind_token(s: &str) -> Option<AddType> {
    match s {
        "skills" => Some(AddType::Skills),
        "mcp" => Some(AddType::Mcp),
        "subagents" => Some(AddType::Subagents),
        "prompts" => Some(AddType::Prompts),
        "commands" => Some(AddType::Commands),
        "hooks" => Some(AddType::Hooks),
        _ => None,
    }
}

pub async fn run(args: AddArgs) -> Result<()> {
    let target = match args.manifest.clone() {
        Some(p) => p,
        None => env::current_dir()
            .context("cannot read current working directory")?
            .join(MANIFEST_FILENAME),
    };

    let resolved = resolve_positional_form(&args)?;
    let id = resolved.id;
    let kind = resolved
        .explicit_kind
        .map_or_else(|| infer_kind(&id), AddType::to_core);

    debug!(target: "pakx::add", %id, ?kind, "resolved package kind");

    let mut manifest = load_or_init(&target)?;

    if !args.no_validate && kind == PackageType::Mcp {
        match validate_mcp(&id, args.mcp_base_url.as_deref()).await {
            Ok(version) => {
                eprintln!(
                    "{} {} v{} via official MCP Registry",
                    ui::glyph_ok_err(),
                    id,
                    version
                );
            }
            Err(e) => match e {
                RegistryError::NotFound { .. } => {
                    warn!(target: "pakx::add", %id, "not in official MCP Registry; adding anyway");
                    eprintln!(
                        "{} {} not in the official MCP Registry; adding to manifest anyway (use --no-validate to silence)",
                        ui::glyph_warn_err(),
                        id
                    );
                }
                other => bail!("registry validation failed: {other}"),
            },
        }
    }

    let outcome = add_shorthand(&mut manifest, kind, id.clone())
        .map_err(|e| anyhow!("invalid package id {id:?}: {e}"))?;

    match outcome {
        AddOutcome::AlreadyPresent => {
            eprintln!(
                "{} {} already declared under {}; manifest unchanged",
                ui::glyph_warn_err(),
                id,
                kind.as_str()
            );
            return Ok(());
        }
        AddOutcome::Added => {}
    }

    let project_root = project_root_for(&target);
    write_to(&target, &manifest)
        .with_context(|| format!("write {}", redact_path(&target, &project_root)))?;

    eprintln!(
        "{} added {} ({})",
        ui::glyph_ok_err(),
        ui::success_err(&id),
        kind.as_str(),
    );
    // Single next-step hint, dimmed so it sits visually behind the
    // success line. Mirrored verbatim across action subcommands
    // (`add` / `remove` / `install` / `pack` / `publish` / `unpublish`
    // / `login` / `test`) so users learn the rhythm: every action
    // command ends with exactly one `→ next: <command>` line.
    // The leading character is U+2192 RIGHTWARDS ARROW, written as
    // an escape so source files stay valid UTF-8 without an embedded
    // glyph some Windows terminals re-encode.
    eprintln!("{}", ui::dim_err("\u{2192} next: pakx install"));
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
