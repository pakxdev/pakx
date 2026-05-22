//! Integration tests for manifest parse + write.

use std::path::Path;

use pakx_core::{
    parse_manifest, write_manifest, AgentId, DepSpec, Dependencies, Manifest, PackageType,
};

const FULL_MANIFEST_YAML: &str = r#"name: my-project
version: 1.0.0

agents:
  - claude-code
  - cursor
  - codex

dependencies:
  skills:
    - anthropics/skills/pdf
    - acme/code-review@^2.0
    - git: https://gitlab.com/acme/style-guide.git
      ref: v1.3.0

  mcp:
    - smithery/github-mcp
    - io.github.microsoft/playwright-mcp
    - registry: official
      name: filesystem
      args:
        - "~/projects"

  subagents:
    - voltagent/code-reviewer

  prompts:
    - team/pr-review.prompt.md@main

  commands:
    - jest/run-tests

  hooks:
    - pre-commit/lint
"#;

#[test]
fn parses_full_spec_manifest() {
    let m = parse_manifest(FULL_MANIFEST_YAML, None).unwrap();
    assert_eq!(m.name, "my-project");
    assert_eq!(m.version, "1.0.0");
    let agents = m.agents.as_ref().expect("agents present");
    assert_eq!(agents.len(), 3);
    assert_eq!(agents[0].as_str(), "claude-code");
    assert_eq!(m.dependencies.skills.as_ref().unwrap().len(), 3);
    assert_eq!(m.dependencies.mcp.as_ref().unwrap().len(), 3);
}

#[test]
fn identifies_shorthand_git_and_registry_dep_forms() {
    let m = parse_manifest(FULL_MANIFEST_YAML, None).unwrap();
    let skills = m.dependencies.skills.as_ref().unwrap();
    assert!(matches!(skills[0], DepSpec::String(_)));
    assert!(matches!(skills[2], DepSpec::Git(_)));
    let mcp = m.dependencies.mcp.as_ref().unwrap();
    assert!(matches!(mcp[2], DepSpec::Registry(_)));
}

#[test]
fn parses_minimal_manifest_without_agents_or_dependencies() {
    let m = parse_manifest("name: x\nversion: 0.1.0\n", None).unwrap();
    assert_eq!(m.name, "x");
    assert!(m.agents.is_none());
    assert_eq!(m.dependencies, Dependencies::default());
}

#[test]
fn parses_manifest_with_dependencies_but_no_agents() {
    let m = parse_manifest(
        "name: x\nversion: 0.1.0\ndependencies:\n  skills:\n    - foo/bar\n",
        None,
    )
    .unwrap();
    assert!(m.agents.is_none());
    assert_eq!(m.dependencies.skills.as_ref().unwrap().len(), 1);
}

#[test]
fn rejects_invalid_yaml() {
    let err = parse_manifest(":\n  - [", None).unwrap_err();
    assert!(matches!(err, pakx_core::ManifestError::ParseYaml { .. }));
}

#[test]
fn rejects_non_mapping_top_level() {
    let err = parse_manifest("- a\n- b\n- c\n", None).unwrap_err();
    assert!(matches!(err, pakx_core::ManifestError::Schema { .. }));
}

#[test]
fn rejects_manifest_missing_name() {
    let err = parse_manifest("version: 1.0.0\n", None).unwrap_err();
    assert!(matches!(err, pakx_core::ManifestError::Schema { .. }));
}

#[test]
fn rejects_unknown_top_level_keys() {
    let err = parse_manifest("name: x\nversion: 1.0.0\nextraneous: nope\n", None).unwrap_err();
    assert!(matches!(err, pakx_core::ManifestError::Schema { .. }));
}

#[test]
fn rejects_whitespace_in_shorthand_dep() {
    let err = parse_manifest(
        "name: x\nversion: 1.0.0\ndependencies:\n  skills:\n    - \"has spaces\"\n",
        None,
    )
    .unwrap_err();
    assert!(matches!(err, pakx_core::ManifestError::Schema { .. }));
}

#[test]
fn rejects_invalid_agent_id() {
    let err = parse_manifest("name: x\nversion: 1.0.0\nagents:\n  - 1bad\n", None).unwrap_err();
    assert!(matches!(err, pakx_core::ManifestError::Schema { .. }));
}

#[test]
fn error_carries_path_when_supplied() {
    let path = Path::new("./agents.yml");
    let err = parse_manifest("name: x\n", Some(path)).unwrap_err();
    assert_eq!(err.path().map(std::path::PathBuf::as_path), Some(path));
}

#[test]
fn round_trips_full_manifest() {
    let first = parse_manifest(FULL_MANIFEST_YAML, None).unwrap();
    let rendered = write_manifest(&first);
    let second = parse_manifest(&rendered, None).unwrap();
    assert_eq!(first, second);
}

#[test]
fn write_emits_canonical_top_level_order() {
    let m = Manifest {
        name: "p".into(),
        version: "1.0.0".into(),
        agents: Some(vec![AgentId::new_unchecked("claude-code")]),
        dependencies: Dependencies {
            skills: Some(vec![DepSpec::String(
                pakx_core::StringSpec::parse("foo/bar").unwrap(),
            )]),
            ..Dependencies::default()
        },
    };
    let out = write_manifest(&m);
    let name_idx = out.find("name:").expect("name present");
    let version_idx = out.find("version:").expect("version present");
    let agents_idx = out.find("agents:").expect("agents present");
    let deps_idx = out.find("dependencies:").expect("dependencies present");
    assert!(name_idx < version_idx);
    assert!(version_idx < agents_idx);
    assert!(agents_idx < deps_idx);
}

#[test]
fn write_omits_agents_when_none() {
    let m = Manifest {
        name: "p".into(),
        version: "1.0.0".into(),
        agents: None,
        dependencies: Dependencies::default(),
    };
    let out = write_manifest(&m);
    assert!(!out.contains("agents:"));
}

#[test]
fn write_omits_empty_dep_types() {
    let m = Manifest {
        name: "p".into(),
        version: "1.0.0".into(),
        agents: None,
        dependencies: Dependencies {
            skills: Some(vec![DepSpec::String(
                pakx_core::StringSpec::parse("foo/bar").unwrap(),
            )]),
            ..Dependencies::default()
        },
    };
    let out = write_manifest(&m);
    assert!(out.contains("skills:"));
    assert!(!out.contains("mcp:"));
}

#[test]
fn sponsor_kind_lowercase_parses_and_pascal_does_not() {
    // The `#[serde(rename_all = "lowercase")]` attribute is the only
    // thing guarding the wire shape against case drift — pin it.
    let ok: pakx_core::Sponsor =
        serde_yaml_ng::from_str("kind: github\nurl: https://github.com/sponsors/octocat\n")
            .expect("lowercase kind parses");
    assert!(matches!(ok.kind, pakx_core::SponsorKind::Github));

    let err = serde_yaml_ng::from_str::<pakx_core::Sponsor>(
        "kind: GitHub\nurl: https://github.com/sponsors/octocat\n",
    );
    assert!(err.is_err(), "PascalCase kind must NOT parse, got: {err:?}");
}

#[test]
fn sponsor_struct_denies_unknown_fields() {
    let err = serde_yaml_ng::from_str::<pakx_core::Sponsor>(
        "kind: github\nurl: https://github.com/sponsors/octocat\nextra: 1\n",
    );
    assert!(err.is_err(), "unknown field on Sponsor must be rejected");
}

#[test]
fn dependencies_get_returns_correct_list() {
    let m = parse_manifest(FULL_MANIFEST_YAML, None).unwrap();
    assert!(m.dependencies.get(PackageType::Skills).is_some());
    assert!(m.dependencies.get(PackageType::Mcp).is_some());
    // hooks IS in the spec example; check absence on a different one.
    let bare = parse_manifest("name: x\nversion: 1.0.0\n", None).unwrap();
    assert!(bare.dependencies.get(PackageType::Hooks).is_none());
}
