//! `pakx search <query>` — federated search across all registered sources.

use anyhow::Result;
use clap::Args;
use pakx_registry_client::{CacheDir, OfficialMcpSource, RegistryClient, OFFICIAL_MCP_BASE_URL};
use reqwest::Client;

#[derive(Debug, Clone, Args)]
pub struct SearchArgs {
    /// Free-text query. Empty string returns the first page.
    pub query: Option<String>,

    /// Maximum results to display.
    #[arg(short = 'n', long, default_value_t = 20)]
    pub limit: usize,

    /// Override the official MCP Registry base URL (testing).
    #[arg(long, hide = true)]
    pub mcp_base_url: Option<String>,
}

pub async fn run(args: SearchArgs) -> Result<()> {
    let client = build_client(args.mcp_base_url.as_deref());
    let query = args.query.unwrap_or_default();
    let results = client.search(&query).await;

    if results.is_empty() {
        eprintln!("no results for {query:?}");
        return Ok(());
    }

    for pkg in results.iter().take(args.limit) {
        let desc = pkg.description.as_deref().unwrap_or("");
        println!(
            "{source:14} {name:50} {version:10}  {desc}",
            source = source_tag(pkg.source),
            name = truncate(&pkg.name, 50),
            version = pkg.version,
            desc = truncate(desc, 60),
        );
    }
    if results.len() > args.limit {
        eprintln!("... {} more (raise -n to show)", results.len() - args.limit);
    }
    Ok(())
}

fn build_client(mcp_base: Option<&str>) -> RegistryClient {
    let base = mcp_base.unwrap_or(OFFICIAL_MCP_BASE_URL);
    let cache_root = std::env::temp_dir().join("pakx-search-cache");
    let cache = CacheDir::with_root(&cache_root);
    let source = OfficialMcpSource::with_parts(Client::new(), base, cache);
    RegistryClient::new().with_source(Box::new(source))
}

const fn source_tag(s: pakx_core::RegistrySource) -> &'static str {
    match s {
        pakx_core::RegistrySource::OfficialMcp => "official-mcp",
        pakx_core::RegistrySource::Smithery => "smithery",
        pakx_core::RegistrySource::Glama => "glama",
        pakx_core::RegistrySource::Github => "github",
        pakx_core::RegistrySource::Git => "git",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
