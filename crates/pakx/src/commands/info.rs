//! `pakx info <owner>/<name>` — print registry-side metadata + version
//! list for a published package without installing it. Read-only.
//!
//! With `--version <ver>` the command instead fetches the per-version
//! endpoint (`GET /api/v1/packages/{owner}/{name}/{version}`), which
//! returns the immutable per-version metadata block — sha256,
//! sizeBytes, publishedAt, deprecatedAt, and a **signed, short-TTL**
//! tarballUrl. The signed URL is what the installer downloads from;
//! the list/detail endpoint deliberately omits it to avoid minting
//! one signed URL per version on a single list page.

use anyhow::{anyhow, Result};
use clap::Args;
use comfy_table::{Cell, CellAlignment};
use pakx_core::{http_client, Sponsor};
use pakx_registry_client::{CacheDir, PackageVersion, PakxSource};
use serde::Deserialize;

use crate::registry_url::validate_base_url;
use crate::ui;

const DEFAULT_REGISTRY: &str = "https://registry.pakx.dev";
const USER_AGENT: &str = concat!("pakx/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Args)]
#[command(disable_version_flag = true)]
pub struct InfoArgs {
    /// Canonical `<owner>/<name>` of the package.
    pub id: String,

    /// Fetch and render the per-version metadata block — sha256,
    /// sizeBytes, publishedAt, and the signed tarballUrl — instead of
    /// the package-level metadata + version list. Only supported for
    /// pakx-source packages today.
    #[arg(long, value_name = "VER")]
    pub version: Option<String>,

    /// Override the pakx-registry base URL.
    #[arg(long, default_value = DEFAULT_REGISTRY)]
    pub registry: String,

    /// Print JSON instead of the human-friendly table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Deserialize)]
struct PackageDetail {
    id: String,
    kind: Option<String>,
    description: Option<String>,
    created_at: Option<String>,
    #[serde(default)]
    sponsors: Vec<Sponsor>,
    #[serde(default)]
    versions: Vec<VersionEntry>,
}

#[derive(Debug, Deserialize)]
struct VersionEntry {
    version: String,
    sha256: Option<String>,
    size_bytes: Option<u64>,
    published_at: Option<String>,
    #[serde(default)]
    deprecated_at: Option<String>,
}

#[allow(clippy::too_many_lines)] // linear branches; helpers would obscure shape
pub async fn run(args: InfoArgs) -> Result<()> {
    let (owner, name) = split_id(&args.id)?;
    // Vet any user-supplied `--registry` BEFORE any HTTP work. Even
    // though this command is read-only, leaking the queried `<owner>/
    // <name>` over plaintext HTTP would still hand a network observer
    // the user's package-of-interest. Mirrors `pakx install` / `pakx
    // outdated` discipline.
    if args.registry != DEFAULT_REGISTRY {
        validate_base_url(&args.registry)?;
    }
    if args.json {
        // JSON output must never carry ANSI escapes — even though the
        // current render path doesn't actively inject any, the future
        // contract for `--json | jq` is "byte-clean stdout". Mirrors
        // the other `--json` subcommands (search, list, outdated, ...).
        ui::force_stdout_no_color();
    }
    if let Some(version) = args.version.as_deref() {
        return run_version(&args, &owner, &name, version).await;
    }
    let url = format!(
        "{}/api/v1/packages/{}/{}",
        args.registry.trim_end_matches('/'),
        owner,
        name,
    );
    // Wrap the network call in `with_context` so a connection refused
    // / DNS failure shows the registry URL the user gave us, not the
    // full reqwest error chain (which leaks the userinfo-stripped URL
    // and a 3-level cause stack that confuses adopters).
    let response = http_client()
        .get(&url)
        .header("user-agent", USER_AGENT)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| anyhow!("could not reach {}: {e}", args.registry))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!("{}/{} not found on {}", owner, name, args.registry));
    }
    let response = response
        .error_for_status()
        .map_err(|e| anyhow!("registry returned an error: {e}"))?;
    let detail: PackageDetail = response
        .json()
        .await
        .map_err(|e| anyhow!("registry response was not valid JSON: {e}"))?;

    if args.json {
        // `sponsors` is a **stable** field on the `--json` contract per
        // spec §2 — always emit the array (empty when none) so callers
        // can rely on `.sponsors | length` without null-checking.
        let raw = serde_json::to_string_pretty(&serde_json::json!({
            "id": detail.id,
            "kind": detail.kind,
            "description": detail.description,
            "createdAt": detail.created_at,
            "sponsors": detail.sponsors.iter().map(|s| serde_json::json!({
                "kind": s.kind.as_str(),
                "url": s.url,
            })).collect::<Vec<_>>(),
            "versions": detail.versions.iter().map(|v| serde_json::json!({
                "version": v.version,
                "sha256": v.sha256,
                "sizeBytes": v.size_bytes,
                "publishedAt": v.published_at,
                "deprecatedAt": v.deprecated_at,
            })).collect::<Vec<_>>(),
        }))?;
        println!("{raw}");
        return Ok(());
    }

    println!("{}", ui::heading(&detail.id));
    if let Some(k) = &detail.kind {
        println!("  {} {}", ui::dim("kind:       "), k);
    }
    if let Some(d) = &detail.description {
        println!("  {} {}", ui::dim("description:"), d);
    }
    if let Some(c) = &detail.created_at {
        println!("  {} {}", ui::dim("created:    "), c);
    }
    println!("  {} {}", ui::dim("registry:   "), args.registry);
    // Sponsors render between the description block and the versions
    // table per spec §7 open-question #7. Heading-less when empty so a
    // sponsor-less package looks the same as before this feature.
    if !detail.sponsors.is_empty() {
        println!();
        println!("{}", ui::heading("sponsors:"));
        for s in &detail.sponsors {
            // Pad the kind to a fixed width so the URLs line up
            // visually across rows. Matches the dim/label cadence used
            // by the `kind:` / `description:` / `created:` lines above.
            println!(
                "  {} {}",
                ui::dim(&format!("{:<8}", s.kind.as_str())),
                s.url
            );
        }
    }
    println!();
    if detail.versions.is_empty() {
        println!("  {}", ui::dim("no versions published yet."));
    } else {
        println!("{}", ui::heading("versions:"));
        let mut table = ui::table();
        table.set_header(vec![
            Cell::new("version").set_alignment(CellAlignment::Right),
            Cell::new("size").set_alignment(CellAlignment::Right),
            Cell::new("published"),
            Cell::new("status"),
        ]);
        for v in &detail.versions {
            let size = v.size_bytes.map_or_else(|| "-".to_string(), human_bytes);
            let published = v.published_at.as_deref().unwrap_or("-");
            let status = if v.deprecated_at.is_some() {
                "deprecated"
            } else {
                "active"
            };
            table.add_row(vec![
                Cell::new(v.version.as_str()).set_alignment(CellAlignment::Right),
                Cell::new(size).set_alignment(CellAlignment::Right),
                Cell::new(published),
                Cell::new(status),
            ]);
        }
        println!("{table}");
    }
    Ok(())
}

/// `--version <ver>` branch — fetch the per-version endpoint via
/// `PakxSource::fetch_version` (the same call the installer makes when
/// resolving a pinned version) and render the per-version detail
/// block. The signed `tarballUrl` is short-TTL so the human render
/// includes an expiry note; the JSON render emits the raw URL plus the
/// rest of the per-version fields verbatim.
///
/// `kind` is **not** part of the per-version endpoint shape — it lives
/// on the package-level row — so we additionally fire a best-effort
/// GET to `/api/v1/packages/{owner}/{name}` to thread the kind through
/// to the install-hint footer (so `pakx info <id> --version <ver>` no
/// longer prints `-t skills` for an mcp / hooks / commands package).
/// The kind lookup is best-effort: a 404 or transport error degrades
/// gracefully — the install hint just omits the `-t <kind>` flag.
async fn run_version(args: &InfoArgs, owner: &str, name: &str, version: &str) -> Result<()> {
    // `PakxSource::fetch_version` does **not** cache (signed URLs are
    // short-TTL), but the constructor still requires a `CacheDir` for
    // the search/detail paths. We give it a transient tempdir so a
    // misconfigured machine cache doesn't break `pakx info --version`
    // and so we don't pollute the user's persistent cache with a
    // throwaway entry.
    let cache_root =
        tempfile::tempdir().map_err(|e| anyhow!("could not create temp cache dir: {e}"))?;
    let cache = CacheDir::with_root(cache_root.path());
    let source = PakxSource::with_parts(http_client(), &args.registry, cache);
    let meta = source
        .fetch_version(owner, name, version)
        .await
        .map_err(|e| match e {
            pakx_registry_client::RegistryError::NotFound { id, .. } => {
                anyhow!("{id} not found on {}", args.registry)
            }
            other => anyhow!("could not reach {}: {other}", args.registry),
        })?;
    let kind = fetch_package_kind(&args.registry, owner, name).await;

    if args.json {
        let raw = serde_json::to_string_pretty(&serde_json::json!({
            "id": meta.id.clone().unwrap_or_else(|| format!("{owner}/{name}")),
            "version": meta.version,
            "kind": kind,
            "sha256": meta.sha256,
            "sizeBytes": meta.size_bytes,
            "publishedAt": meta.published_at,
            "deprecatedAt": meta.deprecated_at,
            "tarballUrl": meta.tarball_url,
        }))?;
        println!("{raw}");
        return Ok(());
    }

    render_version_human(args, owner, name, &meta, kind.as_deref());
    Ok(())
}

/// Best-effort fetch of the package-level `kind` discriminator from
/// `/api/v1/packages/{owner}/{name}`. Returns `None` on any failure
/// path — 404, transport error, malformed JSON, or a missing / unknown
/// kind value. Callers fall back to an install hint without `-t <kind>`
/// rather than guessing.
async fn fetch_package_kind(registry: &str, owner: &str, name: &str) -> Option<String> {
    let url = format!(
        "{}/api/v1/packages/{}/{}",
        registry.trim_end_matches('/'),
        owner,
        name,
    );
    let response = http_client()
        .get(&url)
        .header("user-agent", USER_AGENT)
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let detail: PackageDetail = response.json().await.ok()?;
    detail.kind.filter(|k| !k.is_empty())
}

/// Human-friendly render of a single per-version detail block. Matches
/// the indentation / label-width cadence used by the package-level
/// render above (`kind:` / `description:` / `created:`) so both views
/// look like the same command's output.
///
/// `kind` threads through from the package-level fetch (best-effort —
/// `None` when the lookup 404s or fails). When known, the install hint
/// becomes `pakx add <id>@<ver> -t <kind>`; when unknown the `-t`
/// suffix is omitted entirely so the user sees a runnable command
/// rather than a wrong-kind one.
fn render_version_human(
    args: &InfoArgs,
    owner: &str,
    name: &str,
    meta: &PackageVersion,
    kind: Option<&str>,
) {
    let id_label = meta.id.clone().unwrap_or_else(|| format!("{owner}/{name}"));
    println!("{}", ui::heading(&id_label));
    println!("  {} {}", ui::dim("version:    "), meta.version);
    if let Some(k) = kind {
        println!("  {} {}", ui::dim("kind:       "), k);
    }
    if let Some(sha) = &meta.sha256 {
        println!("  {} {}", ui::dim("sha256:     "), sha);
    }
    if let Some(bytes) = meta.size_bytes {
        println!(
            "  {} {} (gzipped tarball)",
            ui::dim("size:       "),
            human_bytes(bytes)
        );
    }
    if let Some(ts) = &meta.published_at {
        let line = relative_time(ts).map_or_else(|| ts.clone(), |rel| format!("{rel} ({ts})"));
        println!("  {} {}", ui::dim("published:  "), line);
    }
    if let Some(ts) = &meta.deprecated_at {
        let line = relative_time(ts).map_or_else(|| ts.clone(), |rel| format!("{rel} ({ts})"));
        println!("  {} {}", ui::dim("deprecated: "), line);
    }
    if let Some(url) = &meta.tarball_url {
        println!("  {} {}", ui::dim("tarball:    "), url);
    }
    if meta.tarball_url.is_some() {
        println!();
        println!(
            "  {}",
            ui::dim("note: tarball URL is signed and expires after 1 hour.")
        );
    }
    println!();
    // Install hint: include `-t <kind>` only when we resolved a
    // package kind from the registry. Hardcoding `-t skills` (the
    // pre-fix behaviour) shipped a wrong-kind command for any mcp /
    // hooks / commands / subagents / prompts package; omitting the
    // flag entirely lets `pakx add` re-probe the kind itself.
    if let Some(k) = kind {
        println!(
            "{} pakx add {}/{}@{} -t {}",
            ui::dim("\u{2192} install:"),
            owner,
            name,
            meta.version,
            k,
        );
    } else {
        println!(
            "{} pakx add {}/{}@{}",
            ui::dim("\u{2192} install:"),
            owner,
            name,
            meta.version,
        );
    }
    // `args` is reserved for the future render of `args.registry` —
    // currently the registry-base is already implicit in the id label,
    // but keeping the param shape stable avoids a churn-churn churn if
    // we decide to surface it on this view too.
    let _ = args;
}

fn split_id(id: &str) -> Result<(String, String)> {
    let Some((owner, name)) = id.split_once('/') else {
        return Err(anyhow!(
            "expected `<owner>/<name>` (e.g. acme/cool-skill), got {id:?}"
        ));
    };
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(anyhow!(
            "expected `<owner>/<name>` (e.g. acme/cool-skill), got {id:?}"
        ));
    }
    Ok((owner.to_string(), name.to_string()))
}

#[allow(clippy::cast_precision_loss)]
fn human_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if n >= MB {
        format!("{:.1} MiB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KiB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

/// Best-effort "N <unit> ago" formatter for an RFC3339 / ISO-8601
/// timestamp. Returns `None` when the input cannot be parsed (so the
/// caller can fall back to the raw timestamp) or when the timestamp is
/// in the future (clock skew — show the absolute value instead).
///
/// Implemented without `chrono` / `time` to keep the dep tree lean.
/// Accepts the subset `YYYY-MM-DDTHH:MM:SS[.fff]Z` — every timestamp
/// the registry mints today (`new Date().toISOString()` on the backend
/// produces exactly that). Unknown timezone forms fall through to the
/// raw display.
fn relative_time(iso: &str) -> Option<String> {
    let then = parse_iso8601_utc_seconds(iso)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let now_i64 = i64::try_from(now).ok()?;
    let delta = now_i64.checked_sub(then)?;
    if delta < 0 {
        // Future timestamp (clock skew) — let the caller fall back to
        // the raw ISO so we don't lie about "3 seconds ago" on what is
        // actually a future date.
        return None;
    }
    Some(format_delta_secs(delta))
}

/// Format a non-negative second-delta as a coarse-grained "N <unit>
/// ago" string. Buckets are deliberately wide — this is a header-line
/// readability hint, not a precise time-since display.
fn format_delta_secs(delta: i64) -> String {
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    if delta < MIN {
        "just now".to_string()
    } else if delta < HOUR {
        let n = delta / MIN;
        format!("{n} {} ago", plural(n, "minute"))
    } else if delta < DAY {
        let n = delta / HOUR;
        format!("{n} {} ago", plural(n, "hour"))
    } else if delta < WEEK {
        let n = delta / DAY;
        format!("{n} {} ago", plural(n, "day"))
    } else if delta < MONTH {
        let n = delta / WEEK;
        format!("{n} {} ago", plural(n, "week"))
    } else if delta < YEAR {
        let n = delta / MONTH;
        format!("{n} {} ago", plural(n, "month"))
    } else {
        let n = delta / YEAR;
        format!("{n} {} ago", plural(n, "year"))
    }
}

fn plural(n: i64, singular: &str) -> String {
    if n == 1 {
        singular.to_string()
    } else {
        format!("{singular}s")
    }
}

/// Parse the `YYYY-MM-DDTHH:MM:SS[.fff]Z` subset of ISO-8601 into a
/// Unix epoch second count. Returns `None` on any deviation from that
/// shape — the caller falls back to the raw input.
#[allow(clippy::cast_possible_wrap)]
fn parse_iso8601_utc_seconds(iso: &str) -> Option<i64> {
    // Strip trailing `Z` (UTC marker) — anything else (offset like
    // `+00:00`) we treat as unparseable and fall through to raw.
    let rest = iso.strip_suffix('Z')?;
    // Drop fractional seconds if present — the registry emits them
    // (`.123`), our second-granularity bucketing doesn't need them.
    let main = rest.split_once('.').map_or(rest, |(a, _)| a);
    let (date, time) = main.split_once('T')?;

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }

    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() {
        return None;
    }

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    Some(
        days_from_civil(year, month, day) * 86_400
            + i64::from(hour) * 3_600
            + i64::from(minute) * 60
            + i64::from(second),
    )
}

/// Howard Hinnant's `days_from_civil` algorithm — convert a proleptic
/// Gregorian (Y, M, D) into a day-count since 1970-01-01. Stable,
/// well-known, allocation-free. Avoids pulling in `chrono` just for
/// the per-version published-at relative display.
///
/// See <https://howardhinnant.github.io/date_algorithms.html#days_from_civil>.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    // Howard Hinnant's algorithm requires the year-of-era to be a
    // non-negative integer in `[0, 399]`. The era arithmetic above
    // guarantees that range, so the `as u64` cast is safe — clippy
    // can't statically see the invariant, hence the targeted allow.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let m_u = u64::from(m);
    let d_u = u64::from(d);
    let doy = (153 * (if m_u > 2 { m_u - 3 } else { m_u + 9 }) + 2) / 5 + d_u - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_id_accepts_canonical_form() {
        assert_eq!(
            split_id("acme/cool").unwrap(),
            ("acme".to_owned(), "cool".to_owned()),
        );
    }

    #[test]
    fn split_id_rejects_missing_slash() {
        assert!(split_id("no-slash").is_err());
    }

    #[test]
    fn split_id_rejects_empty_halves() {
        assert!(split_id("/right").is_err());
        assert!(split_id("left/").is_err());
    }

    #[test]
    fn split_id_rejects_too_many_slashes() {
        assert!(split_id("a/b/c").is_err());
    }

    #[test]
    fn human_bytes_picks_right_unit() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn parses_well_known_iso8601_z() {
        // 2026-05-22T00:00:00Z == days_from_civil(2026,5,22) * 86400
        let secs = parse_iso8601_utc_seconds("2026-05-22T00:00:00Z").unwrap();
        assert_eq!(secs, days_from_civil(2026, 5, 22) * 86_400);
    }

    #[test]
    fn parses_iso8601_with_fractional_seconds() {
        // Backend emits `.fff` — must not break parsing.
        let secs = parse_iso8601_utc_seconds("2026-05-22T08:06:19.123Z").unwrap();
        assert_eq!(
            secs,
            days_from_civil(2026, 5, 22) * 86_400 + 8 * 3600 + 6 * 60 + 19
        );
    }

    #[test]
    fn rejects_non_utc_timezones() {
        // We only handle the `Z` form — anything else falls through to
        // the raw render. That's intentional: we'd rather show the raw
        // ISO than mis-attribute the offset.
        assert!(parse_iso8601_utc_seconds("2026-05-22T00:00:00+00:00").is_none());
        assert!(parse_iso8601_utc_seconds("2026-05-22T00:00:00").is_none());
    }

    #[test]
    fn format_delta_picks_right_bucket() {
        assert_eq!(format_delta_secs(0), "just now");
        assert_eq!(format_delta_secs(59), "just now");
        assert_eq!(format_delta_secs(60), "1 minute ago");
        assert_eq!(format_delta_secs(120), "2 minutes ago");
        assert_eq!(format_delta_secs(3_600), "1 hour ago");
        assert_eq!(format_delta_secs(86_400), "1 day ago");
        assert_eq!(format_delta_secs(2 * 86_400), "2 days ago");
        assert_eq!(format_delta_secs(7 * 86_400), "1 week ago");
        assert_eq!(format_delta_secs(31 * 86_400), "1 month ago");
        assert_eq!(format_delta_secs(366 * 86_400), "1 year ago");
    }

    #[test]
    fn days_from_civil_matches_known_epoch() {
        // 1970-01-01 == 0 days since epoch.
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        // 1970-01-02 == 1 day.
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        // 2000-01-01 == 10957 days (a constant well-pinned by the
        // POSIX cal — Hinnant's algorithm reproduces it).
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
    }
}
