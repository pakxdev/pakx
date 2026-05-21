//! Integration tests for lockfile parse + write.

use std::collections::BTreeMap;

use pakx_core::{
    parse_lockfile, write_lockfile, AgentId, Integrity, LockEntry, Lockfile, PackageType,
    RegistrySource, LOCKFILE_VERSION,
};

fn sample_integrity() -> Integrity {
    Integrity::parse(format!("sha256-{}=", "A".repeat(43))).unwrap()
}

fn sample_lockfile() -> Lockfile {
    let mut entries = BTreeMap::new();
    entries.insert(
        "skills/anthropics/pdf@1.2.0".into(),
        LockEntry {
            name: "anthropics/pdf".into(),
            kind: PackageType::Skills,
            version: "1.2.0".into(),
            resolved_from: "https://github.com/anthropics/skills/tree/v1.2.0/pdf".into(),
            registry: RegistrySource::Github,
            integrity: sample_integrity(),
            agents: vec![
                AgentId::new_unchecked("cursor"),
                AgentId::new_unchecked("claude-code"),
            ],
            dependencies: vec![],
        },
    );
    entries.insert(
        "mcp/smithery/github-mcp@0.5.1".into(),
        LockEntry {
            name: "smithery/github-mcp".into(),
            kind: PackageType::Mcp,
            version: "0.5.1".into(),
            resolved_from: "https://smithery.ai/server/github-mcp/0.5.1".into(),
            registry: RegistrySource::Smithery,
            integrity: sample_integrity(),
            agents: vec![AgentId::new_unchecked("claude-code")],
            dependencies: vec![],
        },
    );
    Lockfile {
        lockfile_version: LOCKFILE_VERSION,
        manifest_hash: sample_integrity(),
        entries,
    }
}

#[test]
fn parses_valid_lockfile() {
    let json = write_lockfile(&sample_lockfile());
    let parsed = parse_lockfile(&json, None).unwrap();
    assert_eq!(parsed.lockfile_version, LOCKFILE_VERSION);
}

#[test]
fn rejects_invalid_json() {
    let err = parse_lockfile("{not json", None).unwrap_err();
    assert!(matches!(err, pakx_core::LockfileError::ParseJson { .. }));
}

#[test]
fn rejects_wrong_lockfile_version() {
    let mut lf = sample_lockfile();
    lf.lockfile_version = 99;
    let json = serde_json::to_string(&lf).unwrap();
    let err = parse_lockfile(&json, None).unwrap_err();
    assert!(matches!(err, pakx_core::LockfileError::Schema { .. }));
}

#[test]
fn rejects_malformed_integrity() {
    // Build raw JSON manually so we can inject a bad integrity string.
    let bad = r#"{
  "lockfileVersion": 1,
  "manifestHash": "sha1-tooshort",
  "entries": {}
}"#;
    let err = parse_lockfile(bad, None).unwrap_err();
    assert!(matches!(err, pakx_core::LockfileError::ParseJson { .. }));
}

#[test]
fn rejects_unknown_registry_source() {
    let bad = format!(
        r#"{{
  "lockfileVersion": 1,
  "manifestHash": "sha256-{int}=",
  "entries": {{
    "skills/anthropics/pdf@1.2.0": {{
      "name": "anthropics/pdf",
      "type": "skills",
      "version": "1.2.0",
      "resolvedFrom": "https://example.com",
      "registry": "made-up",
      "integrity": "sha256-{int}=",
      "agents": [],
      "dependencies": []
    }}
  }}
}}"#,
        int = "A".repeat(43),
    );
    let err = parse_lockfile(&bad, None).unwrap_err();
    assert!(matches!(err, pakx_core::LockfileError::ParseJson { .. }));
}

#[test]
fn rejects_bad_entry_key_shape() {
    let bad = format!(
        r#"{{
  "lockfileVersion": 1,
  "manifestHash": "sha256-{int}=",
  "entries": {{
    "no-type-prefix@1.0.0": {{
      "name": "x",
      "type": "skills",
      "version": "1.0.0",
      "resolvedFrom": "https://example.com",
      "registry": "github",
      "integrity": "sha256-{int}=",
      "agents": [],
      "dependencies": []
    }}
  }}
}}"#,
        int = "A".repeat(43),
    );
    let err = parse_lockfile(&bad, None).unwrap_err();
    assert!(matches!(err, pakx_core::LockfileError::Schema { .. }));
}

#[test]
fn round_trips_sample_lockfile() {
    let original = sample_lockfile();
    let json = write_lockfile(&original);
    let reparsed = parse_lockfile(&json, None).unwrap();
    // The writer sorts agents and dependencies, so direct equality holds
    // only after the same sort is applied to the original.
    let mut sorted = original;
    for entry in sorted.entries.values_mut() {
        entry.agents.sort_unstable();
        entry.dependencies.sort_unstable();
    }
    assert_eq!(reparsed, sorted);
}

#[test]
fn writer_output_is_deterministic() {
    let a = write_lockfile(&sample_lockfile());
    let b = write_lockfile(&sample_lockfile());
    assert_eq!(a, b);
}

#[test]
fn writer_sorts_entry_keys_alphabetically() {
    let out = write_lockfile(&sample_lockfile());
    let mcp_idx = out
        .find("mcp/smithery/github-mcp@0.5.1")
        .expect("mcp key present");
    let skills_idx = out
        .find("skills/anthropics/pdf@1.2.0")
        .expect("skills key present");
    assert!(
        mcp_idx < skills_idx,
        "BTreeMap should put `mcp/...` before `skills/...`"
    );
}

#[test]
fn writer_sorts_agents_and_dependencies_inside_entry() {
    let mut entries = BTreeMap::new();
    entries.insert(
        "skills/x/y@1.0.0".into(),
        LockEntry {
            name: "x/y".into(),
            kind: PackageType::Skills,
            version: "1.0.0".into(),
            resolved_from: "https://example.com".into(),
            registry: RegistrySource::Github,
            integrity: sample_integrity(),
            agents: vec![
                AgentId::new_unchecked("windsurf"),
                AgentId::new_unchecked("claude-code"),
                AgentId::new_unchecked("codex"),
            ],
            dependencies: vec!["skills/z/b@1.0.0".into(), "skills/z/a@1.0.0".into()],
        },
    );
    let lf = Lockfile {
        lockfile_version: LOCKFILE_VERSION,
        manifest_hash: sample_integrity(),
        entries,
    };
    let reparsed = parse_lockfile(&write_lockfile(&lf), None).unwrap();
    let entry = &reparsed.entries["skills/x/y@1.0.0"];
    let agents: Vec<&str> = entry.agents.iter().map(AgentId::as_str).collect();
    assert_eq!(agents, vec!["claude-code", "codex", "windsurf"]);
    assert_eq!(
        entry.dependencies,
        vec!["skills/z/a@1.0.0".to_string(), "skills/z/b@1.0.0".into()]
    );
}

#[test]
fn writer_ends_with_single_trailing_newline() {
    let out = write_lockfile(&sample_lockfile());
    assert!(out.ends_with('\n'));
    assert!(!out.ends_with("\n\n"));
}
