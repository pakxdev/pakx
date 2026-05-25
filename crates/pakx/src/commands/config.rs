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

use crate::ui;

#[derive(Debug, Clone, Args)]
pub struct ConfigArgs {
    /// Print JSON instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[allow(clippy::unused_async)]
pub async fn run(args: ConfigArgs) -> Result<()> {
    // Resolve as `Option` so the `--json` surface can emit `null` for an
    // unresolvable path rather than a literal `<unavailable: …>` string
    // that a `jq` consumer would have to substring-match to detect. The
    // human render still substitutes a readable placeholder below.
    let credentials_path: Option<String> = Credentials::default_path()
        .ok()
        .map(|p| p.display().to_string());
    let cache_dir: Option<String> =
        CacheDir::default_path().map(|c| c.root().display().to_string());

    if args.json {
        // Force stdout to no-color before any future paint helper
        // memoises a stream decision — `pakx config --color always
        // --json | jq` must yield byte-clean stdout. The other
        // `--json` subcommands (list, search, info, outdated, ...)
        // follow the same discipline.
        ui::force_stdout_no_color();
        // Single-line newline-terminated JSON to match every other
        // `pakx <cmd> --json` surface (list, search, outdated, info,
        // pack, publish, whoami). Previously this one command
        // emitted pretty-printed JSON, which broke `jq` pipelines
        // that assume one object per line.
        let raw = serde_json::to_string(&serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "platform": {
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
            },
            // `null` (not a `<unavailable>` string) when the path can't
            // be resolved — distinguishable in a `jq` pipeline.
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
        "{} {} ({} / {})",
        ui::heading("pakx"),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();
    println!("{}", ui::heading("paths:"));
    println!(
        "  credentials: {}",
        ui::dim(credentials_path.as_deref().unwrap_or("(unavailable)")),
    );
    println!(
        "  cache:       {}",
        ui::dim(
            cache_dir
                .as_deref()
                .unwrap_or("(unavailable on this platform)")
        ),
    );
    println!();
    println!("{}", ui::heading("registries:"));
    println!("  official-mcp: {}", ui::dim(OFFICIAL_MCP_BASE_URL));
    println!("  smithery:     {}", ui::dim(SMITHERY_BASE_URL));
    println!("  pakx:         {}", ui::dim(PAKX_BASE_URL));
    Ok(())
}
