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

use crate::pack::{dry_run_dir, pack_dir};
use crate::ui;

#[derive(Debug, Clone, Args)]
pub struct PackArgs {
    /// Source directory containing `SKILL.md`. Defaults to cwd.
    pub source: Option<PathBuf>,

    /// Where to write the tarball. Defaults to cwd. The historical
    /// short form `-o` + long form `--out` are both still accepted;
    /// `--output` is the canonical long form so the flag matches the
    /// shape used by `pakx export --output` and the broader CLI
    /// convention.
    #[arg(short = 'o', long = "output", alias = "out")]
    pub out: Option<PathBuf>,

    /// Emit a single machine-readable JSON object on stdout describing
    /// the produced tarball. Warnings + progress lines still go to
    /// stderr — exit code stays 0 on success regardless of `warnings`.
    /// Field names are a stable contract for downstream pipelines.
    ///
    /// Composes with `--dry-run`: `pakx pack --dry-run --json` emits
    /// the would-be payload plus a `files: [{path, sizeBytes}]` array
    /// enumerating tarball entries WITHOUT writing the tarball.
    #[arg(long)]
    pub json: bool,

    /// Enumerate the tarball entries without writing the `.tgz`. Pairs
    /// with `--json` to surface the `files: [{path, sizeBytes}]`
    /// payload for downstream pipelines that want a content listing
    /// before committing to the network round-trip on `pakx publish`.
    /// Human mode still prints a short summary on stderr.
    #[arg(long)]
    pub dry_run: bool,
}

#[allow(clippy::unused_async)] // matches the other commands::*::run signatures
pub async fn run(args: PackArgs) -> Result<()> {
    let src = args.source.unwrap_or_else(|| PathBuf::from("."));

    if args.dry_run {
        return run_dry(&src, args.json);
    }

    let out_dir = args.out.unwrap_or_else(|| PathBuf::from("."));
    let result = pack_dir(&src, &out_dir)?;
    // Always stream warnings to stderr — both modes. `--json` consumers
    // that want to ignore warnings can just discard stderr; pipelines
    // that want to surface them have them there alongside other logs.
    for warning in &result.warnings {
        eprintln!("{} {warning}", ui::glyph_warn_err());
    }

    if args.json {
        // Force stdout to no-color BEFORE any paint helper memoises a
        // stream decision (the `[warn]` glyphs above already wrote to
        // stderr, which we leave colour-able). Keeps `pakx pack
        // --color always --json | jq` byte-clean.
        crate::ui::force_stdout_no_color();
        // Single newline-terminated JSON object on stdout — same shape
        // discipline as `pakx list --json` / `pakx outdated --json`.
        // `kind` mirrors whatever the SKILL.md frontmatter declared
        // (defaulting to `"skills"` when omitted), so a publisher who
        // packs a non-skills bundle no longer sees the misleading
        // hardcoded `"kind": "skills"` on the wire.
        let mut hasher = Sha256::new();
        hasher.update(&result.bytes);
        let sha256_hex = hex_lower(&hasher.finalize());
        let payload = serde_json::json!({
            "ok": true,
            "name": result.manifest.name,
            "version": result.manifest.version,
            "kind": result.manifest.kind,
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

/// `--dry-run` branch. Enumerates the would-be tarball entries from
/// `src` without compressing or writing the `.tgz`, and emits the
/// summary in human or JSON form.
///
/// JSON shape (additive to the regular `pack --json` contract):
///   `{ ok, name, version, kind, dryRun: true, files: [{path, sizeBytes}], warnings }`
///
/// The `dryRun: true` discriminator is what lets a pipeline tell apart
/// a real pack from a dry-run inspection without re-parsing the
/// command-line. Human mode prints a short summary on stderr; nothing
/// goes to stdout in non-JSON dry-run mode.
fn run_dry(src: &std::path::Path, json: bool) -> Result<()> {
    let out = dry_run_dir(src)?;

    for warning in &out.warnings {
        eprintln!("{} {warning}", ui::glyph_warn_err());
    }

    if json {
        crate::ui::force_stdout_no_color();
        let files: Vec<_> = out
            .entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "path": e.path,
                    "sizeBytes": e.size_bytes,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "ok": true,
            "name": out.manifest.name,
            "version": out.manifest.version,
            "kind": out.manifest.kind,
            "dryRun": true,
            "files": files,
            "warnings": out.warnings,
        });
        let line = serde_json::to_string(&payload).expect("serialize pack dry-run json");
        println!("{line}");
        return Ok(());
    }

    let total_bytes: u64 = out.entries.iter().map(|e| e.size_bytes).sum();
    eprintln!(
        "{} would pack {} ({} file{}, {} uncompressed bytes)",
        ui::glyph_ok_err(),
        ui::success_err(&format!("{}@{}", out.manifest.name, out.manifest.version)),
        out.entries.len(),
        if out.entries.len() == 1 { "" } else { "s" },
        total_bytes,
    );
    // Dimmed `→ next` hint so users see the natural follow-up command.
    eprintln!(
        "{}",
        ui::dim_err("\u{2192} next: pakx pack (without --dry-run)")
    );
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
