//! `pakx install` CLI surface.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::install::{
    run_with_progress, InstallOpts, InstallReportEntry, InstallStatus, MultiProgressSink,
};
use crate::ui;

#[derive(Debug, Clone, Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flags are independent toggles; a state machine here would obscure the surface
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

    /// Emit a machine-readable JSON array on stdout
    /// (`[{id, status, kind, version, error?}, ...]`).
    ///
    /// Human progress + summary continue to render on stderr — the
    /// JSON shape is the stable contract for downstream pipelines
    /// (`jq`, CI parsers, ...). Mirrors `pakx outdated --json` /
    /// `pakx audit --json`.
    #[arg(long)]
    pub json: bool,

    /// Bypass the federated-source cache for this invocation. Sets the
    /// per-call cache TTL to zero so any cached `versions[]` /
    /// `latestVersion` response is ignored and the registry is
    /// re-queried. Useful right after a publish when the cache may
    /// still hold a stale "no such version" response. Mirrored across
    /// `pakx search` / `pakx info` / `pakx outdated` / `pakx audit` /
    /// `pakx add`.
    #[arg(long)]
    pub no_cache: bool,

    /// Restore local state if any dependency fails (all-or-nothing).
    ///
    /// Snapshots every install target before writing, then reverts the
    /// whole run on any failure: dirs created by the run are removed and
    /// dirs that already existed are restored to their prior contents,
    /// leaving the filesystem as if the run never happened. Opt-in in
    /// this version; becoming the default is reserved for a future major
    /// release. Without this flag, a partial failure leaves the
    /// already-installed dependencies in place (the current behavior).
    #[arg(long)]
    pub rollback_on_error: bool,
}

pub async fn run_cmd(args: InstallArgs) -> Result<()> {
    if args.json {
        // Force stdout to no-color BEFORE any paint helper memoises a
        // stream decision. Keeps `pakx install --color always --json | jq`
        // byte-clean. Stderr remains color-able for the human render.
        ui::force_stdout_no_color();
    }
    let opts = InstallOpts {
        project_root: args.directory,
        mcp_base_url: args.mcp_base_url,
        smithery_base_url: args.smithery_base_url,
        pakx_base_url: args.pakx_base_url,
        no_smithery: args.no_smithery,
        no_pakx_registry: args.no_pakx_registry,
        claude_home: args.claude_home,
        no_lockfile: args.no_lockfile,
        no_cache: args.no_cache,
        rollback_on_error: args.rollback_on_error,
    };
    // Per-dependency progress: one `indicatif` bar per dep, each
    // advancing resolve -> install and settling to `[ok]`/`[skip]`/`[fail]`.
    // The sink renders to stderr only when stderr is an interactive
    // terminal we own (folds in `--color never` / `NO_COLOR`); otherwise
    // every bar is hidden, so CI logs, pipes, and `--json` stdout stay
    // byte-clean — identical to the prior single-spinner gate. We drop
    // the sink before printing the summary so any unfinished bar clears
    // and the finished per-dep trail sits above the summary block.
    let report = {
        let sink = MultiProgressSink::new(ui::stderr_progress_enabled());
        let report = run_with_progress(opts, &sink).await;
        drop(sink);
        report
    }?;

    // Empty manifest (or one whose only deps were all filtered out before
    // dispatch): nothing was processed. Give a friendly empty-state
    // pointing at `pakx add` instead of an unhelpful "installed 0,
    // skipped 0, failed 0" with no context. JSON callers still get their
    // (empty) array below, so this is human-render-only.
    if report.entries.is_empty() && !args.json {
        eprintln!(
            "{}",
            ui::dim_err("nothing to install — add deps with `pakx add <id>`")
        );
    }

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
            // A reason built from a `{e:#}` cause chain can span multiple
            // lines, which would break the aligned `failed:` block. Flatten
            // to a single line (`; `-joined) for the human enumeration; the
            // full chain is still available via `--json` / debug logs.
            eprintln!(
                "  {} {id}: {}",
                ui::glyph_fail_err(),
                flatten_reason(reason)
            );
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
    } else if !args.no_lockfile && !report.failed.is_empty() {
        // The runner gates the lockfile write on a zero-failure run, so a
        // partial / total failure leaves the prior `agents.lock` untouched.
        // Call it out so the user isn't surprised that the on-disk lockfile
        // may now lag `agents.yml`. Suppressed under `--no-lockfile` (no
        // write was ever going to happen).
        eprintln!(
            "{} agents.lock not updated (install had failures) — it may be out of date vs agents.yml",
            ui::glyph_warn_err()
        );
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

    if args.json {
        emit_install_json(&report.entries);
    }

    if report.failed.is_empty() {
        // Single dimmed hint pointing at the lockfile — users want
        // to see which file landed without scanning the project tree.
        // We show the absolute path here (not just the file name)
        // because the user's next action is typically `git add` and
        // they need to know the absolute location. Skipped when
        // `--no-lockfile` was passed (no lockfile was written).
        if let Some(p) = &report.lockfile_path {
            eprintln!(
                "{}",
                ui::dim_err(&format!("\u{2192} lockfile: {}", p.display()))
            );
        }
        Ok(())
    } else {
        anyhow::bail!("{} dep(s) failed to install", report.failed.len())
    }
}

/// Collapse a multi-line failure reason into one line so the aligned
/// `failed:` block stays readable. Internal whitespace runs (including
/// the newlines an anyhow `{e:#}` cause chain emits) become a single
/// `; ` separator. The full multi-line chain is preserved for the
/// `--json` payload (`error` field) and the `tracing` trail.
fn flatten_reason(reason: &str) -> String {
    reason
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Emit the `pakx install --json` payload: a single newline-terminated
/// array of `{id, status, kind, version, error?}` rows on stdout. The
/// field set mirrors `pakx outdated --json` discipline — `error` is
/// omitted (rather than `null`) on success rows so `jq '.[] | select(.error)'`
/// returns only failures without an `.error == null` filter.
fn emit_install_json(entries: &[InstallReportEntry]) {
    let rows: Vec<_> = entries
        .iter()
        .map(|e| {
            let mut obj = serde_json::json!({
                "id": e.id,
                "status": status_tag(e.status),
                "kind": e.kind.as_str(),
            });
            if let Some(v) = e.version.as_deref() {
                obj["version"] = serde_json::Value::String(v.to_owned());
            } else {
                obj["version"] = serde_json::Value::Null;
            }
            if let Some(err) = e.error.as_deref() {
                obj["error"] = serde_json::Value::String(err.to_owned());
            }
            obj
        })
        .collect();
    let line = serde_json::to_string(&rows).expect("serialize install report json");
    println!("{line}");
}

/// Stable wire tag for [`InstallStatus`]. Pulled out here so the
/// match arm sits next to the JSON shape that consumes it — keeps the
/// contract obvious when an additive variant lands later.
const fn status_tag(s: InstallStatus) -> &'static str {
    s.as_str()
}
