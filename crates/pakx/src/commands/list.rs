//! `pakx list` — show what's pinned in the lockfile.
//!
//! Output is one row per lockfile entry. Optional cross-check against the
//! Claude Code adapter flags entries that pakx pinned but that the agent
//! no longer has installed on disk.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use pakx_agents::{Adapter, ClaudeCodeAdapter};
use pakx_core::read_lockfile_from;

const LOCKFILE_FILENAME: &str = "agents.lock";

#[derive(Debug, Clone, Args)]
pub struct ListArgs {
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Skip the adapter-side reconciliation step (faster, lockfile-only).
    #[arg(long)]
    pub no_check: bool,

    /// Override Claude Code home dir (testing).
    #[arg(long, hide = true)]
    pub claude_home: Option<PathBuf>,
}

pub async fn run(args: ListArgs) -> Result<()> {
    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };
    let lockfile_path = project_root.join(LOCKFILE_FILENAME);
    let lock = read_lockfile_from(&lockfile_path)
        .with_context(|| format!("read lockfile {}", lockfile_path.display()))?;

    let Some(lock) = lock else {
        eprintln!(
            "no {} found in {} — run `pakx install` first",
            LOCKFILE_FILENAME,
            project_root.display()
        );
        return Ok(());
    };

    if lock.entries.is_empty() {
        eprintln!("lockfile has no entries");
        return Ok(());
    }

    let claude = build_claude(args.claude_home.as_deref(), &project_root);
    let on_disk = if args.no_check {
        None
    } else {
        claude.list().await.ok()
    };

    for (key, entry) in &lock.entries {
        let status = on_disk.as_ref().map_or("", |list| {
            if list.iter().any(|i| matches_entry(i, entry)) {
                "[ok]"
            } else {
                "[drift]"
            }
        });
        println!(
            "{status:7} {kind:9} {name} @ {version}  ({key})",
            status = status,
            kind = entry.kind.as_str(),
            name = entry.name,
            version = entry.version,
            key = key,
        );
    }

    Ok(())
}

fn build_claude(
    home_override: Option<&std::path::Path>,
    project_root: &std::path::Path,
) -> ClaudeCodeAdapter {
    let home = home_override
        .map(std::path::Path::to_path_buf)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
        .unwrap_or_else(|| project_root.join(".claude"));
    ClaudeCodeAdapter::with_config_dir(home).with_project_root(project_root)
}

#[allow(clippy::suspicious_operation_groupings)]
fn matches_entry(installed: &pakx_agents::Installed, entry: &pakx_core::LockEntry) -> bool {
    // installed.id and entry.name both hold canonical `<owner>/<name>`;
    // differently-named fields are intentional, not a copy-paste bug.
    installed.id == entry.name && installed.version == entry.version
}
