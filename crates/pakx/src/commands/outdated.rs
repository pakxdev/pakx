//! `pakx outdated` — show lockfile entries whose source registry has a
//! newer non-deprecated version available.
//!
//! Reads `agents.lock` (canonical pin source). For each entry, queries
//! the appropriate registry source by id:
//!
//! - `pakx` entries → `GET /api/v1/packages/{owner}/{name}` on
//!   `registry.pakx.dev`. Latest = first non-deprecated version in the
//!   server-sorted `versions[]` array (the backend orders highest →
//!   lowest semver).
//! - `official-mcp` entries → `OfficialMcpSource::fetch` and pick the
//!   `version` field.
//! - `smithery` entries → `SmitherySource::fetch` similarly. Smithery
//!   uses a `"latest"` literal placeholder rather than semver — those
//!   entries are surfaced as `unknown` (cannot compare).
//! - `glama` / `github` / `git` entries are not yet wired and are
//!   reported as `skip` (informational; not an error).
//!
//! Comparison: `latest > current` → `upgrade`. `latest < current` →
//! `drift` (downgrade, unusual — usually means a pinned version was
//! unpublished and rolled back). Equal versions are counted as
//! up-to-date and skipped from the table.
//!
//! Registry unreachable: emits the error reason on stderr and marks
//! the entry as `error` — does **not** fail the whole command. The
//! `--json` shape gains `error: "<reason>"` so pipelines can inspect.
//!
//! Exit code: `0` when everything is up-to-date (no `upgrade` /
//! `drift` entries), `1` when anything is outdated — CI-friendly:
//! `pakx outdated || echo "deps drift"`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use comfy_table::{Cell, CellAlignment};
use pakx_core::{http_client, read_lockfile_from, LockEntry, RegistrySource};
use pakx_registry_client::{
    CacheDir, OfficialMcpSource, PakxSource, SmitherySource, Source, OFFICIAL_MCP_BASE_URL,
    PAKX_BASE_URL, SMITHERY_BASE_URL,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::registry_url::validate_base_url;
use crate::ui;

const LOCKFILE_FILENAME: &str = "agents.lock";

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
pub struct OutdatedArgs {
    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Emit machine-readable JSON on stdout (single line,
    /// newline-terminated). Field names are a stable contract for
    /// downstream pipelines.
    #[arg(long)]
    pub json: bool,

    /// Restrict the check to a single registry tag. Useful in CI when
    /// only first-party (`pakx`) drift matters.
    #[arg(long, value_name = "TAG")]
    pub registry: Option<RegistryFilter>,

    /// Override the pakx-registry base URL (testing).
    #[arg(long, hide = true)]
    pub pakx_base_url: Option<String>,

    /// Override the official MCP Registry base URL (testing).
    #[arg(long, hide = true)]
    pub mcp_base_url: Option<String>,

    /// Override the Smithery registry base URL (testing).
    #[arg(long, hide = true)]
    pub smithery_base_url: Option<String>,
}

/// Lockfile entry classification after the registry query.
///
/// `upgrade` / `drift` are the actionable rows. `up_to_date` is
/// counted but not rendered into the human table (would be noise).
/// `error` is surfaced both in the table and on stderr so the user
/// sees the reason without grepping logs. `skip` covers registries
/// without an outdated check yet — informational, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Upgrade,
    Drift,
    UpToDate,
    Unknown,
    Error,
    Skip,
}

impl Status {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Upgrade => "upgrade",
            Self::Drift => "drift",
            Self::UpToDate => "up-to-date",
            Self::Unknown => "unknown",
            Self::Error => "error",
            Self::Skip => "skip",
        }
    }
}

/// Wire-format row emitted by `--json`. Field names are a stable
/// contract — only additive changes (new optional fields) are
/// backwards-compatible.
#[derive(Debug, Serialize)]
struct JsonRow<'a> {
    id: &'a str,
    current: &'a str,
    latest: Option<&'a str>,
    registry: &'static str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

/// Internal report shape — used for both human + JSON rendering, and
/// re-exposed for `pakx update` which reuses the same registry-query
/// pipeline to decide which entries to rewrite. The struct is `pub` so
/// the `commands::update` module can read every field without
/// per-field getters; the binary's private `commands` module keeps it
/// from leaking outside the crate.
#[derive(Debug, Clone)]
pub struct Row {
    pub id: String,
    pub current: String,
    pub latest: Option<String>,
    pub registry: RegistrySource,
    pub status: Status,
    /// Populated for `Status::Error` only. Routed to stderr; never
    /// painted into the table (would wrap and look ugly).
    pub error: Option<String>,
}

/// Collect one `Row` per lockfile entry, honouring the optional
/// `registry` filter the same way `pakx outdated` does. Built so
/// `pakx update` can reuse the federated-query pipeline without going
/// through the CLI surface.
///
/// The `CacheDir` + `Source` instances are built fresh for each call
/// — `pakx update` and `pakx outdated` never run concurrently from
/// the same process, so a per-call cache root is fine.
pub async fn gather_outdated(
    project_root: &std::path::Path,
    pakx_base_url: Option<&str>,
    mcp_base_url: Option<&str>,
    smithery_base_url: Option<&str>,
    registry_filter: Option<RegistryFilter>,
) -> Result<Vec<Row>> {
    let lockfile_path = project_root.join(LOCKFILE_FILENAME);
    let lock = read_lockfile_from(&lockfile_path)
        .with_context(|| format!("read lockfile {}", lockfile_path.display()))?;
    let Some(lock) = lock else {
        return Ok(Vec::new());
    };
    if lock.entries.is_empty() {
        return Ok(Vec::new());
    }
    let clients = build_clients(pakx_base_url, mcp_base_url, smithery_base_url)?;
    let mut rows = Vec::with_capacity(lock.entries.len());
    for entry in lock.entries.values() {
        if let Some(filter) = registry_filter {
            if !filter.matches(entry.registry) {
                continue;
            }
        }
        rows.push(check_entry(entry, &clients).await);
    }
    Ok(rows)
}

pub async fn run(args: OutdatedArgs) -> Result<ExitCode> {
    if args.json {
        // Force stdout to no-color so `--color always --json | jq`
        // stays byte-clean. Stderr (the "no lockfile" / "no entries"
        // human notes below) remains color-able.
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

    let rows = gather_outdated(
        &project_root,
        args.pakx_base_url.as_deref(),
        args.mcp_base_url.as_deref(),
        args.smithery_base_url.as_deref(),
        args.registry,
    )
    .await?;

    render(&rows, args.json);

    // Exit non-zero when any actionable row is present. `error`
    // doesn't trip the exit code — it's already surfaced on stderr,
    // and a transient network blip shouldn't break a CI gate that
    // only cares about real drift.
    let any_outdated = rows
        .iter()
        .any(|r| matches!(r.status, Status::Upgrade | Status::Drift));
    Ok(if any_outdated {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

struct Clients {
    pakx: PakxSource,
    mcp: OfficialMcpSource,
    smithery: SmitherySource,
}

fn build_clients(
    pakx_base_url: Option<&str>,
    mcp_base_url: Option<&str>,
    smithery_base_url: Option<&str>,
) -> Result<Clients> {
    let pakx_url = match pakx_base_url {
        Some(u) => {
            validate_base_url(u)?;
            u
        }
        None => PAKX_BASE_URL,
    };
    let mcp_url = match mcp_base_url {
        Some(u) => {
            validate_base_url(u)?;
            u
        }
        None => OFFICIAL_MCP_BASE_URL,
    };
    let smithery_url = match smithery_base_url {
        Some(u) => {
            validate_base_url(u)?;
            u
        }
        None => SMITHERY_BASE_URL,
    };

    // Use a tempdir-rooted cache so `pakx outdated` never blocks on a
    // platform that lacks `CacheDir::default_path()`. The check is
    // read-only and the cache is incidental — a fresh cache per
    // invocation is fine.
    //
    // The dir name is keyed on pid + nanos so parallel integration
    // tests cannot share cache entries when their `wiremock` mock
    // servers happen to land on the same loopback port (Linux releases
    // ports aggressively; on a hot CI runner two sequential tests
    // routinely see a port collision). With per-call dirs the cache
    // key collision window goes to zero.
    let cache_root = std::env::temp_dir().join(format!(
        "pakx-outdated-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let http = http_client();
    Ok(Clients {
        pakx: PakxSource::with_parts(http.clone(), pakx_url, CacheDir::with_root(&cache_root)),
        mcp: OfficialMcpSource::with_parts(http.clone(), mcp_url, CacheDir::with_root(&cache_root)),
        smithery: SmitherySource::with_parts(http, smithery_url, CacheDir::with_root(&cache_root)),
    })
}

async fn check_entry(entry: &LockEntry, clients: &Clients) -> Row {
    let id = entry.name.clone();
    let current = entry.version.clone();
    let registry = entry.registry;
    debug!(target: "pakx::outdated", %id, %current, ?registry, "checking entry");

    let result = match registry {
        RegistrySource::Pakx => fetch_latest_pakx(&clients.pakx, &id).await,
        RegistrySource::OfficialMcp => fetch_latest_via_source(&clients.mcp, &id).await,
        RegistrySource::Smithery => fetch_latest_via_source(&clients.smithery, &id).await,
        RegistrySource::Glama | RegistrySource::Github | RegistrySource::Git => {
            return Row {
                id,
                current,
                latest: None,
                registry,
                status: Status::Skip,
                error: None,
            };
        }
    };

    match result {
        Ok(latest) => {
            let status = compare_versions(&current, &latest);
            Row {
                id,
                current,
                latest: Some(latest),
                registry,
                status,
                error: None,
            }
        }
        Err(e) => {
            // Print the reason once to stderr so CI logs surface it
            // alongside the table. The table row itself stays terse.
            eprintln!("{} {}: {}", ui::glyph_warn_err(), id, e);
            Row {
                id,
                current,
                latest: None,
                registry,
                status: Status::Error,
                error: Some(e),
            }
        }
    }
}

/// `pakx` source: prefer the per-package endpoint's `versions[]`
/// array and pick the first non-deprecated version. The backend
/// returns versions sorted highest → lowest semver, so the first
/// non-deprecated entry IS the latest non-deprecated. Falls back to
/// the federated `Source::fetch` shape if the detail response lacks
/// the array (older deployments).
async fn fetch_latest_pakx(source: &PakxSource, id: &str) -> Result<String, String> {
    let pkg = source.fetch(id).await.map_err(|e| e.to_string())?;
    // `PakxSource::fetch` writes the parsed `versions[]` into
    // `install_hints.versions` as `[{version, deprecatedAt?, ...}]`.
    if let Some(versions) = pkg.install_hints.get("versions").and_then(Value::as_array) {
        for v in versions {
            let version = v.get("version").and_then(Value::as_str);
            let deprecated = v.get("deprecatedAt").is_some_and(|d| !d.is_null());
            if let Some(version) = version {
                if !deprecated {
                    return Ok(version.to_owned());
                }
            }
        }
    }
    // Fallback: package-level `version` field (the federated source's
    // own latest hint). Honoured only when no `versions[]` array is
    // available — otherwise we'd risk picking a deprecated version.
    Ok(pkg.version)
}

/// Generic latest-version fetch via a `Source`. The federated source
/// already condenses "latest" into `Package::version`; for non-pakx
/// sources we trust that. Smithery returns the literal string
/// `"latest"` here — the comparison layer handles that case.
async fn fetch_latest_via_source<S: Source + ?Sized>(
    source: &S,
    id: &str,
) -> Result<String, String> {
    let pkg = source.fetch(id).await.map_err(|e| e.to_string())?;
    Ok(pkg.version)
}

/// Compare a lockfile-pinned version against the registry-side
/// latest. Pure function — exposed for the unit-test module below.
fn compare_versions(current: &str, latest: &str) -> Status {
    if current == latest {
        return Status::UpToDate;
    }
    let (Ok(cur), Ok(lat)) = (Version::parse(current), Version::parse(latest)) else {
        // Smithery's `"latest"` placeholder lands here. Equal strings
        // would have short-circuited above; an unequal-string-but-
        // unparseable pair is genuinely unknown.
        return Status::Unknown;
    };
    match lat.cmp(&cur) {
        Ordering::Greater => Status::Upgrade,
        Ordering::Less => Status::Drift,
        Ordering::Equal => Status::UpToDate,
    }
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
        .filter(|r| {
            // JSON contract surfaces only actionable rows + errors.
            // Up-to-date entries are excluded so a `jq 'length'`
            // produces the outdated count directly.
            matches!(
                r.status,
                Status::Upgrade | Status::Drift | Status::Error | Status::Unknown
            )
        })
        .map(|r| JsonRow {
            id: r.id.as_str(),
            current: r.current.as_str(),
            latest: r.latest.as_deref(),
            registry: r.registry.as_tag(),
            status: r.status.as_str(),
            error: r.error.as_deref(),
        })
        .collect();
    let line = serde_json::to_string(&json_rows).expect("serialize outdated rows");
    println!("{line}");
}

#[allow(clippy::cognitive_complexity)] // straight-through render: branching is shallow
fn render_human(rows: &[Row]) {
    // Group counts for the summary line.
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for r in rows {
        *counts.entry(r.status.as_str()).or_insert(0) += 1;
    }

    let actionable: Vec<&Row> = rows
        .iter()
        .filter(|r| matches!(r.status, Status::Upgrade | Status::Drift | Status::Error))
        .collect();

    if actionable.is_empty() {
        // Mirrors the `→ ...` hint cadence used by other action subcommands.
        eprintln!("{}", ui::dim_err("\u{2192} all dependencies up to date"));
        return;
    }

    let mut table = ui::table();
    table.set_header(vec![
        Cell::new("package"),
        Cell::new("current").set_alignment(CellAlignment::Right),
        Cell::new("latest").set_alignment(CellAlignment::Right),
        Cell::new("registry"),
        Cell::new("status"),
    ]);
    for r in &actionable {
        let latest = r.latest.as_deref().unwrap_or("-");
        table.add_row(vec![
            Cell::new(&r.id),
            Cell::new(&r.current).set_alignment(CellAlignment::Right),
            Cell::new(latest).set_alignment(CellAlignment::Right),
            Cell::new(r.registry.as_tag()),
            Cell::new(r.status.as_str()),
        ]);
    }
    println!("{table}");

    let total = rows.len();
    let outdated =
        counts.get("upgrade").copied().unwrap_or(0) + counts.get("drift").copied().unwrap_or(0);
    println!();
    println!(
        "{}: {} outdated of {} entries",
        ui::heading("summary"),
        outdated,
        total
    );
}

#[cfg(test)]
mod tests {
    use super::{compare_versions, Status};

    #[test]
    fn compare_versions_returns_upgrade_when_latest_is_higher_semver() {
        assert_eq!(compare_versions("0.1.0", "0.1.2"), Status::Upgrade);
        assert_eq!(compare_versions("1.2.0", "1.3.0"), Status::Upgrade);
        assert_eq!(compare_versions("0.9.0", "1.0.0"), Status::Upgrade);
    }

    #[test]
    fn compare_versions_returns_drift_when_latest_is_lower_semver() {
        // Unusual — usually means the pinned version was unpublished
        // and rolled back. Surfacing as `drift` lets the user notice
        // they're holding a stale pin against a regressed registry.
        assert_eq!(compare_versions("0.1.2", "0.1.0"), Status::Drift);
        assert_eq!(compare_versions("2.0.0", "1.9.9"), Status::Drift);
    }

    #[test]
    fn compare_versions_returns_up_to_date_when_equal() {
        assert_eq!(compare_versions("0.1.0", "0.1.0"), Status::UpToDate);
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Status::UpToDate);
    }

    #[test]
    fn compare_versions_returns_up_to_date_for_equal_non_semver() {
        // Smithery's `"latest"` placeholder: equal strings collapse
        // to `UpToDate` regardless of whether they parse as semver.
        assert_eq!(compare_versions("latest", "latest"), Status::UpToDate);
    }

    #[test]
    fn compare_versions_returns_unknown_for_non_semver_pair() {
        // Two different non-semver strings — can't compare, can't
        // claim they're equal. Honest answer: unknown.
        assert_eq!(compare_versions("latest", "stable"), Status::Unknown);
        assert_eq!(compare_versions("0.1.0", "latest"), Status::Unknown);
    }

    #[test]
    fn compare_versions_handles_semver_pre_release() {
        // semver pre-release ordering: 1.0.0 > 1.0.0-rc.1.
        assert_eq!(compare_versions("1.0.0-rc.1", "1.0.0"), Status::Upgrade);
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc.1"), Status::Drift);
    }

    #[test]
    fn status_as_str_matches_documented_contract() {
        // The status strings are part of the JSON contract. Pin them.
        assert_eq!(Status::Upgrade.as_str(), "upgrade");
        assert_eq!(Status::Drift.as_str(), "drift");
        assert_eq!(Status::UpToDate.as_str(), "up-to-date");
        assert_eq!(Status::Unknown.as_str(), "unknown");
        assert_eq!(Status::Error.as_str(), "error");
        assert_eq!(Status::Skip.as_str(), "skip");
    }
}
