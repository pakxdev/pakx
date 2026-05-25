//! Generic "bundle" install path for the non-skill / non-MCP package
//! kinds: `commands`, `subagents`, `prompts`, and `hooks`.
//!
//! Architecturally identical to the skill flow in
//! [`crate::install::skill`] — resolve through pakx-registry, download
//! the signed tarball, sha256-verify, then extract under the Claude
//! Code config tree — but the destination subdirectory is selected
//! per [`PackageType`]:
//!
//! | Kind         | Subdir under `<claude_home>`            |
//! |--------------|------------------------------------------|
//! | `Commands`   | `commands/<owner>-<name>/`              |
//! | `Subagents`  | `agents/<owner>-<name>/`                |
//! | `Prompts`    | `prompts/<owner>-<name>/`               |
//! | `Hooks`      | `hooks/<owner>-<name>/`                 |
//!
//! Subdirectory names mirror Claude Code's documented layout
//! (<https://code.claude.com/docs/en/skills>) — note `Subagents` maps
//! to `agents/` (NOT `subagents/`) to match the upstream Claude Code
//! convention. The `<owner>-<name>` leaf matches the skill installer's
//! precedent (a single flat dir per package; reinstalling a different
//! version overwrites in place).
//!
//! ## Validation
//!
//! Install-time validation here is *structural* only: the tarball must
//! download, sha256-verify, and extract without tripping the same
//! zip-slip / symlink / 50 MiB guards the skill path enforces. Per-kind
//! file-shape validation now happens at PACK time instead (see
//! `pack::validate_kind_bundle`) — Claude Code publicly specs
//! every kind (skills / sub-agents / slash-commands / hooks), so the
//! publisher gets the advisory before upload. Install stays best-effort
//! and each kind is logged with a `tracing::info!` so users / operators
//! see that structural-only install is what's happening here.

use std::path::Path;

use anyhow::Result;
use pakx_core::manifest::PackageType;
use pakx_core::Integrity;
use pakx_registry_client::PakxSource;
use tracing::{debug, info};

use super::skill::{
    canonical_url, download_capped, extract_tarball, parse_bundle_shorthand, resolve, verify_sha256,
};

/// Outcome of a bundle install: parallel to
/// [`crate::install::skill::ResolvedSkill`]. Carries everything the
/// runner needs to write a lockfile entry.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `owner`/`name`/`install_path` surfaced for downstream tooling parity.
pub struct ResolvedBundle {
    pub kind: PackageType,
    pub id: String,
    pub owner: String,
    pub name: String,
    pub version: String,
    pub integrity: Integrity,
    pub canonical_url: String,
    pub install_path: std::path::PathBuf,
}

/// Top-level subdirectory under `<claude_home>` for a given bundle
/// kind. Mapping is fixed by Claude Code's filesystem layout — see
/// the module-level table.
///
/// `Skills` + `Mcp` are intentionally absent: skills go through
/// [`crate::install::skill::install_skill_from_pakx`] (a dedicated
/// path with its own subdir constant) and MCP servers don't write a
/// per-package tree at all (they get merged into `.mcp.json`).
const fn subdir_for(kind: PackageType) -> Option<&'static str> {
    match kind {
        PackageType::Commands => Some("commands"),
        PackageType::Subagents => Some("agents"),
        PackageType::Prompts => Some("prompts"),
        PackageType::Hooks => Some("hooks"),
        // Not routed through this module — explicit `None` so a
        // future caller that wires `Skills`/`Mcp` here gets a loud
        // failure instead of silently picking a wrong subdir.
        PackageType::Skills | PackageType::Mcp => None,
    }
}

/// Best-effort validation note logged at install time. Keeps the
/// user / operator aware that we don't yet enforce a strict file
/// shape for these kinds.
///
/// (Pure logging; never errors. Per-kind shape validation can land
/// later without changing the install contract.)
fn log_validation_note(kind: PackageType, id: &str) {
    let note = match kind {
        PackageType::Commands => {
            // Claude Code's `commands/` directory follows the same
            // SKILL.md / frontmatter shape as `skills/`. We still
            // skip the strict check at v0 because not every publisher
            // follows the convention yet.
            "commands bundle: structural-only validation (SKILL.md not enforced at v0)"
        }
        PackageType::Subagents => {
            "subagent bundle: best-effort validation at v0 (no strict file-shape check)"
        }
        PackageType::Prompts => {
            "prompt bundle: best-effort validation at v0 (no strict file-shape check)"
        }
        PackageType::Hooks => {
            "hooks bundle: best-effort validation at v0 (any tarball contents accepted)"
        }
        PackageType::Skills | PackageType::Mcp => {
            // Unreachable in practice — `install_bundle_from_pakx`
            // rejects these kinds before reaching the logger.
            return;
        }
    };
    info!(target: "pakx::install::bundle", %id, kind = kind.as_str(), "{note}");
}

/// Install one bundle dep (commands / subagents / prompts / hooks)
/// into the Claude Code tree.
///
/// Mirrors [`crate::install::skill::install_skill_from_pakx`] step-by-
/// step; the only behavioural delta is the destination subdirectory
/// chosen by [`subdir_for`].
///
/// Errors out if called with [`PackageType::Skills`] or
/// [`PackageType::Mcp`] — those kinds have their own dedicated paths
/// and routing them through this module would silently mis-place the
/// install on disk.
pub async fn install_bundle_from_pakx(
    source: &PakxSource,
    http: &reqwest::Client,
    base_url: &str,
    claude_home: &Path,
    kind: PackageType,
    id: &str,
    requested_version: Option<&str>,
) -> Result<ResolvedBundle> {
    let subdir = subdir_for(kind).ok_or_else(|| {
        anyhow::anyhow!(
            "bundle installer refuses kind {kind:?}; use the dedicated installer instead",
        )
    })?;

    let (owner, name, _) = parse_bundle_shorthand(id)?;
    let canonical_id = format!("{owner}/{name}");

    debug!(
        target: "pakx::install::bundle",
        kind = kind.as_str(),
        id = %canonical_id,
        "resolving bundle dep"
    );

    log_validation_note(kind, &canonical_id);

    let resolution = resolve(source, &canonical_id, requested_version).await?;

    let mut tmp = download_capped(http, &resolution.tarball_url).await?;
    let integrity = verify_sha256(&mut tmp, &resolution.sha256_hex, &canonical_id)?;

    let dest = claude_home.join(subdir).join(format!("{owner}-{name}"));
    extract_tarball(&mut tmp, &dest, &canonical_id)?;

    Ok(ResolvedBundle {
        kind,
        id: canonical_id,
        owner: owner.clone(),
        name: name.clone(),
        version: resolution.version.clone(),
        integrity,
        canonical_url: canonical_url(base_url, &owner, &name, &resolution.version),
        install_path: dest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdir_maps_each_bundle_kind() {
        assert_eq!(subdir_for(PackageType::Commands), Some("commands"));
        // Subagents map to Claude Code's `agents/` (NOT `subagents/`)
        // — the underlying directory name is `agents/` per the
        // upstream layout. This test pins that mapping so a future
        // rename doesn't quietly drift.
        assert_eq!(subdir_for(PackageType::Subagents), Some("agents"));
        assert_eq!(subdir_for(PackageType::Prompts), Some("prompts"));
        assert_eq!(subdir_for(PackageType::Hooks), Some("hooks"));
    }

    #[test]
    fn subdir_refuses_skills_and_mcp() {
        // These two kinds have their own install routes; this
        // installer must never silently accept them.
        assert_eq!(subdir_for(PackageType::Skills), None);
        assert_eq!(subdir_for(PackageType::Mcp), None);
    }
}
