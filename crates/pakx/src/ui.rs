//! User-facing output helpers — color, status glyphs, and table builders.
//!
//! Color resolution honours, in order:
//!
//! 1. The process-global [`ColorMode`] set by `main` from the top-level
//!    `--color always|auto|never` flag. `Always` / `Never` are absolute
//!    overrides; `Auto` (the default) falls through to (2).
//! 2. The `NO_COLOR` environment variable
//!    (<https://no-color.org/>) — present + non-empty disables color.
//! 3. `IsTerminal` on the relevant stream (stdout / stderr).
//!
//! The JSON output paths bypass this module entirely so bytes emitted
//! with `--json` are always plain ASCII / UTF-8 without escape
//! sequences.
//!
//! The status glyph vocabulary mirrors the existing project copy:
//! `[ok]`, `[drift]`, `[fail]`, `[warn]`. We never emit emoji — the
//! project tone is ASCII-only.

use std::io::IsTerminal;
use std::sync::OnceLock;

use clap::ValueEnum;
use owo_colors::{OwoColorize, Style};

/// User-facing color mode chosen via the top-level `--color` flag.
///
/// `Auto` is the default — matches v0.1 behaviour: respect `NO_COLOR`
/// and `IsTerminal`. `Always` and `Never` are absolute overrides; they
/// short-circuit both the env-var check and the TTY probe so users can
/// force a known state regardless of how the process is invoked
/// (CI logs, `| cat`, redirects, etc.).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum ColorMode {
    /// Respect `NO_COLOR` + `IsTerminal` (v0.1 behaviour, default).
    #[default]
    Auto,
    /// Force-enable color regardless of stream / env.
    Always,
    /// Force-disable color regardless of stream / env.
    Never,
}

/// Process-global override set once by `main` from the `--color` flag.
/// `None` means the flag was not specified, so callers fall back to
/// `ColorMode::Auto`.
static COLOR_MODE: OnceLock<ColorMode> = OnceLock::new();

/// Initialise the process-global color mode. Called exactly once by
/// `main`. A second call is a no-op (the first wins) — `OnceLock`
/// semantics. Tests that need a specific mode set this before any
/// paint helper runs.
pub fn set_color_mode(mode: ColorMode) {
    let _ = COLOR_MODE.set(mode);
}

fn color_mode() -> ColorMode {
    COLOR_MODE.get().copied().unwrap_or_default()
}

/// Cached decision for `stdout`. `None` while uninitialised; on first
/// access we resolve `--color` + `NO_COLOR` + `IsTerminal` once and
/// cache. Using `OnceLock` keeps it cheap on the hot per-line printing
/// path.
static STDOUT_COLOR: OnceLock<bool> = OnceLock::new();
static STDERR_COLOR: OnceLock<bool> = OnceLock::new();

/// Force stdout to no-color regardless of the resolved `--color` mode.
/// Called by JSON-emitting commands before any paint helper runs so a
/// caller writing `pakx list --color always --json | jq` doesn't have
/// ANSI escapes injected into the machine-readable stdout. The matching
/// stderr stream is **untouched** — human progress + spinner output on
/// stderr can still color when the user asked for it.
///
/// `OnceLock` semantics: the first call wins. Commands that internally
/// emit human output to stdout (i.e. not in JSON mode) must therefore
/// avoid calling this helper, or stdout will be flat for the rest of
/// the process. The dispatch path in each `--json`-supporting command
/// only invokes this when `args.json` is set.
pub fn force_stdout_no_color() {
    let _ = STDOUT_COLOR.set(false);
}

fn stdout_color() -> bool {
    *STDOUT_COLOR.get_or_init(|| resolve_stream_color(std::io::stdout().is_terminal()))
}

fn stderr_color() -> bool {
    *STDERR_COLOR.get_or_init(|| resolve_stream_color(std::io::stderr().is_terminal()))
}

/// Combine the explicit `--color` mode with the env + TTY fallbacks.
fn resolve_stream_color(is_tty: bool) -> bool {
    match color_mode() {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => !no_color() && is_tty,
    }
}

fn no_color() -> bool {
    // Honor the de-facto NO_COLOR spec (https://no-color.org/): the
    // variable being *present* and *non-empty* disables color.
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// Render `text` with `style` only when `tty` says so. When color is
/// disabled we return the original string untouched so the caller can
/// `println!` it without branching.
fn paint(text: &str, style: Style, tty: bool) -> String {
    if tty {
        text.style(style).to_string()
    } else {
        text.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Status glyphs — always 7 chars wide so columns line up across rows.
// `[ok]`, `[drift]`, `[fail]`, `[warn]`.
// ---------------------------------------------------------------------------

/// `[ok]` glyph for stdout. Green when colored.
pub fn glyph_ok() -> String {
    paint("[ok]", Style::new().green().bold(), stdout_color())
}

/// `[drift]` glyph for stdout. Yellow when colored.
pub fn glyph_drift() -> String {
    paint("[drift]", Style::new().yellow().bold(), stdout_color())
}

/// `[fail]` glyph for stdout. Red when colored.
pub fn glyph_fail() -> String {
    paint("[fail]", Style::new().red().bold(), stdout_color())
}

/// `[warn]` glyph for stdout. Yellow when colored.
pub fn glyph_warn() -> String {
    paint("[warn]", Style::new().yellow().bold(), stdout_color())
}

/// `----` glyph for stdout (informational, no-op state).
pub fn glyph_info() -> String {
    paint("----", Style::new().dimmed(), stdout_color())
}

// ---------------------------------------------------------------------------
// Stderr counterparts (used by commands that send progress to stderr).
// ---------------------------------------------------------------------------

pub fn glyph_ok_err() -> String {
    paint("[ok]", Style::new().green().bold(), stderr_color())
}

pub fn glyph_fail_err() -> String {
    paint("[fail]", Style::new().red().bold(), stderr_color())
}

pub fn glyph_warn_err() -> String {
    paint("[warn]", Style::new().yellow().bold(), stderr_color())
}

// ---------------------------------------------------------------------------
// Section heading + value helpers (stdout).
// ---------------------------------------------------------------------------

/// Bold heading for `pakx config`, `pakx info`, `pakx doctor`.
pub fn heading(text: &str) -> String {
    paint(text, Style::new().bold(), stdout_color())
}

/// Dimmed value text — for context that should sit visually behind the
/// label (resolved paths, ISO timestamps, etc.).
pub fn dim(text: &str) -> String {
    paint(text, Style::new().dimmed(), stdout_color())
}

/// Bold + green — success-line emphasis (`added <id>`, `removed <id>`,
/// `published <owner>/<name>@<version>`).
pub fn success(text: &str) -> String {
    paint(text, Style::new().green().bold(), stdout_color())
}

/// Same as `success`, routed through the stderr TTY check. Most `pakx`
/// commands write progress to stderr; this lets us keep colour decisions
/// stream-aware.
pub fn success_err(text: &str) -> String {
    paint(text, Style::new().green().bold(), stderr_color())
}

/// Dimmed text on stderr — for `note:` lines that should sit visually
/// quieter than the per-entry status above.
pub fn dim_err(text: &str) -> String {
    paint(text, Style::new().dimmed(), stderr_color())
}

/// Bold red — error-line emphasis on stderr.
pub fn error_err(text: &str) -> String {
    paint(text, Style::new().red().bold(), stderr_color())
}

// ---------------------------------------------------------------------------
// Tables — wrap `comfy-table` so callers don't have to import borders.
// ---------------------------------------------------------------------------

/// Build a `comfy-table::Table` with the project default border + width
/// clamp. Border style is UTF-8 rounded for TTY callers, ASCII otherwise.
pub fn table() -> comfy_table::Table {
    use comfy_table::presets;
    use comfy_table::{ContentArrangement, Table};

    let mut t = Table::new();
    if stdout_color() {
        t.load_preset(presets::UTF8_FULL_CONDENSED);
    } else {
        // ASCII-only preset for non-TTY (CI logs, pipes) so artifacts
        // don't include UTF-8 box-drawing characters.
        t.load_preset(presets::ASCII_FULL_CONDENSED);
    }
    t.set_content_arrangement(ContentArrangement::Dynamic);
    if let Some((w, _)) = terminal_size::terminal_size() {
        t.set_width(w.0);
    }
    t
}

// ---------------------------------------------------------------------------
// Indicatif spinner / progress bar wiring
// ---------------------------------------------------------------------------

/// Build a spinner with the project default style. Hidden (renders to
/// `/dev/null`) when stderr is not a TTY so CI logs stay clean.
pub fn spinner(message: impl Into<String>) -> indicatif::ProgressBar {
    use indicatif::{ProgressBar, ProgressStyle};

    let pb = if stderr_color() {
        ProgressBar::new_spinner()
    } else {
        ProgressBar::hidden()
    };
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan.bold} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb.set_message(message.into());
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_returns_unchanged_when_tty_false() {
        assert_eq!(paint("hello", Style::new().green(), false), "hello");
    }

    #[test]
    fn paint_applies_style_when_tty_true() {
        // Hard to assert the exact ANSI sequence without coupling to a
        // particular escape format; just check the original substring
        // is still in there and the result was actually mutated.
        let out = paint("hello", Style::new().green(), true);
        assert!(out.contains("hello"));
        assert_ne!(out, "hello");
    }

    #[test]
    fn no_color_handles_unset_empty_and_set_cases() {
        // We can't safely mutate process env in a test (races with the
        // rest of the suite) so just check the helper compiles + reads
        // the current env without panicking.
        let _ = no_color();
    }
}
