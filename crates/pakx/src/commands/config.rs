//! `pakx config` — inspect resolved CLI configuration.
//!
//! Read-only. Prints the paths + registry URLs pakx would use for
//! the current invocation, so adopters can sanity-check where
//! credentials live, where the federated-search cache writes, and
//! which registry the publish flow targets.

use anyhow::Result;
use clap::Args;
use pakx_core::Credentials;
use pakx_registry_client::{CacheDir, OFFICIAL_MCP_BASE_URL, PAKX_BASE_URL, SMITHERY_BASE_URL};

#[derive(Debug, Clone, Args)]
pub struct ConfigArgs {
    /// Print JSON instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[allow(clippy::unused_async)]
pub async fn run(args: ConfigArgs) -> Result<()> {
    let credentials_path = Credentials::default_path().map_or_else(
        |e| format!("<unavailable: {e}>"),
        |p| p.display().to_string(),
    );
    let cache_dir = CacheDir::default_path().map_or_else(
        || "<unavailable on this platform>".to_string(),
        |c| c.root().display().to_string(),
    );

    if args.json {
        let raw = serde_json::to_string_pretty(&serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "platform": {
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
            },
            "credentialsPath": credentials_path,
            "cacheDir": cache_dir,
            "registries": {
                "officialMcp": OFFICIAL_MCP_BASE_URL,
                "smithery": SMITHERY_BASE_URL,
                "pakx": PAKX_BASE_URL,
            },
        }))?;
        println!("{raw}");
        return Ok(());
    }

    println!(
        "pakx {} ({} / {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();
    println!("paths:");
    println!("  credentials: {credentials_path}");
    println!("  cache:       {cache_dir}");
    println!();
    println!("registries:");
    println!("  official-mcp: {OFFICIAL_MCP_BASE_URL}");
    println!("  smithery:     {SMITHERY_BASE_URL}");
    println!("  pakx:         {PAKX_BASE_URL}");
    Ok(())
}
