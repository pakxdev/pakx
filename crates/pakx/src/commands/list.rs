//! `pakx list` — show what's pinned in the lockfile.
//!
//! Output is one row per lockfile entry. Optional cross-check against the
//! Claude Code adapter flags entries that pakx pinned but that the agent
//! no longer has installed on disk.
//!
//! With `--json`, the same data is emitted as a single-line JSON array on
//! stdout (newline-terminated). Field names are stable — downstream
//! pipelines depend on them.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use comfy_table::{Cell, CellAlignment};
use pakx_agents::{Adapter, ClaudeCodeAdapter};
use pakx_core::{read_lockfile_from, LockEntry};
use serde::Serialize;

use crate::ui;

const LOCKFILE_FILENAME: &str = "agents.lock";

#[derive(Debug, Clone, Args)]
pub struct ListArgs {
    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Skip the adapter-side reconciliation step (faster, lockfile-only).
    #[arg(long)]
    pub no_check: bool,

    /// Emit machine-readable JSON on stdout (single line, newline-terminated).
    /// Field names are a stable contract for downstream pipelines.
    #[arg(long)]
    pub json: bool,

    /// Override Claude Code home dir (testing).
    #[arg(long, hide = true)]
    pub claude_home: Option<PathBuf>,
}

/// Wire-format entry emitted by `--json`. Field names are a stable
/// contract — only additive changes (new optional fields) are
/// backwards-compatible.
#[derive(Debug, Serialize)]
struct JsonEntry<'a> {
    /// Lockfile key (`<type>/<name>@<version>`).
    key: &'a str,
    id: &'a str,
    version: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    registry: &'static str,
    resolved_from: &'a str,
    integrity: &'a str,
    agents: Vec<&'a str>,
    /// `ok` | `drift` | `unknown` (when `--no-check` skips reconciliation).
    status: &'static str,
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
        if args.json {
            println!("[]");
        } else {
            eprintln!("no {LOCKFILE_FILENAME} found — run `pakx install` first");
        }
        return Ok(());
    };

    if lock.entries.is_empty() {
        if args.json {
            println!("[]");
        } else {
            eprintln!("lockfile has no entries");
        }
        return Ok(());
    }

    let claude = build_claude(args.claude_home.as_deref(), &project_root);
    let on_disk = if args.no_check {
        None
    } else {
        claude.list().await.ok()
    };

    let entries: Vec<(&String, &LockEntry, &'static str)> = lock
        .entries
        .iter()
        .map(|(key, entry)| {
            let status = on_disk.as_ref().map_or("unknown", |list| {
                if list.iter().any(|i| matches_entry(i, entry)) {
                    "ok"
                } else {
                    "drift"
                }
            });
            (key, entry, status)
        })
        .collect();

    if args.json {
        let json_entries: Vec<JsonEntry<'_>> = entries
            .iter()
            .map(|(key, entry, status)| JsonEntry {
                key: key.as_str(),
                id: entry.name.as_str(),
                version: entry.version.as_str(),
                kind: entry.kind.as_str(),
                registry: entry.registry.as_tag(),
                resolved_from: entry.resolved_from.as_str(),
                integrity: entry.integrity.as_str(),
                agents: entry
                    .agents
                    .iter()
                    .map(pakx_core::AgentId::as_str)
                    .collect(),
                status,
            })
            .collect();
        let line = serde_json::to_string(&json_entries).context("serialize list as json")?;
        println!("{line}");
        return Ok(());
    }

    let mut table = ui::table();
    table.set_header(vec![
        Cell::new("status"),
        Cell::new("id"),
        Cell::new("version").set_alignment(CellAlignment::Right),
        Cell::new("registry"),
        Cell::new("agents"),
    ]);
    for (_key, entry, status) in &entries {
        let badge = match *status {
            "ok" => ui::glyph_ok(),
            "drift" => ui::glyph_drift(),
            _ => ui::glyph_info(),
        };
        let agents = entry
            .agents
            .iter()
            .map(pakx_core::AgentId::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        table.add_row(vec![
            Cell::new(badge),
            Cell::new(entry.name.as_str()),
            Cell::new(entry.version.as_str()).set_alignment(CellAlignment::Right),
            Cell::new(entry.registry.as_tag()),
            Cell::new(agents),
        ]);
    }
    println!("{table}");

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
