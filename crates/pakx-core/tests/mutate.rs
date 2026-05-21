//! Unit tests for manifest in-memory mutation helpers.

use pakx_core::{
    add_dep, add_shorthand, AddOutcome, DepSpec, Dependencies, Manifest, PackageType, StringSpec,
};

fn empty() -> Manifest {
    Manifest {
        name: "p".into(),
        version: "1.0.0".into(),
        agents: None,
        dependencies: Dependencies::default(),
    }
}

#[test]
fn add_dep_appends_when_absent() {
    let mut m = empty();
    let outcome = add_dep(
        &mut m,
        PackageType::Mcp,
        DepSpec::String(StringSpec::parse("a/b").unwrap()),
    );
    assert_eq!(outcome, AddOutcome::Added);
    assert_eq!(m.dependencies.mcp.as_ref().unwrap().len(), 1);
}

#[test]
fn add_dep_is_idempotent_on_exact_match() {
    let mut m = empty();
    let dep = DepSpec::String(StringSpec::parse("a/b").unwrap());
    add_dep(&mut m, PackageType::Mcp, dep.clone());
    let outcome = add_dep(&mut m, PackageType::Mcp, dep);
    assert_eq!(outcome, AddOutcome::AlreadyPresent);
    assert_eq!(m.dependencies.mcp.as_ref().unwrap().len(), 1);
}

#[test]
fn add_shorthand_validates_id() {
    let mut m = empty();
    let err = add_shorthand(&mut m, PackageType::Mcp, "has whitespace");
    assert!(err.is_err());
}

#[test]
fn add_dep_initialises_list_for_each_package_type() {
    let kinds = [
        PackageType::Skills,
        PackageType::Mcp,
        PackageType::Subagents,
        PackageType::Prompts,
        PackageType::Commands,
        PackageType::Hooks,
    ];
    for kind in kinds {
        let mut m = empty();
        let outcome = add_shorthand(&mut m, kind, "owner/name").unwrap();
        assert_eq!(outcome, AddOutcome::Added);
        assert!(m.dependencies.get(kind).is_some(), "{kind:?} list missing");
    }
}
