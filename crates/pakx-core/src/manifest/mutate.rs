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

/// Outcome of [`update_shorthand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// Entry rewritten in-place. Carries the prior shorthand text so the
    /// caller can log a `0.1.0 -> 0.1.2`-style diff line.
    Updated { previous: String },
    /// No shorthand entry whose pre-`@` id matches `id_no_version` was
    /// found in the named section. Caller's choice whether that's a
    /// soft no-op or a hard error (`pakx update <id>` treats it as an
    /// error; the interactive path skips silently).
    NotPresent,
    /// An entry was found, but it's a git or registry-object spec —
    /// `pakx update` only handles shorthand strings at v0.1.
    NonShorthand,
}

/// Rewrite a shorthand-string dep in `manifest.dependencies.<kind>`
/// from `<id_no_version>[@<old>]` to `<id_no_version>@<new_version>`.
///
/// Matching is **id-only**: every shorthand entry in the section whose
/// pre-`@` segment equals `id_no_version` is a candidate (a manifest
/// can validly contain `owner/name` and `owner/name@0.1.0` only by way
/// of duplicates — `add_shorthand` rejects exact duplicates but allows
/// the version-less / versioned mix; `update_shorthand` collapses them
/// onto the new pin). The first match in source order is rewritten;
/// any other duplicates remain untouched (the duplicate case is
/// vanishingly rare in practice and surfacing it would require an
/// `UpdateOutcome::Ambiguous` variant the CLI has nowhere to lift to
/// today).
///
/// `id_no_version` MUST be the pre-`@` segment — passing
/// `owner/name@0.1.0` here will never match. Splitting is the caller's
/// job and mirrors the semantics of `parse_skill_shorthand`.
pub fn update_shorthand(
    manifest: &mut Manifest,
    kind: PackageType,
    id_no_version: &str,
    new_version: &str,
) -> UpdateOutcome {
    let Some(list) = section_mut(&mut manifest.dependencies, kind) else {
        return UpdateOutcome::NotPresent;
    };
    // Walk the list once and find the first shorthand whose pre-`@`
    // segment matches. Bail with `NonShorthand` if we hit a matching
    // git / registry-object spec first — surfaces the source-form
    // mismatch precisely instead of silently treating it as
    // "NotPresent" and confusing the user.
    let mut found_non_shorthand = false;
    for dep in list.iter_mut() {
        match dep {
            DepSpec::String(s) => {
                let (id_part, _) = split_shorthand(s.as_str());
                if id_part == id_no_version {
                    let previous = s.as_str().to_owned();
                    let next = format!("{id_no_version}@{new_version}");
                    // `StringSpec::parse` only rejects whitespace +
                    // empty; the rewritten value is shaped identically
                    // to a freshly-parsed shorthand so a parse error
                    // here would be a real bug.
                    *s = StringSpec::parse(next)
                        .expect("rewritten shorthand has no whitespace and is non-empty");
                    return UpdateOutcome::Updated { previous };
                }
            }
            DepSpec::Git(_) | DepSpec::Registry(_) => {
                // Only count git/registry as "blocking" when nothing
                // else has matched yet — otherwise a String match
                // later in the list would still win.
                if let DepSpec::Git(g) = dep {
                    if g.git == id_no_version {
                        found_non_shorthand = true;
                    }
                } else if let DepSpec::Registry(r) = dep {
                    let combined = format!("{}/{}", r.registry, r.name);
                    if combined == id_no_version || r.name == id_no_version {
                        found_non_shorthand = true;
                    }
                }
            }
        }
    }
    if found_non_shorthand {
        UpdateOutcome::NonShorthand
    } else {
        UpdateOutcome::NotPresent
    }
}

/// Split a shorthand into `(id, Option<version>)`.
///
/// Returns `(<id>, Some(<version>))` when the string contains an `@`
/// with a non-empty version suffix, otherwise `(<id>, None)`. The
/// original string is returned verbatim when the `@` is the first
/// character (degenerate but well-formed input that `StringSpec::parse`
/// would have rejected at load time anyway).
#[must_use]
pub fn split_shorthand(s: &str) -> (&str, Option<&str>) {
    match s.split_once('@') {
        Some(("", _)) => (s, None),
        Some((id, v)) if !v.is_empty() => (id, Some(v)),
        _ => (s, None),
    }
}

/// Sections holding a shorthand whose pre-`@` segment matches `id_no_version`.
///
/// Mirrors [`sections_containing`] but compares the
/// **id-without-version** rather than the full shorthand string —
/// what `pakx update` wants when the user types
/// `pakx update owner/name` (no version pin).
#[must_use]
pub fn sections_containing_id(manifest: &Manifest, id_no_version: &str) -> Vec<PackageType> {
    PACKAGE_TYPES
        .iter()
        .copied()
        .filter(|kind| {
            manifest.dependencies.get(*kind).is_some_and(|list| {
                list.iter()
                    .any(|dep| matches_shorthand_id(dep, id_no_version))
            })
        })
        .collect()
}

fn matches_shorthand_id(dep: &DepSpec, id_no_version: &str) -> bool {
    match dep {
        DepSpec::String(s) => split_shorthand(s.as_str()).0 == id_no_version,
        DepSpec::Git(_) | DepSpec::Registry(_) => false,
    }
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

    #[test]
    fn split_shorthand_separates_id_and_version() {
        assert_eq!(split_shorthand("alice/bob"), ("alice/bob", None));
        assert_eq!(
            split_shorthand("alice/bob@0.1.0"),
            ("alice/bob", Some("0.1.0"))
        );
        // Trailing `@` with no version → treat as no version.
        assert_eq!(split_shorthand("alice/bob@"), ("alice/bob@", None));
        // Leading `@` is degenerate; we keep the whole thing as the id.
        assert_eq!(split_shorthand("@scope/name"), ("@scope/name", None));
    }

    #[test]
    fn update_shorthand_rewrites_pin_in_place() {
        let mut m = empty_manifest();
        add_shorthand(&mut m, PackageType::Skills, "alice/bob@0.1.0").unwrap();
        let outcome = update_shorthand(&mut m, PackageType::Skills, "alice/bob", "0.1.2");
        assert_eq!(
            outcome,
            UpdateOutcome::Updated {
                previous: "alice/bob@0.1.0".into()
            }
        );
        let after = m
            .dependencies
            .skills
            .as_ref()
            .and_then(|v| v.first())
            .and_then(|d| match d {
                DepSpec::String(s) => Some(s.as_str()),
                _ => None,
            });
        assert_eq!(after, Some("alice/bob@0.1.2"));
    }

    #[test]
    fn update_shorthand_pins_unversioned_entry() {
        // Manifest holds the bare `alice/bob` (no `@<ver>` pin). `pakx
        // update` MUST be able to add a pin from scratch — otherwise
        // running `pakx update` on a freshly-`add`ed dep would be a
        // no-op.
        let mut m = empty_manifest();
        add_shorthand(&mut m, PackageType::Skills, "alice/bob").unwrap();
        let outcome = update_shorthand(&mut m, PackageType::Skills, "alice/bob", "0.1.2");
        assert_eq!(
            outcome,
            UpdateOutcome::Updated {
                previous: "alice/bob".into()
            }
        );
        let after = m
            .dependencies
            .skills
            .as_ref()
            .and_then(|v| v.first())
            .and_then(|d| match d {
                DepSpec::String(s) => Some(s.as_str()),
                _ => None,
            });
        assert_eq!(after, Some("alice/bob@0.1.2"));
    }

    #[test]
    fn update_shorthand_returns_not_present_when_section_missing() {
        let mut m = empty_manifest();
        let outcome = update_shorthand(&mut m, PackageType::Skills, "alice/bob", "0.1.2");
        assert_eq!(outcome, UpdateOutcome::NotPresent);
    }

    #[test]
    fn update_shorthand_returns_not_present_when_id_missing() {
        let mut m = empty_manifest();
        add_shorthand(&mut m, PackageType::Skills, "other/dep@0.1.0").unwrap();
        let outcome = update_shorthand(&mut m, PackageType::Skills, "alice/bob", "0.1.2");
        assert_eq!(outcome, UpdateOutcome::NotPresent);
    }

    #[test]
    fn update_shorthand_returns_non_shorthand_for_git_only_match() {
        let mut m = empty_manifest();
        m.dependencies.mcp = Some(vec![DepSpec::Git(crate::manifest::GitSpec {
            git: "alice/bob".into(),
            git_ref: None,
            subpath: None,
        })]);
        let outcome = update_shorthand(&mut m, PackageType::Mcp, "alice/bob", "0.1.2");
        assert_eq!(outcome, UpdateOutcome::NonShorthand);
    }

    #[test]
    fn update_shorthand_rewrites_first_match_when_duplicate_pins_present() {
        // Manifests should not contain duplicates in practice (add_dep
        // refuses byte-for-byte dupes), but a mixed `owner/name` and
        // `owner/name@0.1.0` IS allowed because the strings differ.
        // The first source-order match wins.
        let mut m = empty_manifest();
        add_shorthand(&mut m, PackageType::Skills, "alice/bob").unwrap();
        add_shorthand(&mut m, PackageType::Skills, "alice/bob@0.1.0").unwrap();
        let outcome = update_shorthand(&mut m, PackageType::Skills, "alice/bob", "0.1.2");
        assert_eq!(
            outcome,
            UpdateOutcome::Updated {
                previous: "alice/bob".into()
            }
        );
        let entries: Vec<&str> = m
            .dependencies
            .skills
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|d| match d {
                DepSpec::String(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(entries, vec!["alice/bob@0.1.2", "alice/bob@0.1.0"]);
    }

    #[test]
    fn sections_containing_id_finds_unversioned_and_versioned_pins() {
        let mut m = empty_manifest();
        add_shorthand(&mut m, PackageType::Skills, "alice/bob@0.1.0").unwrap();
        add_shorthand(&mut m, PackageType::Mcp, "alice/bob").unwrap();
        let sections = sections_containing_id(&m, "alice/bob");
        assert_eq!(sections.len(), 2);
        assert!(sections.contains(&PackageType::Skills));
        assert!(sections.contains(&PackageType::Mcp));
    }

    #[test]
    fn sections_containing_id_ignores_unrelated_ids() {
        let mut m = empty_manifest();
        add_shorthand(&mut m, PackageType::Skills, "other/dep@0.1.0").unwrap();
        assert!(sections_containing_id(&m, "alice/bob").is_empty());
    }
}
