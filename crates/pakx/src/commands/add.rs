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
use pakx_core::{http_client, RegistrySource};
use pakx_registry_client::{
    CacheDir, OfficialMcpSource, PakxSource, RegistryClient, RegistryError, Source, PAKX_BASE_URL,
};
use tracing::{debug, warn};

use crate::redact::{project_root_for, redact_path};
use crate::registry_url::validate_base_url;
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

    /// Override the pakx-registry base URL (testing). Used by the
    /// kind-probe path that fires when the user supplied neither a
    /// `<kind>` positional nor `-t/--type` and the id has not been
    /// classified by the local `infer_kind` heuristic.
    #[arg(long, hide = true)]
    pub pakx_base_url: Option<String>,

    /// Bypass the federated-source cache for this invocation. Drops
    /// the per-call cache TTL to zero so the kind-probe and MCP
    /// validation paths re-query their upstreams rather than serving
    /// a stale "not found" response.
    #[arg(long)]
    pub no_cache: bool,
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

    // Validate any user-supplied base URLs BEFORE any HTTP work fires.
    // Mirrors `pakx install` + `pakx test` + `pakx outdated` so a
    // userinfo-smuggled override (`http://localhost@evil.com/`) is
    // rejected before the kind probe or MCP validation hits the wire.
    if let Some(u) = args.mcp_base_url.as_deref() {
        validate_base_url(u)?;
    }
    if let Some(u) = args.pakx_base_url.as_deref() {
        validate_base_url(u)?;
    }

    // Resolve kind:
    //   1. Explicit `<kind>` positional or `-t/--type` wins outright.
    //   2. Local `infer_kind` heuristic catches obvious shapes
    //      (`<owner>/skills/<name>`).
    //   3. Otherwise probe pakx-registry: it is the source of truth for
    //      first-party packages and will tell us `kind: "skills"` /
    //      `"mcp"` / etc. directly.
    //   4. If the probe 404s (or fails), fall back to MCP as the
    //      historical default — `pakx add` predates pakx-registry and
    //      almost every published id used to be an MCP server.
    //
    // `probed_pakx_404` records that step 3 fired AND found nothing,
    // which lets the MCP-registry validation produce a softened warn
    // ("not found in pakx-registry or the official MCP Registry") that
    // is honest about both sources having been consulted.
    let (kind, probed_pakx_404) = if let Some(k) = resolved.explicit_kind {
        (k.to_core(), false)
    } else {
        let local = infer_kind(&id);
        if local == PackageType::Skills {
            (local, false)
        } else {
            match probe_pakx_kind(&id, args.pakx_base_url.as_deref(), args.no_cache).await {
                Ok(Some(remote_kind)) => (remote_kind, false),
                Ok(None) => (PackageType::Mcp, true),
                Err(e) => {
                    // Network / decode error on the probe is non-fatal:
                    // keep the historical MCP-default behavior but log
                    // so users can see why their skill landed under
                    // `mcp:` if the registry was unreachable.
                    debug!(target: "pakx::add", %id, error = %e, "pakx-registry kind probe failed; falling back to MCP default");
                    (PackageType::Mcp, false)
                }
            }
        }
    };

    debug!(target: "pakx::add", %id, ?kind, probed_pakx_404, "resolved package kind");

    let mut manifest = load_or_init(&target)?;

    if !args.no_validate && kind == PackageType::Mcp {
        match validate_mcp(&id, args.mcp_base_url.as_deref(), args.no_cache).await {
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
                    warn!(target: "pakx::add", %id, probed_pakx_404, "not in registry; adding anyway");
                    if probed_pakx_404 {
                        eprintln!(
                            "{} {} not found in pakx-registry or the official MCP Registry; \
                             adding to manifest anyway as kind=mcp \
                             (override with -t skills if this is a pakx skill, or --no-validate to silence)",
                            ui::glyph_warn_err(),
                            id
                        );
                    } else {
                        eprintln!(
                            "{} {} not in the official MCP Registry; adding to manifest anyway (use --no-validate to silence)",
                            ui::glyph_warn_err(),
                            id
                        );
                    }
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

    // Convention across the CLI: machine-readable success lines go to
    // stdout, human progress / hint lines go to stderr. `pakx remove`
    // had this right; `pakx add` previously routed the success line
    // to stderr alongside the progress noise, so a script grepping
    // stdout for `added <id>` saw nothing. Aligning here keeps the
    // surface predictable across the add/remove pair.
    println!(
        "{} added {} ({})",
        ui::glyph_ok(),
        ui::success(&id),
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

/// Probe pakx-registry for `<owner>/<name>` and, if the package is
/// known, return its declared `kind` mapped onto [`PackageType`].
///
/// Returns:
///   * `Ok(Some(kind))` — package exists, kind known.
///   * `Ok(None)` — package does not exist (404) **or** exists but
///     reports an unknown/missing `kind` value (we prefer the safe
///     MCP default over guessing).
///   * `Err(_)` — transport / decode failure. Caller decides whether
///     to fall back silently or surface.
///
/// The id is rejected up front if it does not look like the
/// `<owner>/<name>` shape pakx-registry requires; in that case the
/// probe simply reports `Ok(None)` so callers fall through to the
/// historical default without firing a doomed HTTP request.
async fn probe_pakx_kind(
    id: &str,
    base_url_override: Option<&str>,
    no_cache: bool,
) -> Result<Option<PackageType>, RegistryError> {
    // pakx-registry ids are exactly `<owner>/<name>` with one slash.
    // Anything else (e.g. `io.github.acme/foo` MCP-shape, free-form
    // strings) cannot exist there — skip the round-trip.
    if !is_pakx_shaped_id(id) {
        return Ok(None);
    }
    let base = base_url_override.unwrap_or(PAKX_BASE_URL);
    // Per-call cache root keyed by pid + nanos so parallel integration
    // tests cannot collide. Mirrors `outdated::build_clients` and
    // `validate_mcp` below.
    let cache_root = std::env::temp_dir().join(format!(
        "pakx-add-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let cache = if no_cache {
        CacheDir::with_root(&cache_root).with_ttl(std::time::Duration::ZERO)
    } else {
        CacheDir::with_root(&cache_root)
    };
    let source = PakxSource::with_parts(http_client(), base, cache);
    match source.fetch(id).await {
        Ok(pkg) => Ok(pkg
            .install_hints
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(parse_registry_kind)),
        Err(RegistryError::NotFound { .. }) => Ok(None),
        Err(other) => Err(other),
    }
}

/// Cheap shape check matching `pakx_source::split_owner_name`: exactly
/// one `/`, both halves non-empty. We can't import the private helper,
/// and the rule is stable enough to mirror here verbatim.
fn is_pakx_shaped_id(id: &str) -> bool {
    match id.split_once('/') {
        Some((owner, rest)) => !owner.is_empty() && !rest.is_empty() && !rest.contains('/'),
        None => false,
    }
}

/// Map the registry's `kind` JSON string onto [`PackageType`].
/// Unknown / future kinds return `None` so the caller falls back to the
/// historical default rather than guessing.
fn parse_registry_kind(s: &str) -> Option<PackageType> {
    match s {
        "skills" => Some(PackageType::Skills),
        "mcp" => Some(PackageType::Mcp),
        "subagents" => Some(PackageType::Subagents),
        "prompts" => Some(PackageType::Prompts),
        "commands" => Some(PackageType::Commands),
        "hooks" => Some(PackageType::Hooks),
        _ => None,
    }
}

async fn validate_mcp(
    id: &str,
    base_url_override: Option<&str>,
    no_cache: bool,
) -> Result<String, RegistryError> {
    let base = base_url_override.unwrap_or(DEFAULT_MCP_BASE);
    // Per-call cache root — see `outdated::build_clients` for rationale.
    let cache_root = env::temp_dir().join(format!(
        "pakx-add-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let cache = if no_cache {
        CacheDir::with_root(&cache_root).with_ttl(std::time::Duration::ZERO)
    } else {
        CacheDir::with_root(&cache_root)
    };
    let source = OfficialMcpSource::with_parts(http_client(), base, cache);
    let client = RegistryClient::new().with_source(Box::new(source));
    let pkg = client.fetch(RegistrySource::OfficialMcp, id).await?;
    Ok(pkg.version)
}
