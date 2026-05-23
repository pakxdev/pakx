//! `pakx remove <id>` — inverse of `pakx add`.
//!
//! Strips a single shorthand dep entry from `agents.yml`. Mirrors `add`'s
//! kind-inference and `--directory` ergonomics; refuses ambiguous ids
//! (present in multiple sections) unless the caller disambiguates with
//! `--kind`. Does **not** touch `agents.lock` or any adapter install
//! state — same symmetry as `pakx add` (the resolve / install loop
//! handles that on the next `pakx install` run).

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, ValueEnum};
use inquire::Confirm;
use pakx_core::manifest::{
    read_from, remove_shorthand, sections_containing, write_to, Manifest, PackageType,
    RemoveOutcome,
};

use crate::redact::{project_root_for, redact_path};
use crate::ui;

const MANIFEST_FILENAME: &str = "agents.yml";

/// CLI-facing copy of [`PackageType`] so clap can derive `ValueEnum`
/// without forcing the trait onto the core type. Matches the variant
/// set used by `pakx add --type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RemoveKind {
    Skills,
    Mcp,
    Subagents,
    Prompts,
    Commands,
    Hooks,
}

impl RemoveKind {
    const fn to_core(self) -> PackageType {
        match self {
            Self::Skills => PackageType::Skills,
            Self::Mcp => PackageType::Mcp,
            Self::Subagents => PackageType::Subagents,
            Self::Prompts => PackageType::Prompts,
            Self::Commands => PackageType::Commands,
            Self::Hooks => PackageType::Hooks,
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct RemoveArgs {
    /// Shorthand id to remove. Must match an existing manifest entry
    /// byte-for-byte (same equality rule as `pakx add`).
    pub id: String,

    /// Explicit kind. Required when `<id>` is present in multiple
    /// sections; optional otherwise.
    #[arg(short = 'k', long = "kind", value_enum)]
    pub kind: Option<RemoveKind>,

    /// Operate on a project at a path other than the cwd. Mirrors
    /// `pakx install --directory` / `pakx list --directory`.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Skip the confirmation prompt. Required for non-interactive
    /// (CI / scripted) use.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Operate on a manifest at a path other than `<dir>/agents.yml`.
    /// Hidden; intended for tests that need to point at an alternate
    /// file inside the same project root.
    #[arg(long, hide = true)]
    pub manifest: Option<PathBuf>,
}

#[allow(clippy::unused_async)] // matches every other `commands::*::run` signature
pub async fn run(args: RemoveArgs) -> Result<()> {
    let target = resolve_manifest_path(args.directory.as_deref(), args.manifest.as_deref())?;
    let project_root = project_root_for(&target);
    let mut manifest = read_from(&target).map_err(|e| {
        anyhow!(e).context(format!(
            "read manifest at {}",
            redact_path(&target, &project_root)
        ))
    })?;

    let kind = pick_kind(&manifest, &args.id, args.kind.map(RemoveKind::to_core))?;

    if !args.yes && !confirm(&args.id, kind)? {
        eprintln!("aborted; manifest unchanged");
        return Ok(());
    }

    match remove_shorthand(&mut manifest, kind, &args.id) {
        RemoveOutcome::Removed => {}
        RemoveOutcome::NotPresent => {
            // `pick_kind` already verified at least one section holds
            // the id, so reaching here means the spec is a non-shorthand
            // (git / registry-object) entry. Surface that explicitly so
            // the user knows the YAML form is the blocker, not a typo.
            bail!(
                "{}/{} is a git or registry-object spec; `pakx remove` only handles shorthand strings",
                kind.as_str(),
                args.id,
            );
        }
    }

    write_to(&target, &manifest)
        .with_context(|| format!("write {}", redact_path(&target, &project_root)))?;

    // Machine-readable success line on stdout; the human "→ next:"
    // hint that follows belongs on stderr so a script piping stdout
    // through `grep removed` doesn't see the dimmed hint line too.
    // Matches the convention `pakx add` now follows after the
    // 2026-05-23 stdout/stderr alignment.
    println!(
        "{} removed {} ({})",
        ui::glyph_ok(),
        ui::success(&args.id),
        kind.as_str(),
    );
    // Single dimmed next-step hint — mirrors `pakx add`. U+2192
    // RIGHTWARDS ARROW written as an escape to keep source ASCII.
    eprintln!("{}", ui::dim_err("\u{2192} next: pakx install"));
    Ok(())
}

fn resolve_manifest_path(directory: Option<&Path>, manifest: Option<&Path>) -> Result<PathBuf> {
    let root: PathBuf = match directory {
        Some(p) => p.to_path_buf(),
        None => env::current_dir().context("cannot read current working directory")?,
    };
    Ok(match manifest {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => root.join(p),
        None => root.join(MANIFEST_FILENAME),
    })
}

/// Decide which section to remove from. Priority:
///   1. Explicit `--kind` wins, but is rejected if the id isn't actually
///      present in that section (better to error than silently no-op).
///   2. Otherwise the id must be present in exactly one section.
///   3. Ambiguous (≥2 sections) or absent (0 sections) → error.
fn pick_kind(manifest: &Manifest, id: &str, explicit: Option<PackageType>) -> Result<PackageType> {
    let present = sections_containing(manifest, id);
    if let Some(kind) = explicit {
        if !present.contains(&kind) {
            bail!("no `{}` entry named `{id}` in agents.yml", kind.as_str());
        }
        return Ok(kind);
    }
    match present.as_slice() {
        [] => bail!("{id} not found in agents.yml"),
        [only] => Ok(*only),
        many => {
            let listed: Vec<&str> = many.iter().map(|k| k.as_str()).collect();
            bail!(
                "{id} is declared under multiple sections ({}); rerun with `--kind <{}>`",
                listed.join(", "),
                listed.join("|"),
            )
        }
    }
}

fn confirm(id: &str, kind: PackageType) -> Result<bool> {
    Confirm::new(&format!("Remove {} ({})?", id, kind.as_str()))
        .with_default(false)
        .prompt()
        .map_err(|e| anyhow!("prompt failed: {e}"))
}
