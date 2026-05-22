//! `pakx install` CLI surface.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::install::{run, InstallOpts};
use crate::ui;

#[derive(Debug, Clone, Args)]
pub struct InstallArgs {
    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Skip writing `agents.lock` (useful for ad-hoc diagnostic runs).
    #[arg(long)]
    pub no_lockfile: bool,

    /// Override the official MCP Registry base URL (testing).
    #[arg(long, hide = true)]
    pub mcp_base_url: Option<String>,

    /// Override the Smithery registry base URL (testing).
    ///
    /// Mutually exclusive with `--no-smithery`: opting out of a source
    /// while supplying a base URL for it is a contradiction. Clap
    /// errors on the contradiction so the user sees the mistake instead
    /// of silently dropping the override.
    #[arg(long, hide = true, conflicts_with = "no_smithery")]
    pub smithery_base_url: Option<String>,

    /// Override the pakx-registry base URL (testing).
    ///
    /// Mutually exclusive with `--no-pakx-registry` for the same reason
    /// as `--smithery-base-url` / `--no-smithery`.
    #[arg(long, hide = true, conflicts_with = "no_pakx_registry")]
    pub pakx_base_url: Option<String>,

    /// Skip Smithery resolution. Default: enabled.
    #[arg(long)]
    pub no_smithery: bool,

    /// Skip the pakx-registry source. Default: enabled.
    #[arg(long)]
    pub no_pakx_registry: bool,

    /// Override the Claude Code home directory (testing).
    #[arg(long, hide = true)]
    pub claude_home: Option<PathBuf>,
}

pub async fn run_cmd(args: InstallArgs) -> Result<()> {
    let opts = InstallOpts {
        project_root: args.directory,
        mcp_base_url: args.mcp_base_url,
        smithery_base_url: args.smithery_base_url,
        pakx_base_url: args.pakx_base_url,
        no_smithery: args.no_smithery,
        no_pakx_registry: args.no_pakx_registry,
        claude_home: args.claude_home,
        no_lockfile: args.no_lockfile,
    };
    let pb = ui::spinner("resolving dependencies");
    let report = run(opts).await;
    pb.finish_and_clear();
    let report = report?;

    if !report.installed.is_empty() {
        eprintln!("{}", ui::heading("installed:"));
        for id in &report.installed {
            eprintln!("  {} {}", ui::glyph_ok_err(), id);
        }
    }
    if !report.skipped.is_empty() {
        eprintln!(
            "{}",
            ui::heading("skipped (unchanged or not yet supported):")
        );
        for id in &report.skipped {
            eprintln!("  {} {}", ui::dim_err("~"), id);
        }
    }
    if !report.failed.is_empty() {
        eprintln!("{}", ui::heading("failed:"));
        for (id, reason) in &report.failed {
            eprintln!("  {} {id}: {reason}", ui::glyph_fail_err());
        }
    }
    if let Some(p) = &report.lockfile_path {
        // Print the file name rather than the absolute path so CI logs
        // / pasted snippets don't leak the host's temp / project dir.
        let label = p
            .file_name()
            .and_then(|n| n.to_str())
            .map_or_else(|| p.display().to_string(), str::to_owned);
        eprintln!("{} wrote {}", ui::glyph_ok_err(), label);
    }

    // Final summary so users can scan the outcome at a glance.
    let installed = report.installed.len();
    let skipped = report.skipped.len();
    let failed = report.failed.len();
    eprintln!(
        "\n{}: installed {}, skipped {}, failed {}",
        ui::heading("summary"),
        ui::success_err(&installed.to_string()),
        skipped,
        if failed > 0 {
            ui::error_err(&failed.to_string())
        } else {
            failed.to_string()
        },
    );

    if report.failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} dep(s) failed to install", report.failed.len())
    }
}
