//! `pakx audit` — flag lockfile entries pinned to a deprecated version.
//!
//! Reads `agents.lock` (canonical pin source). For each entry, queries
//! the per-version endpoint:
//!
//! - `pakx` entries → `PakxSource::fetch_version(owner, name, version)`
//!   on `registry.pakx.dev`. The response's `deprecatedAt` field is the
//!   deprecation signal — non-null → the version is in the 30-day grace
//!   window after `pakx unpublish` and consumers should migrate off.
//! - `official-mcp` / `smithery` / `glama` / `github` / `git` entries
//!   are reported as `skip` — those registries do not expose a per-
//!   version deprecation signal today (`pakx outdated` applies the same
//!   discipline for sources without a "latest" probe).
//!
//! Exit code: `0` when no entries are deprecated (skip / error / ok all
//! tolerate), `1` when at least one entry is `deprecated`. Errors do
//! **not** trip the exit code — same shape as `pakx outdated`, so a
//! transient network blip never breaks a CI gate that only cares about
//! real deprecation. CI gate: `pakx audit || echo "deprecated dep"`.
//!
//! `--offline` performs the same walk WITHOUT any network I/O. The
//! deprecation signal lives behind `fetch_version`, which is a live
//! request (signed-URL TTL discipline means it never reads an on-disk
//! cache), so an offline audit cannot KNOW whether a pakx entry is
//! deprecated. Rather than emit a false `ok`, every pakx entry is
//! reported as `skip` with a `note` of `not checked (offline)` — the
//! same exit-code-neutral status used for sources that lack a
//! deprecation signal. Offline mode never trips exit code 1 (nothing
//! can be confirmed deprecated without the network); it exits 0 so an
//! airgapped CI gate degrades gracefully instead of false-failing.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use comfy_table::{Cell, CellAlignment};
use pakx_core::{http_client, read_lockfile_from, LockEntry, RegistrySource};
use pakx_registry_client::{PakxSource, RegistryError, PAKX_BASE_URL};
use serde::Serialize;
use tracing::debug;

use crate::commands::cache_tempdir::{cache_dir_at, make_cache_tempdir};
use crate::registry_url::validate_base_url;
use crate::ui;

const LOCKFILE_FILENAME: &str = "agents.lock";

/// Note attached to a pakx row that was skipped because `--offline`
/// suppressed the live `fetch_version` probe. Surfaced verbatim in the
/// human table and in the additive `note` JSON field so the `skip`
/// status reads honestly as "unknown, not checked" rather than "no
/// deprecation signal exists".
const OFFLINE_NOTE: &str = "not checked (offline)";

/// Note attached to non-pakx rows: those registries expose no
/// per-version deprecation signal, so the `skip` is structural, not a
/// consequence of `--offline`.
const NO_SIGNAL_NOTE: &str = "no deprecation signal";

/// Restrict the audit to entries from a single registry. Mirrors the
/// shape used by `pakx outdated --registry <tag>` so the two commands
/// feel like siblings — same flag, same parse, same kebab-case tags.
#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum RegistryFilter {
    Pakx,
    OfficialMcp,
    Smithery,
}

impl RegistryFilter {
    const fn matches(self, src: RegistrySource) -> bool {
        matches!(
            (self, src),
            (Self::Pakx, RegistrySource::Pakx)
                | (Self::OfficialMcp, RegistrySource::OfficialMcp)
                | (Self::Smithery, RegistrySource::Smithery)
        )
    }
}

#[derive(Debug, Clone, Args)]
pub struct AuditArgs {
    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Emit machine-readable JSON on stdout (single line,
    /// newline-terminated). Field names are a stable contract for
    /// downstream pipelines.
    #[arg(long)]
    pub json: bool,

    /// Restrict the audit to a single registry tag.
    #[arg(long, value_name = "TAG")]
    pub registry: Option<RegistryFilter>,

    /// Override the pakx-registry base URL (testing).
    #[arg(long, hide = true)]
    pub pakx_base_url: Option<String>,

    /// Override the official MCP Registry base URL (testing). Kept for
    /// shape symmetry with `pakx outdated`; today the audit does not
    /// query MCP per-server data, but a future per-server deprecation
    /// signal would land behind this flag.
    #[arg(long, hide = true)]
    pub mcp_base_url: Option<String>,

    /// Override the Smithery registry base URL (testing). Same
    /// rationale as `--mcp-base-url`.
    #[arg(long, hide = true)]
    pub smithery_base_url: Option<String>,

    /// Bypass the federated-source cache for this invocation. The
    /// `fetch_version` call this audit relies on already skips the
    /// cache (signed URLs are short-TTL), but the flag is accepted for
    /// shape parity with the other read commands so a CI script can
    /// pass `--no-cache` unconditionally across `pakx outdated`,
    /// `pakx audit`, `pakx search`, etc.
    #[arg(long)]
    pub no_cache: bool,

    /// Skip the network round-trip. Report purely from local state —
    /// every pakx entry is marked `skip` (note: `not checked
    /// (offline)`) because the deprecation signal can only be fetched
    /// live. Use this in airgapped / no-egress CI: the audit never
    /// false-fails, but it also can't confirm a deprecation, so it
    /// always exits 0.
    #[arg(long)]
    pub offline: bool,
}

/// Per-entry classification after the registry query.
///
/// `Deprecated` is the actionable row (exits 1). `Skip` covers
/// registries without a deprecation signal — informational, not an
/// error — and, under `--offline`, pakx entries whose signal could not
/// be fetched (the row's `note` distinguishes the two cases). `Ok`
/// means the version is still active. `Error` is surfaced both in the
/// table and on stderr without tripping the exit code so a transient
/// network blip doesn't break CI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Ok,
    Deprecated,
    Skip,
    Error,
}

impl Status {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Deprecated => "deprecated",
            Self::Skip => "skip",
            Self::Error => "error",
        }
    }
}

/// Wire-format row emitted by `--json`. Stable contract — only additive
/// changes (new optional fields) are backwards-compatible.
#[derive(Debug, Serialize)]
struct JsonRow<'a> {
    id: &'a str,
    version: &'a str,
    registry: &'static str,
    status: &'static str,
    #[serde(rename = "deprecatedAt")]
    deprecated_at: Option<&'a str>,
    /// Human-readable qualifier on a `skip` row. `not checked
    /// (offline)` for a pakx entry whose deprecation signal was
    /// suppressed by `--offline`; `no deprecation signal` for sources
    /// that structurally lack one. Omitted (not `null`) on `ok` /
    /// `deprecated` / `error` rows so the field never reads as a false
    /// signal. Additive contract — only present when meaningful.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

/// Internal row used for both human + JSON rendering.
#[derive(Debug, Clone)]
struct Row {
    id: String,
    version: String,
    registry: RegistrySource,
    status: Status,
    deprecated_at: Option<String>,
    /// Qualifier on a `skip` row. See [`JsonRow::note`]. `None` on
    /// non-skip rows.
    note: Option<&'static str>,
    /// Populated for `Status::Error` only. Routed to stderr; never
    /// painted into the table (would wrap and look ugly).
    error: Option<String>,
}

pub async fn run(args: AuditArgs) -> Result<ExitCode> {
    if args.json {
        // `--json | jq` discipline — keep stdout byte-clean.
        crate::ui::force_stdout_no_color();
    }
    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };
    let lockfile_path = project_root.join(LOCKFILE_FILENAME);
    let lock = read_lockfile_from(&lockfile_path)
        .with_context(|| format!("read lockfile {}", lockfile_path.display()))?;

    let Some(lock) = lock else {
        if args.json {
            println!("[]");
        } else {
            eprintln!("no {LOCKFILE_FILENAME} found — run `pakx install` first");
        }
        return Ok(ExitCode::SUCCESS);
    };

    if lock.entries.is_empty() {
        if args.json {
            println!("[]");
        } else {
            eprintln!("lockfile has no entries");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // `--mcp-base-url` / `--smithery-base-url` are accepted for shape
    // parity with `pakx outdated` but the audit doesn't query MCP /
    // Smithery today (those sources have no per-version deprecation
    // signal). Still validate the URLs early so a typo surfaces with a
    // clean error instead of silently being ignored.
    //
    // Under `--offline` we never construct a `PakxSource` (no HTTP
    // client, no cache tempdir) and never validate the override URLs —
    // they are inert in offline mode, and constructing the source would
    // be the only place that could touch the network.
    let pakx_source = if args.offline {
        None
    } else {
        let (pakx, cache_guard) = build_pakx_source(args.pakx_base_url.as_deref(), args.no_cache)?;
        if let Some(u) = args.mcp_base_url.as_deref() {
            validate_base_url(u)?;
        }
        if let Some(u) = args.smithery_base_url.as_deref() {
            validate_base_url(u)?;
        }
        Some((pakx, cache_guard))
    };

    let mut rows = Vec::with_capacity(lock.entries.len());
    for entry in lock.entries.values() {
        if let Some(filter) = args.registry {
            if !filter.matches(entry.registry) {
                continue;
            }
        }
        let row = match pakx_source.as_ref() {
            Some((pakx, _)) => audit_entry(entry, pakx).await,
            None => audit_entry_offline(entry),
        };
        rows.push(row);
    }

    render(&rows, args.json);

    // Offline mode can never confirm a deprecation (the signal is a
    // live fetch), so it never trips exit code 1 — an airgapped CI gate
    // degrades to "all clear" instead of false-failing. Online mode
    // keeps the round-49 contract: exit 1 iff any row is `deprecated`.
    let any_deprecated = !args.offline && rows.iter().any(|r| r.status == Status::Deprecated);
    Ok(if any_deprecated {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Build the `PakxSource` + a [`tempfile::TempDir`] guard rooted at
/// the per-call cache root.
///
/// Returning the guard alongside the source forces the caller to keep
/// it alive for the lifetime of every `fetch_version` call — without
/// it, the per-invocation `pakx-audit-cache-*` dir would accumulate
/// in `/tmp` whenever the user skips `pakx doctor --clear-cache`.
/// Same discipline applies in `pakx outdated`, `pakx search`,
/// `pakx add`.
fn build_pakx_source(
    pakx_base_url: Option<&str>,
    no_cache: bool,
) -> Result<(PakxSource, tempfile::TempDir)> {
    let pakx_url = match pakx_base_url {
        Some(u) => {
            validate_base_url(u)?;
            u
        }
        None => PAKX_BASE_URL,
    };
    // Use a tempdir-rooted cache so `pakx audit` never blocks on a
    // platform that lacks `CacheDir::default_path()`. The check is
    // read-only and the cache is incidental — a fresh cache per
    // invocation is fine. `PakxSource::fetch_version` itself never
    // touches the cache (signed-URL TTL discipline), so the cache
    // matters only for any future helper that piggy-backs on the same
    // `PakxSource` instance.
    //
    // The dir name is keyed on pid + nanos (via `make_cache_tempdir`)
    // so parallel integration tests don't share cache entries when
    // their `wiremock` mock servers happen to land on the same
    // loopback port. The `tempfile::TempDir` returned alongside the
    // source self-deletes on drop.
    let cache_root =
        make_cache_tempdir("pakx-audit-cache").context("create audit cache tempdir")?;
    let cache = cache_dir_at(cache_root.path(), no_cache);
    Ok((
        PakxSource::with_parts(http_client(), pakx_url, cache),
        cache_root,
    ))
}

async fn audit_entry(entry: &LockEntry, pakx: &PakxSource) -> Row {
    let id = entry.name.clone();
    let version = entry.version.clone();
    let registry = entry.registry;
    debug!(target: "pakx::audit", %id, %version, ?registry, "auditing entry");

    match registry {
        RegistrySource::Pakx => audit_pakx(id, version, registry, pakx).await,
        RegistrySource::OfficialMcp
        | RegistrySource::Smithery
        | RegistrySource::Glama
        | RegistrySource::Github
        | RegistrySource::Git => Row {
            id,
            version,
            registry,
            status: Status::Skip,
            deprecated_at: None,
            note: Some(NO_SIGNAL_NOTE),
            error: None,
        },
    }
}

/// Offline counterpart to [`audit_entry`]: classify every entry without
/// any network I/O. pakx entries become `skip` with the
/// [`OFFLINE_NOTE`] qualifier — the deprecation signal is a live fetch,
/// so offline we honestly report "not checked" rather than guessing
/// `ok`. Non-pakx entries are `skip` regardless of mode (no signal),
/// so they reuse the same classification as the online path.
fn audit_entry_offline(entry: &LockEntry) -> Row {
    let registry = entry.registry;
    let note = match registry {
        RegistrySource::Pakx => OFFLINE_NOTE,
        RegistrySource::OfficialMcp
        | RegistrySource::Smithery
        | RegistrySource::Glama
        | RegistrySource::Github
        | RegistrySource::Git => NO_SIGNAL_NOTE,
    };
    debug!(
        target: "pakx::audit",
        id = %entry.name,
        version = %entry.version,
        ?registry,
        "offline: skipping live deprecation probe"
    );
    Row {
        id: entry.name.clone(),
        version: entry.version.clone(),
        registry,
        status: Status::Skip,
        deprecated_at: None,
        note: Some(note),
        error: None,
    }
}

/// `pakx`-source audit: split the id, hit `fetch_version`, classify
/// the row. Pulled out as a helper so the outer `match` over
/// `RegistrySource` reads as one branch per source rather than nested
/// match-of-match-of-result (which clippy's `single_match_else`
/// rightly complains about).
async fn audit_pakx(
    id: String,
    version: String,
    registry: RegistrySource,
    pakx: &PakxSource,
) -> Row {
    let Some((owner, name)) = split_owner_name(&id) else {
        // A pakx lockfile entry without a `<owner>/<name>` id is a
        // corrupted lockfile — but the audit shouldn't blow up on it.
        // Surface as `Error`, leaving the rest of the walk intact.
        let reason = format!("not a valid <owner>/<name> id: {id:?}");
        eprintln!("{} {}: {}", ui::glyph_warn_err(), id, reason);
        return Row {
            id,
            version,
            registry,
            status: Status::Error,
            deprecated_at: None,
            note: None,
            error: Some(reason),
        };
    };
    match pakx.fetch_version(owner, name, &version).await {
        Ok(meta) => {
            let deprecated_at = meta.deprecated_at;
            let status = if deprecated_at.is_some() {
                Status::Deprecated
            } else {
                Status::Ok
            };
            Row {
                id,
                version,
                registry,
                status,
                deprecated_at,
                note: None,
                error: None,
            }
        }
        Err(e) => {
            let reason = format_registry_error(&e);
            // The raw error stays in `tracing` (debug) for diagnosis; the
            // user-facing line + table cell carry only the actionable
            // hint so a transient 5xx / DNS failure doesn't dump driver
            // jargon into the audit table.
            tracing::debug!(target: "pakx::audit", %id, %version, error = %e, "version fetch failed");
            // Print once to stderr so CI logs surface the reason
            // alongside the table. The table row itself stays terse.
            eprintln!("{} {}@{}: {}", ui::glyph_warn_err(), id, version, reason);
            Row {
                id,
                version,
                registry,
                status: Status::Error,
                deprecated_at: None,
                note: None,
                error: Some(reason),
            }
        }
    }
}

fn format_registry_error(e: &RegistryError) -> String {
    // Delegate to the shared mapper so `pakx audit` and `pakx outdated`
    // render the same actionable hints for the same failure classes
    // (transient / offline / not-found / invalid) rather than each
    // command leaking raw `RegistryError::Display` jargon into its table.
    crate::registry_hint::registry_error_hint(e)
}

fn split_owner_name(id: &str) -> Option<(&str, &str)> {
    let (owner, rest) = id.split_once('/')?;
    if owner.is_empty() || rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some((owner, rest))
}

fn render(rows: &[Row], json: bool) {
    if json {
        render_json(rows);
        return;
    }
    render_human(rows);
}

fn render_json(rows: &[Row]) {
    let json_rows: Vec<JsonRow<'_>> = rows
        .iter()
        .map(|r| JsonRow {
            id: r.id.as_str(),
            version: r.version.as_str(),
            registry: r.registry.as_tag(),
            status: r.status.as_str(),
            deprecated_at: r.deprecated_at.as_deref(),
            note: r.note,
            error: r.error.as_deref(),
        })
        .collect();
    let line = serde_json::to_string(&json_rows).expect("serialize audit rows");
    println!("{line}");
}

#[allow(clippy::cognitive_complexity)] // straight-through render: branching is shallow
fn render_human(rows: &[Row]) {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for r in rows {
        *counts.entry(r.status.as_str()).or_insert(0) += 1;
    }

    if rows.is_empty() {
        eprintln!("{}", ui::dim_err("\u{2192} no lockfile entries to audit"));
        return;
    }

    let mut table = ui::table();
    table.set_header(vec![
        Cell::new("package"),
        Cell::new("version").set_alignment(CellAlignment::Right),
        Cell::new("status"),
        Cell::new("deprecated_at"),
    ]);
    for r in rows {
        let deprecated_at = match (r.status, r.deprecated_at.as_deref()) {
            (Status::Deprecated, Some(ts)) => ts.to_owned(),
            // Skip rows print their note verbatim so offline pakx rows
            // ("not checked (offline)") read differently from
            // structural skips ("no deprecation signal"). Parenthesised
            // for the same visual cue the old single string carried.
            (Status::Skip, _) => format!("({})", r.note.unwrap_or(NO_SIGNAL_NOTE)),
            _ => "\u{2014}".to_owned(),
        };
        table.add_row(vec![
            Cell::new(&r.id),
            Cell::new(&r.version).set_alignment(CellAlignment::Right),
            Cell::new(r.status.as_str()),
            Cell::new(deprecated_at),
        ]);
    }
    println!("{table}");

    let total = rows.len();
    let deprecated = counts.get("deprecated").copied().unwrap_or(0);
    println!();
    println!(
        "{}: {} deprecated of {} entries",
        ui::heading("summary"),
        deprecated,
        total
    );
}

#[cfg(test)]
mod tests {
    use super::{split_owner_name, Status};

    #[test]
    fn status_as_str_matches_documented_contract() {
        // Status strings are part of the JSON contract. Pin them.
        assert_eq!(Status::Ok.as_str(), "ok");
        assert_eq!(Status::Deprecated.as_str(), "deprecated");
        assert_eq!(Status::Skip.as_str(), "skip");
        assert_eq!(Status::Error.as_str(), "error");
    }

    #[test]
    fn split_owner_name_accepts_canonical_form() {
        assert_eq!(split_owner_name("alice/hello"), Some(("alice", "hello")));
    }

    #[test]
    fn split_owner_name_rejects_malformed_ids() {
        assert!(split_owner_name("no-slash").is_none());
        assert!(split_owner_name("/right").is_none());
        assert!(split_owner_name("left/").is_none());
        assert!(split_owner_name("a/b/c").is_none());
    }
}
