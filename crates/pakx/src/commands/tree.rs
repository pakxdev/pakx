//! `pakx tree` — grouped lockfile view.
//!
//! Reads `agents.lock` and groups entries by `(kind, registry source)`
//! then renders a nested tree. Closes the parity gap with `cargo tree`
//! / `pnpm list --depth ...` — `pakx list` already exists for a flat
//! per-entry table; `tree` is the kind-then-source pivot of the same
//! data.
//!
//! Adapter status is encoded per-kind via the single source of truth
//! [`crate::install::ADAPTER_WIRED_KINDS`]: kinds in that constant get
//! tagged `wired`, everything else gets `skipped`. After the sub-
//! adapter installer round all six kinds are wired, but the `skipped`
//! branch stays in code so a hypothetical future kind that lands
//! without an install dispatch arm still renders honestly.
//!
//! Output:
//!   - human: ASCII tree, no empty-group headers, one row per entry.
//!   - `--json`: `{ "kinds": { "<kind>": { "<registry>": [Entry, ...] } } }`
//!     `Entry = { id, version, adapter }`. Stable contract; only
//!     additive changes are backwards-compatible.
//!
//! Exit code: always `0`. `tree` is purely informational.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use pakx_core::{read_lockfile_from, LockEntry, PackageType, RegistrySource, PACKAGE_TYPES};
use serde::Serialize;

use crate::install::ADAPTER_WIRED_KINDS;
use crate::ui;

const LOCKFILE_FILENAME: &str = "agents.lock";

/// Adapter wiring status. Reads from the single-source-of-truth
/// constant [`crate::install::ADAPTER_WIRED_KINDS`]. The wire strings
/// (`wired` / `skipped`) are part of the JSON contract — only
/// additive changes.
fn adapter_status(kind: PackageType) -> &'static str {
    if ADAPTER_WIRED_KINDS.contains(&kind) {
        "wired"
    } else {
        "skipped"
    }
}

#[derive(Debug, Clone, Args)]
pub struct TreeArgs {
    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Emit machine-readable JSON on stdout (single line,
    /// newline-terminated). Field names are a stable contract for
    /// downstream pipelines.
    #[arg(long)]
    pub json: bool,
}

/// Wire-format leaf emitted by `--json`. Field names are a stable
/// contract — only additive changes (new optional fields) are
/// backwards-compatible.
#[derive(Debug, Serialize)]
struct JsonLeaf<'a> {
    id: &'a str,
    version: &'a str,
    adapter: &'static str,
}

/// Top-level JSON shape: `{ kinds: { <kind>: { <registry>: [leaf, ...] } } }`.
#[derive(Debug, Serialize)]
struct JsonOutput<'a> {
    kinds: BTreeMap<&'static str, BTreeMap<&'static str, Vec<JsonLeaf<'a>>>>,
}

#[allow(clippy::unused_async)] // matches every other `commands::*::run` signature
pub async fn run(args: TreeArgs) -> Result<()> {
    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };
    let lockfile_path = project_root.join(LOCKFILE_FILENAME);
    let lock = read_lockfile_from(&lockfile_path)
        .with_context(|| format!("read lockfile {}", lockfile_path.display()))?;

    let Some(lock) = lock else {
        if args.json {
            println!("{{\"kinds\":{{}}}}");
        } else {
            eprintln!("no {LOCKFILE_FILENAME} found — run `pakx install` first");
        }
        return Ok(());
    };

    if lock.entries.is_empty() {
        if args.json {
            println!("{{\"kinds\":{{}}}}");
        } else {
            eprintln!("lockfile has no entries");
        }
        return Ok(());
    }

    // Group: kind -> registry -> [entries]. `BTreeMap` keeps the
    // output deterministic; the outer iteration also goes through
    // `PACKAGE_TYPES` so the canonical kind order matches every other
    // pakx surface (init / outdated / doctor).
    let mut grouped: BTreeMap<PackageType, BTreeMap<RegistrySource, Vec<&LockEntry>>> =
        BTreeMap::new();
    for entry in lock.entries.values() {
        grouped
            .entry(entry.kind)
            .or_default()
            .entry(entry.registry)
            .or_default()
            .push(entry);
    }

    if args.json {
        render_json(&grouped);
    } else {
        render_human(&grouped);
    }

    Ok(())
}

fn render_json(grouped: &BTreeMap<PackageType, BTreeMap<RegistrySource, Vec<&LockEntry>>>) {
    let mut kinds: BTreeMap<&'static str, BTreeMap<&'static str, Vec<JsonLeaf<'_>>>> =
        BTreeMap::new();
    for kind in PACKAGE_TYPES {
        let Some(by_registry) = grouped.get(&kind) else {
            continue;
        };
        let mut registries: BTreeMap<&'static str, Vec<JsonLeaf<'_>>> = BTreeMap::new();
        for (registry, entries) in by_registry {
            let leaves: Vec<JsonLeaf<'_>> = entries
                .iter()
                .map(|e| JsonLeaf {
                    id: e.name.as_str(),
                    version: e.version.as_str(),
                    adapter: adapter_status(e.kind),
                })
                .collect();
            registries.insert(registry.as_tag(), leaves);
        }
        kinds.insert(kind.as_str(), registries);
    }
    let out = JsonOutput { kinds };
    let line = serde_json::to_string(&out).expect("serialize tree as json");
    println!("{line}");
}

fn render_human(grouped: &BTreeMap<PackageType, BTreeMap<RegistrySource, Vec<&LockEntry>>>) {
    for kind in PACKAGE_TYPES {
        let Some(by_registry) = grouped.get(&kind) else {
            // Empty kind: skip silently — don't paint empty headers.
            continue;
        };
        if by_registry.is_empty() {
            continue;
        }
        println!("{}/", ui::heading(kind.as_str()));
        for (registry, entries) in by_registry {
            println!("  {}/", registry.as_tag());
            for entry in entries {
                let adapter = adapter_status(entry.kind);
                let adapter_label = if adapter == "wired" {
                    format!("{} adapter", entry.kind.as_str())
                } else {
                    format!("skipped — {} adapter not wired", entry.kind.as_str())
                };
                println!(
                    "    {}  {}  ({})",
                    entry.name,
                    entry.version,
                    ui::dim(&adapter_label),
                );
            }
        }
    }
}
