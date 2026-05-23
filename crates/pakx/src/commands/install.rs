//! `pakx install` CLI surface.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::install::{run, InstallOpts, InstallReportEntry, InstallStatus};
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
