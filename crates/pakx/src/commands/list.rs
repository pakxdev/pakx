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

#[allow(clippy::too_many_lines)] // linear branches; helpers would obscure shape
pub async fn run(args: ListArgs) -> Result<()> {
    if args.json {
        // Force stdout to no-color before any paint helper memoises a
        // decision — `pakx list --color always --json | jq` must yield
        // byte-clean stdout. Stderr remains color-able (the
        // empty-lockfile / no-entries hints there still color).
        ui::force_stdout_no_color();
    }
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
    // Reconcile against the on-disk adapter state. `None` means "no
    // reconciliation was performed" — either `--no-check` was passed OR
    // the adapter's `list()` itself errored. Previously a failed
    // `claude.list()` was swallowed via `.ok()` and silently rendered
    // every row as `unknown`, indistinguishable from `--no-check`. We
    // now warn on stderr when the check FAILED so the user knows the
    // `unknown` rows mean "couldn't verify", not "verified, no adapter".
    let on_disk = if args.no_check {
        None
    } else {
        match claude.list().await {
            Ok(list) => Some(list),
            Err(e) => {
                eprintln!(
                    "{} could not read installed state from the Claude adapter ({e}); \
                     drift column shows `unknown` (re-run without the failing adapter, \
                     or with --no-check to silence)",
                    ui::glyph_warn_err(),
                );
                None
            }
        }
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

    let table = build_table(&entries);
    println!("{table}");

    Ok(())
}

/// Build the human-readable `pakx list` table from the reconciled
/// entries.
///
/// The `status` cell holds a PRE-COLORED `owo_colors` string (e.g. the
/// green-bold `[ok]` badge) when stdout color is enabled. comfy-table
/// is built with the `custom_styling` feature so its column-width
/// measurement is ANSI-aware (`ansi_strip().width()`): the badge cell
/// is measured by its 4-visible-char width, not by the ~13 raw escaped
/// bytes. Without that feature the escape bytes inflate the `status`
/// column and the box-drawing borders no longer line up under
/// `--color always` / a real color terminal.
///
/// Extracted from [`run`] so the alignment invariant is unit-testable
/// with color forced on (see the `tests` module): a colored cell must
/// not widen the column relative to its visible content.
fn build_table(entries: &[(&String, &LockEntry, &'static str)]) -> comfy_table::Table {
    let mut table = ui::table();
    // `kind` sits between `id` and `version` so a reader scans
    // "what package, what type, what version" left-to-right. The
    // lockfile entry already carries the kind (`entry.kind.as_str()`);
    // `--json` exposes it as `type`. This human column is additive —
    // the JSON contract is unchanged.
    table.set_header(vec![
        Cell::new("status"),
        Cell::new("id"),
        Cell::new("kind"),
        Cell::new("version").set_alignment(CellAlignment::Right),
        Cell::new("registry"),
        Cell::new("agents"),
    ]);
    for (_key, entry, status) in entries {
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
            Cell::new(entry.kind.as_str()),
            Cell::new(entry.version.as_str()).set_alignment(CellAlignment::Right),
            Cell::new(entry.registry.as_tag()),
            Cell::new(agents),
        ]);
    }
    table
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

#[cfg(test)]
mod tests {
    use comfy_table::presets;
    use comfy_table::{Cell, CellAlignment, ContentArrangement, Table};
    use owo_colors::{OwoColorize, Style};

    /// Strip ANSI escape sequences (CSI `ESC [ ... m`) so we can measure
    /// the VISIBLE width of a rendered table line. Deliberately tiny and
    /// dependency-free — the badge cell only ever carries SGR color
    /// sequences (`\x1b[32m`, `\x1b[1m`, `\x1b[0m`).
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Consume `[ … <final byte>` of a CSI sequence. SGR
                // (color) sequences end in `m`; any final byte in the
                // 0x40..=0x7e range terminates a CSI sequence.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for inner in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&inner) {
                            break;
                        }
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Build a table mirroring the real `pakx list` layout, but with the
    /// status cell forced to a PRE-COLORED `owo_colors` string (the same
    /// shape `ui::glyph_ok()` produces under `--color always`). This
    /// isolates the width-measurement behaviour without touching the
    /// process-global color `OnceLock` that `ui::glyph_ok()` reads.
    fn colored_status_table() -> Table {
        let mut t = Table::new();
        t.load_preset(presets::UTF8_FULL_CONDENSED);
        t.set_content_arrangement(ContentArrangement::Dynamic);
        t.set_header(vec![
            Cell::new("status"),
            Cell::new("id"),
            Cell::new("kind"),
            Cell::new("version").set_alignment(CellAlignment::Right),
            Cell::new("registry"),
            Cell::new("agents"),
        ]);
        // `[ok]` painted green+bold — exactly what the badge cell holds
        // when stdout color is on. Raw byte length is ~13; visible width
        // is 4. Without comfy-table's `custom_styling` feature the column
        // is sized from the 13 escaped bytes and the borders misalign.
        let badge = "[ok]".style(Style::new().green().bold()).to_string();
        assert!(
            badge.len() > "[ok]".len(),
            "test precondition: badge must actually carry ANSI bytes"
        );
        t.add_row(vec![
            Cell::new(badge),
            Cell::new("arwenizEr/hello-world"),
            Cell::new("skills"),
            Cell::new("0.1.0").set_alignment(CellAlignment::Right),
            Cell::new("pakx"),
            Cell::new("claude-code"),
        ]);
        t
    }

    /// Regression for the colored-table misalignment bug: a pre-colored
    /// status cell must NOT inflate its column width. We assert every
    /// rendered line has the SAME visible width (ANSI stripped). Before
    /// enabling comfy-table's `custom_styling` feature this FAILS — the
    /// header/data border lines differ in visible width because the
    /// `status` column is sized from the escaped byte length of the
    /// badge while the badge renders as 4 visible chars.
    #[test]
    fn colored_status_cell_does_not_misalign_table() {
        let table = colored_status_table();
        let rendered = table.to_string();

        let widths: Vec<usize> = rendered
            .lines()
            .map(|line| strip_ansi(line).chars().count())
            .collect();
        assert!(widths.len() >= 4, "expected a bordered multi-line table");

        let first = widths[0];
        assert!(
            widths.iter().all(|&w| w == first),
            "table rows must all have equal visible width when a cell is \
             ANSI-colored; got per-line visible widths {widths:?} from:\n{rendered}"
        );
    }

    /// Pin the exact mechanism: the border-separator characters (`│` /
    /// `┆`) in the colored data row must sit at the same byte/char
    /// columns (after ANSI strip) as in the header row. This is the
    /// reader-visible symptom from the bug report — the `│`/`┆`
    /// separators not lining up.
    #[test]
    fn colored_status_cell_keeps_separators_column_aligned() {
        let table = colored_status_table();
        let rendered = table.to_string();
        let lines: Vec<String> = rendered.lines().map(strip_ansi).collect();

        // Header content row is the first line that starts with a `│`
        // vertical border; the colored data row is the next such line.
        let bordered: Vec<&String> = lines
            .iter()
            .filter(|l| l.trim_start().starts_with('│'))
            .collect();
        assert!(
            bordered.len() >= 2,
            "expected a header content row and a data row, got {bordered:?}"
        );

        let sep_cols = |line: &str| -> Vec<usize> {
            line.char_indices()
                .filter(|&(_, c)| c == '│' || c == '┆')
                .map(|(i, _)| i)
                .collect()
        };
        let header_seps = sep_cols(bordered[0]);
        let data_seps = sep_cols(bordered[1]);
        assert_eq!(
            header_seps, data_seps,
            "column separators must align between the header row and the \
             ANSI-colored data row; header={header_seps:?} data={data_seps:?}\n{rendered}"
        );
    }
}
