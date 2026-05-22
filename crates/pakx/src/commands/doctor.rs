//! `pakx doctor` — health check for the local project + agent install state.
//!
//! Reports on:
//!  * Manifest presence + parse errors.
//!  * Lockfile presence, version, and `manifestHash` drift.
//!  * Detected agents vs the `agents:` whitelist in `agents.yml`.
//!  * Lockfile entries vs `Adapter::list()` output (drift detection).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use pakx_agents::{
    Adapter, ClaudeCodeAdapter, CodexAdapter, CopilotAdapter, CursorAdapter, WindsurfAdapter,
};
use pakx_core::{
    compute_integrity, read_lockfile_from, read_manifest_from, Lockfile, Manifest, SkillFile,
};

use crate::ui;

const MANIFEST_FILENAME: &str = "agents.yml";
const LOCKFILE_FILENAME: &str = "agents.lock";

#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Override Claude Code home dir (testing).
    #[arg(long, hide = true)]
    pub claude_home: Option<PathBuf>,
}

#[derive(Default)]
struct Tally {
    problems: usize,
}

impl Tally {
    // Glyphs are padded to a uniform visible width of 6 + 2 trailing
    // spaces, so messages line up regardless of glyph length once the
    // ANSI escapes round-trip through a terminal.
    //
    //   [ok]   = 4 chars + 2 pad = 6, then 2 spaces => 8 visible
    //   [drift]= 7 chars + 0 pad           (overflows, but rare)
    //   [fail] = 6 chars + 0 pad
    //   [warn] = 6 chars + 0 pad
    //   ----   = 4 chars + 2 pad = 6, then 2 spaces => 8 visible
    fn ok(msg: &str) {
        println!("  {}    {msg}", ui::glyph_ok());
    }
    fn info(msg: &str) {
        println!("  {}    {msg}", ui::glyph_info());
    }
    fn warn(&mut self, msg: &str) {
        println!("  {}  {msg}", ui::glyph_warn());
        self.problems += 1;
    }
    fn fail(&mut self, msg: &str) {
        println!("  {}  {msg}", ui::glyph_fail());
        self.problems += 1;
    }
}

pub async fn run(args: DoctorArgs) -> Result<()> {
    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };
    println!(
        "{} {} ({})",
        ui::heading("pakx:"),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
    );
    println!("{} {}", ui::heading("project:"), project_root.display());

    let mut t = Tally::default();
    let manifest = check_manifest(&project_root, &mut t);
    let lock = check_lockfile(&project_root, &mut t);
    check_drift(manifest.as_ref(), lock.as_ref(), &mut t);

    let claude = build_claude(args.claude_home.as_deref(), &project_root);
    let detected = check_adapters(&claude).await;
    check_agent_whitelist(manifest.as_ref(), &detected, &mut t);
    check_on_disk(lock.as_ref(), &claude, &mut t).await;

    if t.problems == 0 {
        println!("\n{}", ui::heading("all checks passed"));
        Ok(())
    } else {
        anyhow::bail!("{} issue(s) found", t.problems)
    }
}

fn check_manifest(project_root: &Path, t: &mut Tally) -> Option<Manifest> {
    let path = project_root.join(MANIFEST_FILENAME);
    match read_manifest_from(&path) {
        Ok(m) => {
            Tally::ok(&format!(
                "manifest {MANIFEST_FILENAME} parsed (name={}, version={})",
                m.name, m.version
            ));
            Some(m)
        }
        Err(e) => {
            t.fail(&format!("manifest {MANIFEST_FILENAME} failed: {e}"));
            None
        }
    }
}

fn check_lockfile(project_root: &Path, t: &mut Tally) -> Option<Lockfile> {
    let path = project_root.join(LOCKFILE_FILENAME);
    match read_lockfile_from(&path) {
        Ok(Some(l)) => {
            Tally::ok(&format!(
                "lockfile {LOCKFILE_FILENAME} parsed ({} entries, version {})",
                l.entries.len(),
                l.lockfile_version
            ));
            Some(l)
        }
        Ok(None) => {
            t.warn(&format!("no {LOCKFILE_FILENAME} (run `pakx install`)"));
            None
        }
        Err(e) => {
            t.fail(&format!("lockfile {LOCKFILE_FILENAME} failed: {e}"));
            None
        }
    }
}

fn check_drift(manifest: Option<&Manifest>, lock: Option<&Lockfile>, t: &mut Tally) {
    let Some(m) = manifest else { return };
    let Some(l) = lock else { return };

    let body = pakx_core::manifest::write_manifest(m);
    let computed = compute_integrity(&[SkillFile {
        relative_path: MANIFEST_FILENAME.into(),
        contents: body.into_bytes(),
    }]);
    if computed == l.manifest_hash {
        Tally::ok("manifest hash matches lockfile");
    } else {
        t.warn("manifest drift: lockfile pinned to a different manifest — re-run `pakx install`");
    }

    if let Some(deps) = &m.dependencies.mcp {
        for dep in deps {
            let id = dep.display_hint();
            if l.entries.values().any(|e| e.name == *id) {
                Tally::ok(&format!("mcp/{id} pinned"));
            } else {
                t.warn(&format!("mcp/{id} not in lockfile — run `pakx install`"));
            }
        }
    }
}

async fn check_adapters(claude: &ClaudeCodeAdapter) -> Vec<(&'static str, bool)> {
    let detected: Vec<(&'static str, bool)> = vec![
        (ClaudeCodeAdapter::ID, claude.detect().await),
        (
            CursorAdapter::ID,
            detect_or_false(CursorAdapter::new()).await,
        ),
        (CodexAdapter::ID, detect_or_false(CodexAdapter::new()).await),
        (
            CopilotAdapter::ID,
            detect_or_false(CopilotAdapter::new()).await,
        ),
        (
            WindsurfAdapter::ID,
            detect_or_false(WindsurfAdapter::new()).await,
        ),
    ];
    for (id, present) in &detected {
        if *present {
            Tally::ok(&format!("adapter {id} detected"));
        } else {
            Tally::info(&format!("adapter {id} not detected"));
        }
    }
    detected
}

fn check_agent_whitelist(
    manifest: Option<&Manifest>,
    detected: &[(&'static str, bool)],
    t: &mut Tally,
) {
    let Some(m) = manifest else { return };
    let Some(whitelist) = &m.agents else { return };
    for id in whitelist {
        let is_installed = detected
            .iter()
            .any(|(x, present)| *x == id.as_str() && *present);
        if !is_installed {
            t.warn(&format!(
                "manifest declares agent {:?} but it is not installed",
                id.as_str()
            ));
        }
    }
}

async fn check_on_disk(lock: Option<&Lockfile>, claude: &ClaudeCodeAdapter, t: &mut Tally) {
    let Some(l) = lock else { return };
    let Ok(installed) = claude.list().await else {
        return;
    };
    for (key, entry) in &l.entries {
        let present = installed.iter().any(|i| matches_entry(i, entry));
        if present {
            Tally::ok(&format!("on-disk: {key}"));
        } else if matches!(entry.kind, pakx_core::manifest::PackageType::Skills) {
            t.warn(&format!("on-disk: {key} missing — adapter has no record"));
        }
        // MCP entries live in .mcp.json (not enumerated by list()), so
        // absence here is not a drift signal for them.
    }
}

#[allow(clippy::suspicious_operation_groupings)]
fn matches_entry(installed: &pakx_agents::Installed, entry: &pakx_core::LockEntry) -> bool {
    // installed.id and entry.name both hold the canonical `<owner>/<name>`;
    // clippy flags the differently-named fields as a possible bug, but
    // the comparison is intentional.
    installed.id == entry.name && installed.version == entry.version
}

async fn detect_or_false<A: Adapter>(adapter: Option<A>) -> bool {
    match adapter {
        Some(a) => a.detect().await,
        None => false,
    }
}

fn build_claude(home_override: Option<&Path>, project_root: &Path) -> ClaudeCodeAdapter {
    let home = home_override
        .map(Path::to_path_buf)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
        .unwrap_or_else(|| project_root.join(".claude"));
    ClaudeCodeAdapter::with_config_dir(home).with_project_root(project_root)
}
