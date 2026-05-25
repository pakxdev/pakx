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
    OfficialMcpSource, Package, PakxSource, RegistryClient, RegistryError, Source, PAKX_BASE_URL,
};
use tracing::{debug, warn};

use crate::commands::cache_tempdir::{cache_dir_at, make_cache_tempdir};
use crate::install::mcp_translate::{translate, TranslateError};
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
                    // keep the historical MCP-default behavior. Surface a
                    // visible warn (not just debug) so the user knows the
                    // kind was GUESSED, not confirmed — and how to override
                    // if their package is actually a skill.
                    debug!(target: "pakx::add", %id, error = %e, "pakx-registry kind probe failed; falling back to MCP default");
                    eprintln!(
                        "{} couldn't reach pakx-registry to classify {} — defaulting to kind=mcp \
                         (re-run with `-t skills` if this is a pakx skill)",
                        ui::glyph_warn_err(),
                        id
                    );
                    (PackageType::Mcp, false)
                }
            }
        }
    };

    debug!(target: "pakx::add", %id, ?kind, probed_pakx_404, "resolved package kind");

    let mut manifest = load_or_init(&target)?;

    // When `true`, the trailing `→ next: pakx install` hint is suppressed
    // for this add. Set when validation proves `pakx install` will fail
    // for the id as published (e.g. an MCP server with no installable
    // transport), so we don't cheerfully point the user at a command that
    // can't succeed yet.
    let suppress_next_hint = if !args.no_validate && kind == PackageType::Mcp {
        validate_mcp_and_report(
            &id,
            args.mcp_base_url.as_deref(),
            args.no_cache,
            probed_pakx_404,
        )
        .await
    } else {
        false
    };

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
    //
    // Suppressed when validation proved the install can't succeed yet
    // (e.g. no installable transport) — pointing the user at
    // `pakx install` there would be a lie.
    if !suppress_next_hint {
        eprintln!("{}", ui::dim_err("\u{2192} next: pakx install"));
    }
    Ok(())
}

/// One-line summary of a registry validation error for the human warn
/// line. `RegistryError`'s `Display` can carry a multi-line transport
/// cause; we take the first line so the `[warn]` stays on one row.
fn short_validation_error(e: &RegistryError) -> String {
    let s = e.to_string();
    s.lines().next().unwrap_or(&s).to_owned()
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
    //
    // Wrapped in a `tempfile::TempDir` guard so the dir is removed on
    // function exit — otherwise a user who never runs `pakx doctor
    // --clear-cache` accumulates `pakx-add-probe-*` dirs in
    // `/tmp` indefinitely. The cache only matters for the lifetime of
    // this call (one fetch, no siblings), so dropping the dir at
    // return is correct.
    let cache_root =
        make_cache_tempdir("pakx-add-probe").map_err(|source| RegistryError::Cache {
            source,
            path: Some(std::env::temp_dir()),
        })?;
    let cache = cache_dir_at(cache_root.path(), no_cache);
    let source = PakxSource::with_parts(http_client(), base, cache);
    let result = source.fetch(id).await;
    // Keep `cache_root` alive until after the fetch completes — the
    // explicit drop documents the lifetime constraint that the cache
    // dir must outlive every cache read/write inside `source.fetch`.
    drop(cache_root);
    match result {
        Ok(pkg) => Ok(pkg
            .install_hints
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(parse_registry_kind)),
        Err(RegistryError::NotFound { .. }) => Ok(None),
        Err(other) => Err(other),
    }
}

/// Cheap shape check matching `pakx_source::split_owner_name`: split at
/// the **first** `/`, both halves non-empty. Multi-slash ids
/// (`io.github.acme/srv-name/extra`) are forwarded to the registry —
/// the registry's owner-login regex guarantees they can never resolve
/// to a real pakx package, but we still let the round-trip happen so
/// the upstream `Ok(None)` mapping in `probe_pakx_kind` fires off the
/// registry's `404` rather than a pre-flight reject. See the matching
/// `split_owner_name` doc comment in `pakx-registry-client` for the
/// background on the round-47 relaxation.
fn is_pakx_shaped_id(id: &str) -> bool {
    match id.split_once('/') {
        Some((owner, rest)) => !owner.is_empty() && !rest.is_empty(),
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

/// Validate an `mcp:` id against the official MCP Registry and print the
/// appropriate human line. Returns `true` when the trailing
/// `→ next: pakx install` hint should be SUPPRESSED — i.e. validation
/// proved `pakx install` can't yet succeed for this id (no installable
/// transport, or malformed install hints). Network / not-found problems
/// are downgraded to warnings and never suppress the hint (the add still
/// lands and may install fine once the upstream is reachable).
async fn validate_mcp_and_report(
    id: &str,
    mcp_base_url: Option<&str>,
    no_cache: bool,
    probed_pakx_404: bool,
) -> bool {
    match validate_mcp(id, mcp_base_url, no_cache).await {
        Ok(pkg) => {
            // The id exists in the MCP Registry — but existing isn't
            // enough: if it advertises NO installable transport,
            // `pakx install` will always fail for it. Run the same
            // translation the installer runs so the add is honest.
            match translate(&pkg) {
                Ok(_) => {
                    eprintln!(
                        "{} {} v{} via official MCP Registry",
                        ui::glyph_ok_err(),
                        id,
                        pkg.version
                    );
                    false
                }
                Err(TranslateError::NoTransport { .. }) => {
                    warn!(target: "pakx::add", %id, "id resolves but advertises no installable transport");
                    eprintln!(
                        "{} {} added, but it advertises no installable transport — \
                         `pakx install` will fail until the publisher adds one",
                        ui::glyph_warn_err(),
                        id
                    );
                    true
                }
                Err(other) => {
                    // Schema mismatch etc. — still let the add land, but
                    // don't promise a clean install.
                    warn!(target: "pakx::add", %id, error = %other, "install-hint translation failed during add validation");
                    eprintln!(
                        "{} {} added, but its registry install hints look malformed — \
                         `pakx install` may fail",
                        ui::glyph_warn_err(),
                        id
                    );
                    true
                }
            }
        }
        Err(RegistryError::NotFound { .. }) => {
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
            false
        }
        // A non-NotFound error means we could not REACH or parse the MCP
        // Registry (transient 500 / timeout / DNS / decode). That is no
        // reason to block a local manifest write — the user simply
        // couldn't validate right now. Downgrade to a warn and proceed
        // with the add (and keep the `→ next` hint: install may well
        // succeed once the upstream is back).
        Err(other) => {
            warn!(target: "pakx::add", %id, error = %other, "MCP Registry validation unreachable; adding anyway");
            eprintln!(
                "{} couldn't reach the MCP Registry to validate {} ({}) — adding anyway",
                ui::glyph_warn_err(),
                id,
                short_validation_error(&other),
            );
            false
        }
    }
}

async fn validate_mcp(
    id: &str,
    base_url_override: Option<&str>,
    no_cache: bool,
) -> Result<Package, RegistryError> {
    let base = base_url_override.unwrap_or(DEFAULT_MCP_BASE);
    // Per-call cache root — see `outdated::build_clients` for
    // rationale. Wrapped in `tempfile::TempDir` so the dir is removed
    // on function exit instead of accumulating in `/tmp`.
    let cache_root =
        make_cache_tempdir("pakx-add-cache").map_err(|source| RegistryError::Cache {
            source,
            path: Some(env::temp_dir()),
        })?;
    let cache = cache_dir_at(cache_root.path(), no_cache);
    let source = OfficialMcpSource::with_parts(http_client(), base, cache);
    let client = RegistryClient::new().with_source(Box::new(source));
    let pkg = client.fetch(RegistrySource::OfficialMcp, id).await?;
    // Drop the cache tempdir AFTER the fetch returns — the explicit
    // drop documents the lifetime constraint.
    drop(cache_root);
    Ok(pkg)
}
