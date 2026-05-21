//! In-memory mutation helpers for [`Manifest`].

use super::schema::{DepSpec, Dependencies, Manifest, PackageType, StringSpec};

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
