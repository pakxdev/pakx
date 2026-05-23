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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use comfy_table::{Cell, CellAlignment};
use pakx_core::{http_client, read_lockfile_from, LockEntry, RegistrySource};
use pakx_registry_client::{CacheDir, PakxSource, RegistryError, PAKX_BASE_URL};
use serde::Serialize;
use tracing::debug;

use crate::registry_url::validate_base_url;
use crate::ui;

const LOCKFILE_FILENAME: &str = "agents.lock";

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
}

/// Per-entry classification after the registry query.
///
/// `Deprecated` is the actionable row (exits 1). `Skip` covers
/// registries without a deprecation signal — informational, not an
/// error. `Ok` means the version is still active. `Error` is surfaced
/// both in the table and on stderr without tripping the exit code so a
/// transient network blip doesn't break CI.
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
    let pakx = build_pakx_source(args.pakx_base_url.as_deref(), args.no_cache)?;
    if let Some(u) = args.mcp_base_url.as_deref() {
        validate_base_url(u)?;
    }
    if let Some(u) = args.smithery_base_url.as_deref() {
        validate_base_url(u)?;
    }

    let mut rows = Vec::with_capacity(lock.entries.len());
    for entry in lock.entries.values() {
        if let Some(filter) = args.registry {
            if !filter.matches(entry.registry) {
                continue;
            }
        }
        rows.push(audit_entry(entry, &pakx).await);
    }

    render(&rows, args.json);

    let any_deprecated = rows.iter().any(|r| r.status == Status::Deprecated);
    Ok(if any_deprecated {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn build_pakx_source(pakx_base_url: Option<&str>, no_cache: bool) -> Result<PakxSource> {
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
    // The dir name is keyed on pid + nanos so parallel integration
    // tests don't share cache entries when their `wiremock` mock
    // servers happen to land on the same loopback port — same
    // discipline as `pakx outdated`, `pakx search`, `pakx add`.
    let cache_root = std::env::temp_dir().join(format!(
        "pakx-audit-cache-{}-{}",
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
    Ok(PakxSource::with_parts(http_client(), pakx_url, cache))
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
            error: None,
        },
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
                error: None,
            }
        }
        Err(e) => {
            let reason = format_registry_error(&e);
            // Print once to stderr so CI logs surface the reason
            // alongside the table. The table row itself stays terse.
            eprintln!("{} {}@{}: {}", ui::glyph_warn_err(), id, version, reason);
            Row {
                id,
                version,
                registry,
                status: Status::Error,
                deprecated_at: None,
                error: Some(reason),
            }
        }
    }
}

fn format_registry_error(e: &RegistryError) -> String {
    match e {
        RegistryError::NotFound { id, .. } => format!("not found: {id}"),
        other => other.to_string(),
    }
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
            (Status::Skip, _) => "(no deprecation signal)".to_owned(),
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
