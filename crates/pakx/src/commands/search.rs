//! `pakx search <query>` — federated search across all registered sources.

use anyhow::Result;
use clap::Args;
use pakx_registry_client::{
    CacheDir, OfficialMcpSource, PakxSource, RegistryClient, SmitherySource, OFFICIAL_MCP_BASE_URL,
    PAKX_BASE_URL, SMITHERY_BASE_URL,
};
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
    #[arg(long)]
    pub no_pakx: bool,
}

pub async fn run(args: SearchArgs) -> Result<()> {
    let client = build_client(
        args.mcp_base_url.as_deref(),
        args.smithery_base_url.as_deref(),
        args.pakx_base_url.as_deref(),
        args.no_smithery,
        args.no_pakx,
    );
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

fn build_client(
    mcp_base: Option<&str>,
    smithery_base: Option<&str>,
    pakx_base: Option<&str>,
    no_smithery: bool,
    no_pakx: bool,
) -> RegistryClient {
    let cache_root = std::env::temp_dir().join("pakx-search-cache");
    let mcp_url = mcp_base.unwrap_or(OFFICIAL_MCP_BASE_URL);
    let mcp =
        OfficialMcpSource::with_parts(Client::new(), mcp_url, CacheDir::with_root(&cache_root));
    let mut client = RegistryClient::new().with_source(Box::new(mcp));
    if !no_smithery {
        let smithery_url = smithery_base.unwrap_or(SMITHERY_BASE_URL);
        let sm = SmitherySource::with_parts(
            Client::new(),
            smithery_url,
            CacheDir::with_root(&cache_root),
        );
        client = client.with_source(Box::new(sm));
    }
    if !no_pakx {
        let pakx_url = pakx_base.unwrap_or(PAKX_BASE_URL);
        let pakx =
            PakxSource::with_parts(Client::new(), pakx_url, CacheDir::with_root(&cache_root));
        client = client.with_source(Box::new(pakx));
    }
    client
}

const fn source_tag(s: pakx_core::RegistrySource) -> &'static str {
    match s {
        pakx_core::RegistrySource::OfficialMcp => "official-mcp",
        pakx_core::RegistrySource::Smithery => "smithery",
        pakx_core::RegistrySource::Glama => "glama",
        pakx_core::RegistrySource::Github => "github",
        pakx_core::RegistrySource::Git => "git",
        pakx_core::RegistrySource::Pakx => "pakx",
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
