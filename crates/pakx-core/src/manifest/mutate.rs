//! In-memory mutation helpers for [`Manifest`].

use super::schema::{DepSpec, Dependencies, Manifest, PackageType, StringSpec, PACKAGE_TYPES};

/// Result of adding a dep to a manifest. Distinguishes idempotent re-runs
/// from real changes so the caller can choose between "nothing to do" and
/// "rewrite file".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    Added,
    AlreadyPresent,
}

/// Append `dep` to `manifest.dependencies.<kind>`. No-ops + returns
/// `AlreadyPresent` if the dep is already in the list.
///
/// Equality for shorthand string specs is exact-match; future versions
/// may normalise (e.g. trim trailing `@latest`) — for v0.1 we keep it
/// literal so the user sees exactly what they typed.
pub fn add_dep(manifest: &mut Manifest, kind: PackageType, dep: DepSpec) -> AddOutcome {
    let list = ensure_list(&mut manifest.dependencies, kind);
    if list.iter().any(|existing| existing == &dep) {
        return AddOutcome::AlreadyPresent;
    }
    list.push(dep);
    AddOutcome::Added
}

/// Convenience: add a shorthand string dep.
pub fn add_shorthand(
    manifest: &mut Manifest,
    kind: PackageType,
    id: impl Into<String>,
) -> Result<AddOutcome, String> {
    let spec = StringSpec::parse(id)?;
    Ok(add_dep(manifest, kind, DepSpec::String(spec)))
}

/// Outcome of [`remove_shorthand`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOutcome {
    /// Entry removed from the named section.
    Removed,
    /// The named section had no matching entry. Caller's choice whether
    /// that's a soft no-op or a hard error.
    NotPresent,
}

/// Remove a shorthand-string dep `id` from `manifest.dependencies.<kind>`.
///
/// Only matches `DepSpec::String` entries with byte-for-byte equality
/// against `id` — the same exact-match rule [`add_dep`] uses. Git and
/// registry-object specs are intentionally out of scope for v0.1 because
/// their identity isn't a single string. Order of the remaining entries
/// is preserved.
///
/// Empties the section vector if the removal leaves it empty so the
/// serialised YAML doesn't carry a vestigial empty list (matches the
/// `skip_serializing_if` posture elsewhere).
pub fn remove_shorthand(manifest: &mut Manifest, kind: PackageType, id: &str) -> RemoveOutcome {
    let Some(list) = section_mut(&mut manifest.dependencies, kind) else {
        return RemoveOutcome::NotPresent;
    };
    let before = list.len();
    list.retain(|dep| !matches_shorthand(dep, id));
    if list.len() == before {
        return RemoveOutcome::NotPresent;
    }
    if list.is_empty() {
        clear_section(&mut manifest.dependencies, kind);
    }
    RemoveOutcome::Removed
}

/// Every section that currently contains a shorthand entry matching `id`.
/// Used by `pakx remove` to detect ambiguity before mutating anything.
#[must_use]
pub fn sections_containing(manifest: &Manifest, id: &str) -> Vec<PackageType> {
    PACKAGE_TYPES
        .iter()
        .copied()
        .filter(|kind| {
            manifest
                .dependencies
                .get(*kind)
                .is_some_and(|list| list.iter().any(|dep| matches_shorthand(dep, id)))
        })
        .collect()
}

fn matches_shorthand(dep: &DepSpec, id: &str) -> bool {
    match dep {
        DepSpec::String(s) => s.as_str() == id,
        // Git / registry-object specs are not addressable by the
        // shorthand string `pakx remove` accepts; ignore them.
        DepSpec::Git(_) | DepSpec::Registry(_) => false,
    }
}

const fn section_mut(deps: &mut Dependencies, kind: PackageType) -> Option<&mut Vec<DepSpec>> {
    match kind {
        PackageType::Skills => deps.skills.as_mut(),
        PackageType::Mcp => deps.mcp.as_mut(),
        PackageType::Subagents => deps.subagents.as_mut(),
        PackageType::Prompts => deps.prompts.as_mut(),
        PackageType::Commands => deps.commands.as_mut(),
        PackageType::Hooks => deps.hooks.as_mut(),
    }
}

fn clear_section(deps: &mut Dependencies, kind: PackageType) {
    match kind {
        PackageType::Skills => deps.skills = None,
        PackageType::Mcp => deps.mcp = None,
        PackageType::Subagents => deps.subagents = None,
        PackageType::Prompts => deps.prompts = None,
        PackageType::Commands => deps.commands = None,
        PackageType::Hooks => deps.hooks = None,
    }
}

fn ensure_list(deps: &mut Dependencies, kind: PackageType) -> &mut Vec<DepSpec> {
    match kind {
        PackageType::Skills => deps.skills.get_or_insert_with(Vec::new),
        PackageType::Mcp => deps.mcp.get_or_insert_with(Vec::new),
        PackageType::Subagents => deps.subagents.get_or_insert_with(Vec::new),
        PackageType::Prompts => deps.prompts.get_or_insert_with(Vec::new),
        PackageType::Commands => deps.commands.get_or_insert_with(Vec::new),
        PackageType::Hooks => deps.hooks.get_or_insert_with(Vec::new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::schema::{Dependencies, Manifest};

    fn empty_manifest() -> Manifest {
        Manifest {
            name: "demo".into(),
            version: "0.1.0".into(),
            agents: None,
            dependencies: Dependencies::default(),
        }
    }

    #[test]
    fn remove_shorthand_returns_not_present_for_empty_section() {
        let mut m = empty_manifest();
        let outcome = remove_shorthand(&mut m, PackageType::Mcp, "a/b");
        assert_eq!(outcome, RemoveOutcome::NotPresent);
    }

    #[test]
    fn remove_shorthand_drops_matching_entry_and_clears_section_when_empty() {
        let mut m = empty_manifest();
        add_shorthand(&mut m, PackageType::Mcp, "a/b").unwrap();
        let outcome = remove_shorthand(&mut m, PackageType::Mcp, "a/b");
        assert_eq!(outcome, RemoveOutcome::Removed);
        assert!(m.dependencies.mcp.is_none(), "empty section pruned");
    }

    #[test]
    fn remove_shorthand_preserves_order_of_remaining_entries() {
        let mut m = empty_manifest();
        add_shorthand(&mut m, PackageType::Mcp, "a").unwrap();
        add_shorthand(&mut m, PackageType::Mcp, "b").unwrap();
        add_shorthand(&mut m, PackageType::Mcp, "c").unwrap();
        let outcome = remove_shorthand(&mut m, PackageType::Mcp, "b");
        assert_eq!(outcome, RemoveOutcome::Removed);
        let names: Vec<&str> = m
            .dependencies
            .mcp
            .as_ref()
            .unwrap()
            .iter()
            .map(|d| match d {
                DepSpec::String(s) => s.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn sections_containing_lists_every_kind_with_a_match() {
        let mut m = empty_manifest();
        add_shorthand(&mut m, PackageType::Mcp, "shared").unwrap();
        add_shorthand(&mut m, PackageType::Skills, "shared").unwrap();
        let sections = sections_containing(&m, "shared");
        assert_eq!(sections.len(), 2);
        assert!(sections.contains(&PackageType::Mcp));
        assert!(sections.contains(&PackageType::Skills));
    }

    #[test]
    fn sections_containing_ignores_git_and_registry_specs() {
        let mut m = empty_manifest();
        m.dependencies.mcp = Some(vec![DepSpec::Git(crate::manifest::GitSpec {
            git: "https://example.test".into(),
            git_ref: None,
            subpath: None,
        })]);
        assert!(sections_containing(&m, "https://example.test").is_empty());
    }
}
