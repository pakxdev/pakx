//! `pakx install` CLI surface.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::install::{run, InstallOpts};

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

    /// Override the Claude Code home directory (testing).
    #[arg(long, hide = true)]
    pub claude_home: Option<PathBuf>,
}

pub async fn run_cmd(args: InstallArgs) -> Result<()> {
    let opts = InstallOpts {
        project_root: args.directory,
        mcp_base_url: args.mcp_base_url,
        claude_home: args.claude_home,
        no_lockfile: args.no_lockfile,
    };
    let report = run(opts).await?;

    if !report.installed.is_empty() {
        eprintln!("installed:");
        for id in &report.installed {
            eprintln!("  + {id}");
        }
    }
    if !report.skipped.is_empty() {
        eprintln!("skipped (unchanged or not yet supported):");
        for id in &report.skipped {
            eprintln!("  ~ {id}");
        }
    }
    if !report.failed.is_empty() {
        eprintln!("failed:");
        for (id, reason) in &report.failed {
            eprintln!("  ! {id}: {reason}");
        }
    }
    if let Some(p) = &report.lockfile_path {
        eprintln!("wrote {}", p.display());
    }

    if report.failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} dep(s) failed to install", report.failed.len())
    }
}
