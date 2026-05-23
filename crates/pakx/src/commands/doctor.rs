//! `pakx doctor` — health check for the local project + agent install state.
//!
//! Reports on:
//!  * Manifest presence + parse errors.
//!  * Lockfile presence, version, and `manifestHash` drift.
//!  * Detected agents vs the `agents:` whitelist in `agents.yml`.
//!  * Lockfile entries vs `Adapter::list()` output (drift detection).
//!
//! With `--json`, emits a single-line JSON object on stdout while all
//! human-readable lines route to stderr. Field names are a stable
//! contract — `pakx whoami --json` / `pakx list --json` follow the same
//! discipline so pipelines can `jq` the result without parsing prose.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use pakx_agents::{
    Adapter, ClaudeCodeAdapter, CodexAdapter, CopilotAdapter, CursorAdapter, WindsurfAdapter,
};
use pakx_core::{
    compute_integrity, read_lockfile_from, read_manifest_from, Lockfile, Manifest, SkillFile,
};
use serde::Serialize;

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

    /// Emit machine-readable JSON on stdout (single line, newline-terminated).
    /// All human-readable lines route to stderr instead of stdout, matching
    /// the `--json` discipline of `pakx list` / `pakx info` / `pakx whoami`.
    #[arg(long)]
    pub json: bool,

    /// Wipe the per-call federated-source cache directories that the
    /// read commands (`pakx search`, `pakx info`, `pakx outdated`,
    /// `pakx audit`, `pakx add`) seed under `std::env::temp_dir()` via
    /// their `pid + nanos`-keyed roots. Best-effort — failures to
    /// remove individual entries are reported on stderr without
    /// failing the doctor run. Use this when the install-cache
    /// persistent dir (`<temp>/pakx-install-cache/`) is also holding
    /// a stale entry the `--no-cache` flag alone wouldn't clear.
    #[arg(long)]
    pub clear_cache: bool,
}

/// Wire-format payload emitted by `--json`. Field names are a stable
/// contract — only additive changes (new optional check keys) are
/// backwards-compatible. `ok` is `false` when at least one entry in
/// `errors` is set; `warnings` are informational and never flip `ok`.
#[derive(Debug, Default, Serialize)]
struct JsonPayload {
    ok: bool,
    /// Per-check status keyed by a short stable id (e.g. `"manifest"`,
    /// `"lockfile"`). Order is alphabetical because `BTreeMap` keeps
    /// the wire format deterministic across runs.
    checks: BTreeMap<&'static str, CheckEntry>,
    /// Free-form, non-fatal advisory messages. Examples: a lockfile
    /// missing entirely (warn, not fail — fresh project before
    /// `pakx install` lands here).
    warnings: Vec<String>,
    /// Fatal diagnostics. Any non-empty `errors` array forces
    /// `ok: false` and exit code 1.
    errors: Vec<String>,
}

/// One row in the `checks` map. `ok` is the only required field; the
/// optional `path` / `version` / `count` / `detail` lets each check
/// surface its native datum without forcing a uniform shape.
#[derive(Debug, Default, Serialize)]
struct CheckEntry {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<usize>,
    /// One-line human-readable summary, included so pipelines that just
    /// want a status line per check can render without re-deriving it.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// Sink for diagnostics — collects both into a structured payload and
/// (in human mode) emits the existing glyph lines verbatim.
struct Report {
    json_mode: bool,
    payload: JsonPayload,
}

impl Report {
    fn new(json_mode: bool) -> Self {
        Self {
            json_mode,
            payload: JsonPayload {
                ok: true,
                ..JsonPayload::default()
            },
        }
    }

    /// Pass-through human log line. Stdout in human mode; suppressed in
    /// JSON mode (the structured payload covers it).
    fn say(&self, line: &str) {
        if !self.json_mode {
            println!("{line}");
        }
    }

    /// Stderr log line in JSON mode (so the human-readable trail is
    /// still available for interactive runs that piped `--json` into
    /// `jq` while watching the terminal). Routed to stdout otherwise.
    fn say_err(&self, line: &str) {
        if self.json_mode {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    fn ok_check(&mut self, id: &'static str, entry: CheckEntry, line: &str) {
        self.say_err(&format!("  {}    {line}", ui::glyph_ok()));
        self.payload.checks.insert(id, entry);
    }

    fn info_check(&mut self, id: &'static str, entry: CheckEntry, line: &str) {
        self.say_err(&format!("  {}    {line}", ui::glyph_info()));
        self.payload.checks.insert(id, entry);
    }

    fn warn_check(&mut self, id: &'static str, entry: CheckEntry, line: &str) {
        self.say_err(&format!("  {}  {line}", ui::glyph_warn()));
        self.payload.warnings.push(line.to_owned());
        self.payload.checks.insert(id, entry);
    }

    fn fail_check(&mut self, id: &'static str, entry: CheckEntry, line: &str) {
        self.say_err(&format!("  {}  {line}", ui::glyph_fail()));
        self.payload.errors.push(line.to_owned());
        self.payload.ok = false;
        self.payload.checks.insert(id, entry);
    }

    /// Per-entry status that does not own a stable check id of its own
    /// (e.g. one row per lockfile entry). Goes into warnings or errors
    /// according to severity, but not into the `checks` map.
    fn warn_loose(&mut self, line: &str) {
        self.say_err(&format!("  {}  {line}", ui::glyph_warn()));
        self.payload.warnings.push(line.to_owned());
    }
}

pub async fn run(args: DoctorArgs) -> Result<ExitCode> {
    if args.json {
        // `--json | jq` discipline — keep stdout byte-clean. Stderr
        // (the streaming per-check render below) still colors.
        ui::force_stdout_no_color();
    }
    if args.clear_cache {
        // Stderr-only so the JSON contract is unaffected. Per-call
        // dirs are best-effort cleanup; the persistent
        // `pakx-install-cache` root is also wiped because it's the one
        // location the `--no-cache` flag can't shake off (its TTL
        // applies to the read path only).
        let removed = clear_cache_roots(args.json);
        eprintln!(
            "{} cleared {} cache director{}",
            ui::glyph_ok_err(),
            removed,
            if removed == 1 { "y" } else { "ies" }
        );
    }
    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };
    let mut report = Report::new(args.json);
    report.say_err(&format!(
        "{} {} ({})",
        ui::heading("pakx:"),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
    ));
    report.say_err(&format!(
        "{} {}",
        ui::heading("project:"),
        project_root.display()
    ));

    let manifest = check_manifest(&project_root, &mut report);
    let lock = check_lockfile(&project_root, &mut report);
    check_drift(manifest.as_ref(), lock.as_ref(), &mut report);

    let claude = build_claude(args.claude_home.as_deref(), &project_root);
    let detected = check_adapters(&claude, &mut report).await;
    check_agent_whitelist(manifest.as_ref(), &detected, &mut report);
    check_on_disk(lock.as_ref(), &claude, &mut report).await;

    let problems = report.payload.errors.len() + report.payload.warnings.len();

    if report.json_mode {
        let line = serde_json::to_string(&report.payload).context("serialize doctor as json")?;
        println!("{line}");
        return Ok(if report.payload.errors.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }

    if problems == 0 {
        report.say(&format!("\n{}", ui::heading("all checks passed")));
        Ok(ExitCode::SUCCESS)
    } else {
        anyhow::bail!("{problems} issue(s) found")
    }
}

fn check_manifest(project_root: &Path, r: &mut Report) -> Option<Manifest> {
    let path = project_root.join(MANIFEST_FILENAME);
    let path_str = path.display().to_string();
    match read_manifest_from(&path) {
        Ok(m) => {
            let detail = format!(
                "manifest {MANIFEST_FILENAME} parsed (name={}, version={})",
                m.name, m.version
            );
            r.ok_check(
                "manifest",
                CheckEntry {
                    ok: true,
                    path: Some(path_str),
                    version: Some(m.version.clone()),
                    detail: Some(detail.clone()),
                    ..CheckEntry::default()
                },
                &detail,
            );
            Some(m)
        }
        Err(e) => {
            let detail = format!("manifest {MANIFEST_FILENAME} failed: {e}");
            r.fail_check(
                "manifest",
                CheckEntry {
                    ok: false,
                    path: Some(path_str),
                    detail: Some(detail.clone()),
                    ..CheckEntry::default()
                },
                &detail,
            );
            None
        }
    }
}

fn check_lockfile(project_root: &Path, r: &mut Report) -> Option<Lockfile> {
    let path = project_root.join(LOCKFILE_FILENAME);
    let path_str = path.display().to_string();
    match read_lockfile_from(&path) {
        Ok(Some(l)) => {
            let detail = format!(
                "lockfile {LOCKFILE_FILENAME} parsed ({} entries, version {})",
                l.entries.len(),
                l.lockfile_version
            );
            r.ok_check(
                "lockfile",
                CheckEntry {
                    ok: true,
                    path: Some(path_str),
                    count: Some(l.entries.len()),
                    detail: Some(detail.clone()),
                    ..CheckEntry::default()
                },
                &detail,
            );
            Some(l)
        }
        Ok(None) => {
            let detail = format!("no {LOCKFILE_FILENAME} (run `pakx install`)");
            r.warn_check(
                "lockfile",
                CheckEntry {
                    ok: false,
                    path: Some(path_str),
                    detail: Some(detail.clone()),
                    ..CheckEntry::default()
                },
                &detail,
            );
            None
        }
        Err(e) => {
            let detail = format!("lockfile {LOCKFILE_FILENAME} failed: {e}");
            r.fail_check(
                "lockfile",
                CheckEntry {
                    ok: false,
                    path: Some(path_str),
                    detail: Some(detail.clone()),
                    ..CheckEntry::default()
                },
                &detail,
            );
            None
        }
    }
}

fn check_drift(manifest: Option<&Manifest>, lock: Option<&Lockfile>, r: &mut Report) {
    let Some(m) = manifest else { return };
    let Some(l) = lock else { return };

    let body = pakx_core::manifest::write_manifest(m);
    let computed = compute_integrity(&[SkillFile {
        relative_path: MANIFEST_FILENAME.into(),
        contents: body.into_bytes(),
    }]);
    if computed == l.manifest_hash {
        r.ok_check(
            "manifest_hash",
            CheckEntry {
                ok: true,
                detail: Some("manifest hash matches lockfile".to_owned()),
                ..CheckEntry::default()
            },
            "manifest hash matches lockfile",
        );
    } else {
        let detail =
            "manifest drift: lockfile pinned to a different manifest — re-run `pakx install`";
        r.warn_check(
            "manifest_hash",
            CheckEntry {
                ok: false,
                detail: Some(detail.to_owned()),
                ..CheckEntry::default()
            },
            detail,
        );
    }

    if let Some(deps) = &m.dependencies.mcp {
        for dep in deps {
            let id = dep.display_hint();
            if l.entries.values().any(|e| e.name == *id) {
                r.say_err(&format!("  {}    mcp/{id} pinned", ui::glyph_ok()));
            } else {
                r.warn_loose(&format!("mcp/{id} not in lockfile — run `pakx install`"));
            }
        }
    }
}

async fn check_adapters(claude: &ClaudeCodeAdapter, r: &mut Report) -> Vec<(&'static str, bool)> {
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
    let detected_count = detected.iter().filter(|(_, p)| *p).count();
    for (id, present) in &detected {
        if *present {
            r.say_err(&format!("  {}    adapter {id} detected", ui::glyph_ok()));
        } else {
            r.say_err(&format!(
                "  {}    adapter {id} not detected",
                ui::glyph_info()
            ));
        }
    }
    let detail = format!("{detected_count} adapter(s) detected");
    r.info_check(
        "adapters",
        CheckEntry {
            ok: true,
            count: Some(detected_count),
            detail: Some(detail.clone()),
            ..CheckEntry::default()
        },
        &detail,
    );
    detected
}

fn check_agent_whitelist(
    manifest: Option<&Manifest>,
    detected: &[(&'static str, bool)],
    r: &mut Report,
) {
    let Some(m) = manifest else { return };
    let Some(whitelist) = &m.agents else { return };
    for id in whitelist {
        let is_installed = detected
            .iter()
            .any(|(x, present)| *x == id.as_str() && *present);
        if !is_installed {
            r.warn_loose(&format!(
                "manifest declares agent {:?} but it is not installed",
                id.as_str()
            ));
        }
    }
}

async fn check_on_disk(lock: Option<&Lockfile>, claude: &ClaudeCodeAdapter, r: &mut Report) {
    let Some(l) = lock else { return };
    let Ok(installed) = claude.list().await else {
        return;
    };
    for (key, entry) in &l.entries {
        let present = installed.iter().any(|i| matches_entry(i, entry));
        if present {
            r.say_err(&format!("  {}    on-disk: {key}", ui::glyph_ok()));
        } else if matches!(entry.kind, pakx_core::manifest::PackageType::Skills) {
            r.warn_loose(&format!("on-disk: {key} missing — adapter has no record"));
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

/// Remove every per-call cache directory `pakx` may have left under
/// `std::env::temp_dir()`. The read commands name their cache roots
/// with a `pakx-<verb>-cache-<pid>-<nanos>` pattern, so we walk the
/// temp dir and remove anything whose file name starts with one of
/// the known prefixes — plus the persistent `pakx-install-cache` root
/// that the installer reuses across calls.
///
/// Returns the count of directories successfully removed. Failures
/// are reported on stderr (under `json_mode = true` this is the only
/// way to surface them without breaking the JSON contract) but never
/// trip the exit code — a transient FS error here shouldn't break a
/// `pakx doctor --clear-cache` invocation.
fn clear_cache_roots(json_mode: bool) -> usize {
    const PREFIXES: &[&str] = &[
        "pakx-install-cache",
        "pakx-search-cache-",
        "pakx-outdated-cache-",
        "pakx-audit-cache-",
        "pakx-add-cache-",
        "pakx-add-probe-",
    ];
    let temp = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&temp) else {
        return 0;
    };
    let mut count = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(n) = name.to_str() else { continue };
        if !PREFIXES.iter().any(|p| n.starts_with(p)) {
            continue;
        }
        let path = entry.path();
        // Be cautious — only delete what we created (directories). A
        // file named the same way is suspicious enough to leave
        // alone rather than risk losing user data.
        if !path.is_dir() {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => count = count.saturating_add(1),
            Err(e) => {
                // Stderr-only so JSON mode's stdout stays intact.
                // `json_mode` doesn't suppress the warn — it's a
                // legitimate operational signal the user should see.
                let _ = json_mode;
                eprintln!(
                    "{} could not remove {}: {e}",
                    ui::glyph_warn_err(),
                    path.display()
                );
            }
        }
    }
    count
}

fn build_claude(home_override: Option<&Path>, project_root: &Path) -> ClaudeCodeAdapter {
    let home = home_override
        .map(Path::to_path_buf)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
        .unwrap_or_else(|| project_root.join(".claude"));
    ClaudeCodeAdapter::with_config_dir(home).with_project_root(project_root)
}
