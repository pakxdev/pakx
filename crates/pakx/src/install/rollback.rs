//! Filesystem snapshot + restore for `pakx install --rollback-on-error`.
//!
//! ## Why
//!
//! `pakx install` installs every dep in `agents.yml` in dispatch order
//! (`mcp` → `skills` → `subagents` → `prompts` → `commands` → `hooks`).
//! Each adapter write is independent, so when dep #4 of 6 fails, the
//! first three are already on disk. The default behaviour leaves that
//! partial state in place (the lockfile is gated on zero failures, but
//! the extracted trees remain). For a user who wanted an all-or-nothing
//! install that half-state is a mess to clean up by hand across
//! `~/.claude/{skills,agents,commands,prompts,hooks}` plus the
//! `.mcp.json` merge target.
//!
//! ## What
//!
//! When `--rollback-on-error` is set the runner takes a [`Snapshot`]
//! *before* any adapter write, recording the prior on-disk state of
//! every target the run is about to touch:
//!
//!   * per-id bundle / skill trees — `<claude_home>/<subdir>/<owner>-<name>/`
//!     for each `skills` / `subagents` / `prompts` / `commands` / `hooks`
//!     dep, where `<subdir>` is the kind's Claude Code directory (skills
//!     → `skills`, subagents → `agents`, etc.);
//!   * the project-scoped `.mcp.json` file, which the MCP installer
//!     merges new server entries into.
//!
//! For each target we record whether it pre-existed and, if so, move its
//! prior contents into a sibling temp backup directory (a `rename`, which
//! is atomic within a filesystem). On a failed run we [`restore`] every
//! target: delete the ones that did not pre-exist and move the backups
//! back over the ones that did. On a clean run we [`commit`] (drop the
//! backup dir). The net effect of a rolled-back run is that the
//! filesystem looks exactly as it did before the run started.
//!
//! ## Opt-in
//!
//! Rollback is opt-in at this version (`--rollback-on-error`). Flipping
//! the default to on is reserved for a future major bump — see the flag
//! help text in `commands::install`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pakx_core::manifest::{DepSpec, Manifest, PackageType};
use tracing::{debug, warn};

use super::runner::ADAPTER_WIRED_KINDS;
use super::skill::parse_skill_shorthand;

/// Claude Code subdirectory under `<claude_home>` for a per-id package
/// tree of `kind`. Mirrors the mapping enforced by
/// `install::bundle::subdir_for` plus the skill installer's hardcoded
/// `skills` leaf. Returns `None` for `Mcp` — MCP servers are merged
/// into a shared `.mcp.json` rather than getting a per-id directory, so
/// they are snapshotted separately (see [`Snapshot::capture`]).
const fn dir_subdir_for(kind: PackageType) -> Option<&'static str> {
    match kind {
        PackageType::Skills => Some("skills"),
        PackageType::Subagents => Some("agents"),
        PackageType::Prompts => Some("prompts"),
        PackageType::Commands => Some("commands"),
        PackageType::Hooks => Some("hooks"),
        // Merged into `.mcp.json`, not a per-id tree.
        PackageType::Mcp => None,
    }
}

/// The list of `DepSpec`s for `kind` declared in the manifest, if any.
const fn deps_for_kind(manifest: &Manifest, kind: PackageType) -> Option<&Vec<DepSpec>> {
    let deps = &manifest.dependencies;
    match kind {
        PackageType::Skills => deps.skills.as_ref(),
        PackageType::Subagents => deps.subagents.as_ref(),
        PackageType::Prompts => deps.prompts.as_ref(),
        PackageType::Commands => deps.commands.as_ref(),
        PackageType::Hooks => deps.hooks.as_ref(),
        PackageType::Mcp => deps.mcp.as_ref(),
    }
}

/// One snapshotted target plus its captured prior state.
#[derive(Debug)]
struct Target {
    /// The live path the install run will write to.
    live: PathBuf,
    /// `Some(backup_path)` when the target pre-existed and its prior
    /// contents were moved aside; `None` when the target did not exist
    /// before the run (restore = delete the freshly-created entry).
    backup: Option<PathBuf>,
}

/// A pre-mutation snapshot of every filesystem target an install run
/// will touch. Hold it across the run; call [`commit`](Self::commit) on
/// success or [`restore`](Self::restore) on failure.
#[derive(Debug)]
pub struct Snapshot {
    /// Backing temp dir holding the moved-aside prior contents. Kept
    /// alive for the lifetime of the snapshot; dropped on commit.
    backup_root: tempfile::TempDir,
    targets: Vec<Target>,
}

impl Snapshot {
    /// Capture the prior on-disk state of every target the run will
    /// write, given the manifest, the resolved Claude Code home, and
    /// the project root (where `.mcp.json` lives).
    ///
    /// Targets are derived purely from the manifest's declared deps —
    /// we compute the destination directory each wired kind's installer
    /// would write to (`<claude_home>/<subdir>/<owner>-<name>/`) and the
    /// `.mcp.json` merge target when any `mcp:` dep is present. Deps
    /// whose shorthand can't be parsed into `<owner>/<name>` are skipped
    /// here (the installer will surface the parse error itself); they
    /// never reach a destination dir so there is nothing to snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the backup temp dir can't be created or a
    /// pre-existing target can't be moved aside.
    pub fn capture(manifest: &Manifest, claude_home: &Path, project_root: &Path) -> Result<Self> {
        let backup_root = tempfile::Builder::new()
            .prefix("pakx-install-rollback-")
            .tempdir()
            .context("create rollback backup dir")?;

        // De-dup live paths: two deps could (pathologically) resolve to
        // the same `<owner>-<name>` leaf within a kind. A `BTreeSet`
        // keeps the capture order stable and avoids double-backing-up.
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        let mut targets: Vec<Target> = Vec::new();
        let mut backup_idx: usize = 0;

        for &kind in ADAPTER_WIRED_KINDS {
            let Some(deps) = deps_for_kind(manifest, kind) else {
                continue;
            };
            if let Some(subdir) = dir_subdir_for(kind) {
                for dep in deps {
                    let Some(leaf) = dep_leaf(dep) else { continue };
                    let live = claude_home.join(subdir).join(&leaf);
                    if !seen.insert(live.clone()) {
                        continue;
                    }
                    let target = Self::capture_one(&live, backup_root.path(), &mut backup_idx)?;
                    targets.push(target);
                }
            }
        }

        // `.mcp.json` merge target: snapshot the file whenever any
        // `mcp:` dep is declared. The MCP installer reads-modifies-
        // writes this single shared file, so rolling back means
        // restoring its prior bytes (or removing it if the run created
        // it).
        if deps_for_kind(manifest, PackageType::Mcp).is_some_and(|d| !d.is_empty()) {
            let live = project_root.join(".mcp.json");
            if seen.insert(live.clone()) {
                let target = Self::capture_one(&live, backup_root.path(), &mut backup_idx)?;
                targets.push(target);
            }
        }

        debug!(
            target: "pakx::install::rollback",
            targets = targets.len(),
            "captured rollback snapshot"
        );

        Ok(Self {
            backup_root,
            targets,
        })
    }

    /// Snapshot one live path. If it pre-exists, move it into the backup
    /// dir under a unique leaf and record the backup location; otherwise
    /// record `backup: None` so restore knows to delete a freshly
    /// created entry.
    fn capture_one(live: &Path, backup_root: &Path, backup_idx: &mut usize) -> Result<Target> {
        if !live.exists() {
            return Ok(Target {
                live: live.to_path_buf(),
                backup: None,
            });
        }
        // Per-target backup leaf — a flat counter avoids any collision
        // between two targets that share a base name across kinds
        // (e.g. `skills/a-b` and `commands/a-b`).
        let backup = backup_root.join(format!("backup-{backup_idx}"));
        *backup_idx += 1;
        std::fs::rename(live, &backup).with_context(|| {
            format!(
                "move existing {} aside for rollback snapshot",
                live.display()
            )
        })?;
        Ok(Target {
            live: live.to_path_buf(),
            backup: Some(backup),
        })
    }

    /// Discard the snapshot, keeping whatever the install run wrote.
    /// Called on a clean (zero-failure) run. The backup temp dir is
    /// dropped here, unlinking the moved-aside prior contents.
    pub fn commit(self) {
        debug!(
            target: "pakx::install::rollback",
            targets = self.targets.len(),
            "install succeeded; committing (discarding rollback snapshot)"
        );
        // `backup_root` (a `TempDir`) drops here, removing the backups.
        drop(self.backup_root);
    }

    /// Restore every target to its pre-run state. Called on a failed run
    /// when `--rollback-on-error` is set.
    ///
    /// For each target: delete whatever the run left at the live path,
    /// then — if the target pre-existed — move its backup back into
    /// place. Targets that did not pre-exist are left absent.
    ///
    /// Restore is best-effort-resilient: a failure on one target is
    /// logged and the remaining targets are still attempted, so a single
    /// stubborn path can't strand the rest in a half-restored state. The
    /// first error encountered is returned after every target has been
    /// processed.
    ///
    /// # Errors
    ///
    /// Returns the first restore error encountered (after attempting all
    /// targets). The backup dir is retained on error so the user can
    /// recover by hand; on full success it is dropped.
    pub fn restore(self) -> Result<()> {
        let mut first_err: Option<anyhow::Error> = None;
        let mut restored = 0usize;

        for target in &self.targets {
            if let Err(e) = restore_one(target) {
                warn!(
                    target: "pakx::install::rollback",
                    path = %target.live.display(),
                    error = %e,
                    "failed to restore one target during rollback"
                );
                if first_err.is_none() {
                    first_err = Some(e);
                }
            } else {
                restored += 1;
            }
        }

        match first_err {
            None => {
                debug!(
                    target: "pakx::install::rollback",
                    restored,
                    "rollback complete; filesystem restored to pre-run state"
                );
                drop(self.backup_root);
                Ok(())
            }
            Some(e) => {
                // Keep the backup dir on disk so the user can recover the
                // moved-aside contents by hand. `keep` leaks the tempdir
                // (no auto-delete on drop).
                let kept = self.backup_root.keep();
                warn!(
                    target: "pakx::install::rollback",
                    backup_dir = %kept.display(),
                    "rollback incomplete; prior contents preserved in backup dir"
                );
                Err(e)
            }
        }
    }
}

/// Restore one target to its captured state.
fn restore_one(target: &Target) -> Result<()> {
    // Remove whatever the install run left behind at the live path.
    if target.live.is_dir() {
        std::fs::remove_dir_all(&target.live)
            .with_context(|| format!("remove {} during rollback", target.live.display()))?;
    } else if target.live.exists() {
        std::fs::remove_file(&target.live)
            .with_context(|| format!("remove {} during rollback", target.live.display()))?;
    }

    // If the target pre-existed, move its backup back into place.
    if let Some(backup) = &target.backup {
        if let Some(parent) = target.live.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("recreate parent {} during rollback", parent.display()))?;
        }
        std::fs::rename(backup, &target.live).with_context(|| {
            format!(
                "restore {} from backup during rollback",
                target.live.display()
            )
        })?;
    }
    Ok(())
}

/// The `<owner>-<name>` install-dir leaf for one dep, or `None` when the
/// dep's shorthand can't be parsed (git / registry-object specs, or a
/// malformed string). Only the `String` shorthand form is wired through
/// the per-id installers at v0, so anything else has no per-id directory
/// to snapshot — the installer reports those as failures on its own.
fn dep_leaf(dep: &DepSpec) -> Option<String> {
    let DepSpec::String(s) = dep else {
        return None;
    };
    let (owner, name, _) = parse_skill_shorthand(s.as_str()).ok()?;
    Some(format!("{owner}-{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pakx_core::PACKAGE_TYPES;

    /// `dir_subdir_for` must agree with the bundle installer + skill
    /// installer about where each kind's per-id tree lands. If a future
    /// kind is added to `ADAPTER_WIRED_KINDS`, this test (plus the
    /// runner's coverage test) forces the author to decide its subdir.
    #[test]
    fn dir_subdir_covers_every_wired_kind_except_mcp() {
        for &kind in ADAPTER_WIRED_KINDS {
            if kind == PackageType::Mcp {
                assert!(
                    dir_subdir_for(kind).is_none(),
                    "mcp has no per-id tree (merged into .mcp.json)"
                );
            } else {
                assert!(
                    dir_subdir_for(kind).is_some(),
                    "{} is wired but has no rollback subdir mapping",
                    kind.as_str()
                );
            }
        }
    }

    /// Every `PackageType` variant must be handled by `dir_subdir_for`
    /// (no `_ =>` arm), so adding a kind is a compile error until its
    /// rollback subdir is decided.
    #[test]
    fn dir_subdir_total_over_package_types() {
        for kind in PACKAGE_TYPES {
            // Just exercising the match — Skills..Hooks return Some,
            // Mcp returns None; either way it must not panic.
            let _ = dir_subdir_for(kind);
        }
    }

    fn string_dep(s: &str) -> DepSpec {
        DepSpec::String(pakx_core::manifest::StringSpec::parse(s).unwrap())
    }

    #[test]
    fn dep_leaf_parses_string_shorthand() {
        assert_eq!(
            dep_leaf(&string_dep("alice/hello-world")).as_deref(),
            Some("alice-hello-world")
        );
    }

    #[test]
    fn dep_leaf_strips_version_pin() {
        assert_eq!(
            dep_leaf(&string_dep("alice/hello@1.2.3")).as_deref(),
            Some("alice-hello")
        );
    }

    #[test]
    fn dep_leaf_none_for_malformed_string() {
        assert!(dep_leaf(&string_dep("no-slash-here")).is_none());
    }

    /// Capture → restore round-trip on a target that did NOT pre-exist:
    /// the install run creates it, restore must delete it.
    #[test]
    fn restore_removes_newly_created_target() {
        let claude = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let manifest = manifest_with_skill("alice/fresh");

        let snap = Snapshot::capture(&manifest, claude.path(), project.path()).unwrap();

        // Simulate the installer writing the skill tree.
        let live = claude.path().join("skills").join("alice-fresh");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("SKILL.md"), b"installed").unwrap();
        assert!(live.exists());

        snap.restore().unwrap();
        assert!(
            !live.exists(),
            "newly-created target must be removed on rollback"
        );
    }

    /// Capture → restore round-trip on a target that pre-existed: the
    /// install run overwrites it, restore must bring the prior CONTENTS
    /// back (not merely the directory's presence).
    #[test]
    fn restore_brings_back_prior_contents() {
        let claude = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let manifest = manifest_with_skill("alice/existing");

        // Pre-existing install with a distinctive marker file.
        let live = claude.path().join("skills").join("alice-existing");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("OLD.md"), b"version-one").unwrap();

        let snap = Snapshot::capture(&manifest, claude.path(), project.path()).unwrap();

        // After capture the live dir was moved aside, so the installer
        // starts from a clean slate. Simulate it writing a NEW tree.
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("NEW.md"), b"version-two").unwrap();

        snap.restore().unwrap();

        assert!(
            live.join("OLD.md").is_file(),
            "prior contents must be restored"
        );
        assert_eq!(std::fs::read(live.join("OLD.md")).unwrap(), b"version-one");
        assert!(
            !live.join("NEW.md").exists(),
            "the failed run's writes must be wiped"
        );
    }

    /// Commit keeps whatever the run wrote and drops the backup.
    #[test]
    fn commit_keeps_installed_tree() {
        let claude = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let manifest = manifest_with_skill("alice/keep");

        let snap = Snapshot::capture(&manifest, claude.path(), project.path()).unwrap();

        let live = claude.path().join("skills").join("alice-keep");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("SKILL.md"), b"installed").unwrap();

        snap.commit();
        assert!(
            live.join("SKILL.md").is_file(),
            "commit must leave the installed tree in place"
        );
    }

    /// `.mcp.json` is snapshotted when an `mcp:` dep is present and
    /// restored on rollback.
    #[test]
    fn restore_brings_back_prior_mcp_json() {
        let claude = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let manifest = manifest_with_mcp("some-server");

        let mcp_path = project.path().join(".mcp.json");
        std::fs::write(&mcp_path, br#"{"mcpServers":{}}"#).unwrap();

        let snap = Snapshot::capture(&manifest, claude.path(), project.path()).unwrap();

        // Simulate the MCP installer rewriting the merge file.
        std::fs::write(&mcp_path, br#"{"mcpServers":{"some-server":{}}}"#).unwrap();

        snap.restore().unwrap();
        assert_eq!(
            std::fs::read(&mcp_path).unwrap(),
            br#"{"mcpServers":{}}"#,
            "prior .mcp.json bytes must be restored"
        );
    }

    fn manifest_with_skill(id: &str) -> Manifest {
        let yaml = format!("name: demo\nversion: 0.0.0\ndependencies:\n  skills:\n    - {id}\n");
        pakx_core::parse_manifest(&yaml, None).unwrap()
    }

    fn manifest_with_mcp(id: &str) -> Manifest {
        let yaml = format!("name: demo\nversion: 0.0.0\ndependencies:\n  mcp:\n    - {id}\n");
        pakx_core::parse_manifest(&yaml, None).unwrap()
    }
}
