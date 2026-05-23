//! `pakx pack [<path>]` — build a gzipped tarball of a local skill bundle.
//!
//! Two output modes:
//!
//! - **Human (default).** Progress + warnings go to stderr with the
//!   project's `[ok]` / `[warn]` glyph cadence, and a final `→ next`
//!   hint nudges the user toward `pakx publish`. Nothing on stdout.
//! - **`--json`.** Progress + warnings still go to stderr (so a CI run
//!   can grep the human-readable warnings even when piping JSON), and
//!   stdout carries a **single** machine-readable object describing the
//!   produced tarball. Field names are a stable camelCase contract.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use sha2::{Digest, Sha256};

use crate::pack::pack_dir;
use crate::ui;

#[derive(Debug, Clone, Args)]
pub struct PackArgs {
    /// Source directory containing `SKILL.md`. Defaults to cwd.
    pub source: Option<PathBuf>,

    /// Where to write the tarball. Defaults to cwd.
    #[arg(short = 'o', long = "out")]
    pub out: Option<PathBuf>,

    /// Emit a single machine-readable JSON object on stdout describing
    /// the produced tarball. Warnings + progress lines still go to
    /// stderr — exit code stays 0 on success regardless of `warnings`.
    /// Field names are a stable contract for downstream pipelines.
    #[arg(long)]
    pub json: bool,
}

#[allow(clippy::unused_async)] // matches the other commands::*::run signatures
pub async fn run(args: PackArgs) -> Result<()> {
    let src = args.source.unwrap_or_else(|| PathBuf::from("."));
    let out_dir = args.out.unwrap_or_else(|| PathBuf::from("."));
    let result = pack_dir(&src, &out_dir)?;
    // Always stream warnings to stderr — both modes. `--json` consumers
    // that want to ignore warnings can just discard stderr; pipelines
    // that want to surface them have them there alongside other logs.
    for warning in &result.warnings {
        eprintln!("{} {warning}", ui::glyph_warn_err());
    }

    if args.json {
        // Single newline-terminated JSON object on stdout — same shape
        // discipline as `pakx list --json` / `pakx outdated --json`.
        // `kind` is hard-coded to "skills" because `pack_dir` only
        // operates on SKILL.md bundles today; bumping the JSON shape
        // when we ship a second pack target is forward-compatible.
        let mut hasher = Sha256::new();
        hasher.update(&result.bytes);
        let sha256_hex = hex_lower(&hasher.finalize());
        let payload = serde_json::json!({
            "ok": true,
            "name": result.manifest.name,
            "version": result.manifest.version,
            "kind": "skills",
            "sha256": sha256_hex,
            "sizeBytes": result.bytes.len(),
            "tarballPath": result.tarball_path.display().to_string(),
            "warnings": result.warnings,
        });
        let line = serde_json::to_string(&payload).expect("serialize pack json");
        println!("{line}");
        return Ok(());
    }

    eprintln!(
        "{} packed {} -> {} ({} bytes)",
        ui::glyph_ok_err(),
        ui::success_err(&format!(
            "{}@{}",
            result.manifest.name, result.manifest.version
        )),
        result.tarball_path.display(),
        result.bytes.len(),
    );
    // Single dimmed next-step hint.
    eprintln!("{}", ui::dim_err("\u{2192} next: pakx publish"));
    Ok(())
}

/// Lowercase-hex render of a 32-byte sha256 digest. The integrity
/// pipeline elsewhere in `pakx` uses SRI base64; the public JSON
/// contract here keeps the conventional hex form because that's what
/// downstream tooling (`shasum -a 256`, `sha256sum`) prints.
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}
