//! `pakx search <query>` — federated search across all registered sources.
//!
//! Default output is one row per hit. With `--json`, the same hits are
//! emitted as a single-line JSON array on stdout (newline-terminated).
//! Field names are stable for downstream pipelines.

use anyhow::{Context, Result};
use clap::Args;
use comfy_table::Cell;
use pakx_core::http_client;
use pakx_core::manifest::{PackageType, PACKAGE_TYPES};
use pakx_registry_client::{
    OfficialMcpSource, PakxSource, RegistryClient, SmitherySource, OFFICIAL_MCP_BASE_URL,
    PAKX_BASE_URL, SMITHERY_BASE_URL,
};
use serde::Serialize;

use crate::commands::cache_tempdir::{cache_dir_at, make_cache_tempdir};
use crate::registry_url::validate_base_url;
use crate::ui;

#[derive(Debug, Clone, Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flags are independent toggles; a state machine here would obscure the surface
pub struct SearchArgs {
    /// Free-text query. Empty string returns the first page.
    pub query: Option<String>,

    /// Maximum results to display. Must be >= 1 — `-n 0` would
    /// silently return an empty list, which is never what a user
    /// actually wants from a search command. Clap enforces the floor
    /// via `value_parser` so the error fires at parse time with a
    /// clean diagnostic instead of producing an empty result page.
    #[arg(short = 'n', long, default_value_t = 20, value_parser = parse_limit)]
    pub limit: usize,

    /// Filter results to a single package kind.
    ///
    /// Accepts the same kind tokens as the rest of the CLI
    /// (`skills` / `mcp` / `subagents` / `prompts` / `commands` /
    /// `hooks`). For the pakx-registry source the kind is forwarded as
    /// `?kind=<kind>` so the registry does the filtering server-side;
    /// federated sources with no kind concept (Smithery, the official MCP
    /// Registry) list MCP servers exclusively, so `--kind mcp` keeps them
    /// and any other kind drops them. Composes with the search query.
    #[arg(long, value_parser = parse_kind)]
    pub kind: Option<PackageType>,

    /// Emit machine-readable JSON on stdout (single line, newline-terminated).
    /// Field names are a stable contract for downstream pipelines.
    #[arg(long)]
    pub json: bool,

    /// Override the official MCP Registry base URL (testing).
    #[arg(long, hide = true)]
    pub mcp_base_url: Option<String>,

    /// Override the Smithery registry base URL (testing).
    #[arg(long, hide = true)]
    pub smithery_base_url: Option<String>,

    /// Override the pakx-registry base URL (testing).
    #[arg(long, hide = true)]
    pub pakx_base_url: Option<String>,

    /// Skip Smithery search even if a base URL is available.
    #[arg(long)]
    pub no_smithery: bool,

    /// Skip the pakx-registry source.
    ///
    /// Renamed in 2026-05 from `--no-pakx` so the flag matches the
    /// pre-existing `--no-pakx-registry` on `pakx install` and `pakx
    /// test`. `--no-pakx` is kept as a hidden alias for one release
    /// (slated for removal in v0.2) so scripts continue to work
    /// while users migrate.
    #[arg(long, alias = "no-pakx")]
    pub no_pakx_registry: bool,

    /// Skip the official MCP Registry source.
    ///
    /// Symmetric counterpart to `--no-smithery` / `--no-pakx-registry`
    /// — toggles the third federated source so a user investigating
    /// only first-party (`pakx`) hits can suppress the noise from
    /// the public MCP Registry without resorting to a `--mcp-base-url`
    /// pointing at a sink.
    #[arg(long)]
    pub no_official_mcp: bool,

    /// Bypass the federated-source cache for this invocation. Drops
    /// the per-call cache TTL to zero so a cached search response is
    /// ignored and the source is re-queried. Useful when an upstream
    /// has just published a package the cache hasn't yet expired.
    #[arg(long)]
    pub no_cache: bool,
}

/// Wire-format hit emitted by `--json`. Field names are a stable
/// contract — only additive changes are backwards-compatible.
///
/// `description` is **always present** in the JSON output, even when
/// the upstream registry returned no description (emitted as `""`).
/// Skipping the field on `None` while emitting `""` on `Some("")` made
/// `jq '.description'` brittle for downstream pipelines; treat both
/// cases as the empty string so the field shape is invariant.
#[derive(Debug, Serialize)]
struct JsonHit<'a> {
    id: &'a str,
    name: &'a str,
    version: &'a str,
    source: &'static str,
    /// Declared package kind. **Additive field** — `null` for federated
    /// sources that carry no kind discriminator (Smithery, official MCP
    /// Registry), the raw registry-declared kind string for pakx hits
    /// (e.g. `"skill"`). Every pre-existing key (`id` / `name` /
    /// `version` / `source` / `description`) keeps its shape.
    kind: Option<&'a str>,
    /// Empty string when upstream has no description.
    description: &'a str,
}

pub async fn run(args: SearchArgs) -> Result<()> {
    if args.json {
        // Force stdout to no-color before any paint helper memoises a
        // stream decision — `pakx search --color always --json | jq`
        // must yield byte-clean JSON. Stderr remains color-able.
        crate::ui::force_stdout_no_color();
    }
    let (client, _cache_guard) = build_client(
        args.mcp_base_url.as_deref(),
        args.smithery_base_url.as_deref(),
        args.pakx_base_url.as_deref(),
        args.no_smithery,
        args.no_pakx_registry,
        args.no_official_mcp,
        args.no_cache,
    )?;
    let query = args.query.unwrap_or_default();
    let kind_tag = args.kind.map(PackageType::as_str);
    let outcome = client.search_kind_reporting(&query, kind_tag).await;
    let results = &outcome.packages;

    // Surface a degraded run. When every source erred (all 500 / DNS /
    // rate-limited) the merged result is empty and — without this hint
    // — `pakx search` prints "no results" and exits 0, which reads as a
    // genuinely-empty registry rather than a transport failure. Warn on
    // stderr (never stdout, so it can't pollute `--json | jq`) whenever
    // at least one source failed, so a partial result is also flagged.
    if outcome.failed > 0 {
        eprintln!(
            "{} {} of {} search source(s) failed (transient registry / network error) — results may be incomplete",
            ui::glyph_warn_err(),
            outcome.failed,
            outcome.total,
        );
    }

    let truncated: Vec<_> = results.iter().take(args.limit).collect();

    if args.json {
        let hits: Vec<JsonHit<'_>> = truncated
            .iter()
            .map(|pkg| JsonHit {
                id: pkg.id.as_str(),
                name: pkg.name.as_str(),
                version: pkg.version.as_str(),
                source: pkg.source.as_tag(),
                kind: pkg.kind.as_deref(),
                description: pkg.description.as_deref().unwrap_or(""),
            })
            .collect();
        let line = serde_json::to_string(&hits).context("serialize search hits as json")?;
        println!("{line}");
        return Ok(());
    }

    if results.is_empty() {
        eprintln!("no results for {query:?}");
        return Ok(());
    }

    let mut table = ui::table();
    table.set_header(vec![
        Cell::new("source"),
        Cell::new("kind"),
        Cell::new("name"),
        Cell::new("version"),
        Cell::new("description"),
    ]);
    for pkg in &truncated {
        let desc = pkg.description.as_deref().unwrap_or("");
        // Federated sources carry no kind discriminator → render a dash
        // rather than an empty cell so the column reads cleanly.
        let kind = pkg.kind.as_deref().unwrap_or("-");
        table.add_row(vec![
            Cell::new(pkg.source.as_tag()),
            Cell::new(kind),
            Cell::new(truncate(&pkg.name, 50)),
            Cell::new(pkg.version.as_str()),
            Cell::new(truncate(desc, 60)),
        ]);
    }
    println!("{table}");
    if results.len() > args.limit {
        eprintln!(
            "{}",
            ui::dim_err(&format!(
                "... {} more (raise -n to show)",
                results.len() - args.limit
            ))
        );
    }
    Ok(())
}

#[allow(clippy::fn_params_excessive_bools)] // each bool maps 1:1 to a documented CLI flag; folding into an enum would obscure the surface
fn build_client(
    mcp_base: Option<&str>,
    smithery_base: Option<&str>,
    pakx_base: Option<&str>,
    no_smithery: bool,
    no_pakx_registry: bool,
    no_official_mcp: bool,
    no_cache: bool,
) -> Result<(RegistryClient, tempfile::TempDir)> {
    // Per-call cache root so parallel integration tests can't share
    // cache entries when their `wiremock` mock servers happen to land
    // on the same loopback port (Linux releases ports aggressively).
    // See the matching note on `outdated::build_clients`.
    //
    // Returned alongside the `RegistryClient` so the caller can keep
    // the tempdir alive for the duration of `client.search(...).await`
    // — drop is when the tempdir self-deletes, otherwise the
    // `pakx-search-cache-*` dir would accumulate in `/tmp` on every
    // invocation.
    let cache_root =
        make_cache_tempdir("pakx-search-cache").context("create search cache tempdir")?;
    // Vet every user-supplied base URL BEFORE the federated search
    // fires. Mirrors `pakx outdated::build_clients` — even though the
    // search payload is anonymous, a userinfo-smuggled override would
    // hand a network observer the query string the user typed (which
    // commonly carries the package name they were about to install).
    let mut client = RegistryClient::new();
    if !no_official_mcp {
        let mcp_url = match mcp_base {
            Some(u) => {
                validate_base_url(u)?;
                u
            }
            None => OFFICIAL_MCP_BASE_URL,
        };
        let mcp = OfficialMcpSource::with_parts(
            http_client(),
            mcp_url,
            cache_dir_at(cache_root.path(), no_cache),
        );
        client = client.with_source(Box::new(mcp));
    }
    if !no_smithery {
        let smithery_url = match smithery_base {
            Some(u) => {
                validate_base_url(u)?;
                u
            }
            None => SMITHERY_BASE_URL,
        };
        let sm = SmitherySource::with_parts(
            http_client(),
            smithery_url,
            cache_dir_at(cache_root.path(), no_cache),
        );
        client = client.with_source(Box::new(sm));
    }
    if !no_pakx_registry {
        let pakx_url = match pakx_base {
            Some(u) => {
                validate_base_url(u)?;
                u
            }
            None => PAKX_BASE_URL,
        };
        let pakx = PakxSource::with_parts(
            http_client(),
            pakx_url,
            cache_dir_at(cache_root.path(), no_cache),
        );
        client = client.with_source(Box::new(pakx));
    }
    Ok((client, cache_root))
}

/// Clap value parser for `--limit`. Rejects `0` (which would silently
/// produce an empty result page) and any non-numeric value via the
/// `usize` parse step. Pulled out as a named function so the `#[arg(...)]`
/// attribute stays readable and the rejection message is reused in
/// tests.
fn parse_limit(s: &str) -> Result<usize, String> {
    let n: usize = s
        .parse()
        .map_err(|e| format!("--limit must be a non-negative integer: {e}"))?;
    if n == 0 {
        return Err("--limit must be >= 1 (use a larger value to see results)".to_owned());
    }
    Ok(n)
}

/// Clap value parser for `--kind`. Accepts exactly the canonical plural
/// kind tokens (`skills` / `mcp` / ...) so the accepted set is identical
/// to `pakx add <kind> <id>` and the rest of the CLI. Pulled from
/// [`PACKAGE_TYPES`] so a future kind addition flows through here without
/// a second edit.
fn parse_kind(s: &str) -> Result<PackageType, String> {
    PACKAGE_TYPES
        .iter()
        .copied()
        .find(|t| t.as_str() == s)
        .ok_or_else(|| {
            let allowed: Vec<&str> = PACKAGE_TYPES.iter().map(|t| t.as_str()).collect();
            format!("invalid kind {s:?}: expected one of {}", allowed.join("|"))
        })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
