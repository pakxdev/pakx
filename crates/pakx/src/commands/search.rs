//! `pakx search <query>` — federated search across all registered sources.
//!
//! Default output is one row per hit. With `--json`, the same hits are
//! emitted as a single-line JSON array on stdout (newline-terminated).
//! Field names are stable for downstream pipelines.

use anyhow::{Context, Result};
use clap::Args;
use comfy_table::Cell;
use pakx_core::http_client;
use pakx_registry_client::{
    CacheDir, OfficialMcpSource, PakxSource, RegistryClient, SmitherySource, OFFICIAL_MCP_BASE_URL,
    PAKX_BASE_URL, SMITHERY_BASE_URL,
};
use serde::Serialize;

use crate::registry_url::validate_base_url;
use crate::ui;

#[derive(Debug, Clone, Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flags are independent toggles; a state machine here would obscure the surface
pub struct SearchArgs {
    /// Free-text query. Empty string returns the first page.
    pub query: Option<String>,

    /// Maximum results to display.
    #[arg(short = 'n', long, default_value_t = 20)]
    pub limit: usize,

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
    let client = build_client(
        args.mcp_base_url.as_deref(),
        args.smithery_base_url.as_deref(),
        args.pakx_base_url.as_deref(),
        args.no_smithery,
        args.no_pakx_registry,
        args.no_official_mcp,
    )?;
    let query = args.query.unwrap_or_default();
    let results = client.search(&query).await;

    let truncated: Vec<_> = results.iter().take(args.limit).collect();

    if args.json {
        let hits: Vec<JsonHit<'_>> = truncated
            .iter()
            .map(|pkg| JsonHit {
                id: pkg.id.as_str(),
                name: pkg.name.as_str(),
                version: pkg.version.as_str(),
                source: pkg.source.as_tag(),
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
        Cell::new("name"),
        Cell::new("version"),
        Cell::new("description"),
    ]);
    for pkg in &truncated {
        let desc = pkg.description.as_deref().unwrap_or("");
        table.add_row(vec![
            Cell::new(pkg.source.as_tag()),
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

fn build_client(
    mcp_base: Option<&str>,
    smithery_base: Option<&str>,
    pakx_base: Option<&str>,
    no_smithery: bool,
    no_pakx_registry: bool,
    no_official_mcp: bool,
) -> Result<RegistryClient> {
    // Per-call cache root so parallel integration tests can't share
    // cache entries when their `wiremock` mock servers happen to land
    // on the same loopback port (Linux releases ports aggressively).
    // See the matching note on `outdated::build_clients`.
    let cache_root = std::env::temp_dir().join(format!(
        "pakx-search-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
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
        let mcp =
            OfficialMcpSource::with_parts(http_client(), mcp_url, CacheDir::with_root(&cache_root));
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
            CacheDir::with_root(&cache_root),
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
        let pakx =
            PakxSource::with_parts(http_client(), pakx_url, CacheDir::with_root(&cache_root));
        client = client.with_source(Box::new(pakx));
    }
    Ok(client)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
