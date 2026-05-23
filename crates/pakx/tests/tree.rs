//! Integration tests for `pakx tree`.
//!
//! Drives the real built binary through `assert_cmd` against
//! hand-written `agents.lock` fixtures. Asserts the human output
//! shape (no empty-group headers, kind/registry tree) and the
//! `--json` contract (`{ kinds: { <kind>: { <registry>: [...] } } }`).

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = "pakx";

const MANIFEST_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const ENTRY_INTEGRITY: &str = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

fn write_lockfile(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("agents.lock"), body).unwrap();
}

/// Lockfile with one pakx-registry skill + one official-mcp mcp.
fn multi_kind_lockfile() -> String {
    format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{
  "skills/arwenizEr/hello-world@0.1.2":{{
    "name":"arwenizEr/hello-world",
    "type":"skills",
    "version":"0.1.2",
    "resolvedFrom":"pakx:arwenizEr/hello-world",
    "registry":"pakx",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":["claude-code"],
    "dependencies":[]
  }},
  "mcp/io.github.acme/cool@1.2.0":{{
    "name":"io.github.acme/cool",
    "type":"mcp",
    "version":"1.2.0",
    "resolvedFrom":"official-mcp:io.github.acme/cool",
    "registry":"official-mcp",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    )
}

/// Lockfile with an entry whose kind has no install adapter wired
/// (`subagents`). Used to verify the `skipped` adapter tag surfaces.
fn skipped_kind_lockfile() -> String {
    format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{
  "subagents/foo/bar@0.1.0":{{
    "name":"foo/bar",
    "type":"subagents",
    "version":"0.1.0",
    "resolvedFrom":"pakx:foo/bar",
    "registry":"pakx",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":[],
    "dependencies":[]
  }}
}}}}
"#
    )
}

#[test]
fn tree_without_lockfile_prints_hint() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["tree"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no agents.lock"));
}

#[test]
fn tree_empty_lockfile_emits_hint() {
    let project = TempDir::new().unwrap();
    write_lockfile(
        project.path(),
        &format!(r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{}}}}"#),
    );
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["tree"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no entries"));
}

#[test]
fn tree_renders_kinds_and_registries() {
    let project = TempDir::new().unwrap();
    write_lockfile(project.path(), &multi_kind_lockfile());
    // `--color never` so the heading helper doesn't paint and the
    // string-match below stays portable.
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["--color", "never", "tree"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    // Both kind groups appear, ordered per PACKAGE_TYPES (skills, mcp).
    assert!(
        stdout.contains("skills/"),
        "missing `skills/` header in {stdout}"
    );
    assert!(stdout.contains("mcp/"), "missing `mcp/` header in {stdout}");
    assert!(
        stdout.contains("pakx/"),
        "missing `pakx/` subgroup in {stdout}"
    );
    assert!(
        stdout.contains("official-mcp/"),
        "missing `official-mcp/` subgroup in {stdout}",
    );
    assert!(stdout.contains("arwenizEr/hello-world"));
    assert!(stdout.contains("io.github.acme/cool"));
    // Canonical kind order: skills before mcp.
    let skills_idx = stdout.find("skills/").unwrap();
    let mcp_idx = stdout.find("mcp/").unwrap();
    assert!(skills_idx < mcp_idx, "skills must render before mcp");
}

#[test]
fn tree_skips_empty_groups() {
    // Lockfile only has `subagents` — `skills/` and `mcp/` headers
    // must not appear even though they're earlier in the canonical
    // PACKAGE_TYPES order. Contract: empty group renders nothing.
    let project = TempDir::new().unwrap();
    write_lockfile(project.path(), &skipped_kind_lockfile());
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["--color", "never", "tree"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains("subagents/"));
    assert!(!stdout.contains("skills/"), "empty kind must not render");
    assert!(!stdout.contains("\nmcp/"), "empty kind must not render");
    assert!(stdout.contains("skipped"), "adapter status must surface");
}

#[test]
fn tree_json_emits_expected_shape() {
    let project = TempDir::new().unwrap();
    write_lockfile(project.path(), &multi_kind_lockfile());
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["tree", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    // Single-line JSON terminated with one newline — pipe-friendly.
    assert!(stdout.ends_with('\n'));
    let body = stdout.trim_end_matches('\n');
    assert!(!body.contains('\n'), "json output must be single-line");

    let parsed: Value = serde_json::from_str(body).expect("json parses");
    let kinds = &parsed["kinds"];
    let skills = &kinds["skills"]["pakx"];
    let skills_arr = skills.as_array().expect("skills/pakx is an array");
    assert_eq!(skills_arr.len(), 1);
    assert_eq!(skills_arr[0]["id"], "arwenizEr/hello-world");
    assert_eq!(skills_arr[0]["version"], "0.1.2");
    assert_eq!(skills_arr[0]["adapter"], "wired");

    let mcp = &kinds["mcp"]["official-mcp"];
    let mcp_arr = mcp.as_array().expect("mcp/official-mcp is an array");
    assert_eq!(mcp_arr.len(), 1);
    assert_eq!(mcp_arr[0]["id"], "io.github.acme/cool");
    assert_eq!(mcp_arr[0]["adapter"], "wired");
}

#[test]
fn tree_json_marks_unwired_adapters_skipped() {
    // Contract: kinds without an install adapter (`subagents` here)
    // surface `adapter: "skipped"` so JSON consumers can filter on it.
    let project = TempDir::new().unwrap();
    write_lockfile(project.path(), &skipped_kind_lockfile());
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["tree", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let entry = &parsed["kinds"]["subagents"]["pakx"][0];
    assert_eq!(entry["id"], "foo/bar");
    assert_eq!(entry["adapter"], "skipped");
}

#[test]
fn tree_json_empty_lockfile_emits_empty_kinds() {
    let project = TempDir::new().unwrap();
    write_lockfile(
        project.path(),
        &format!(r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{}}}}"#),
    );
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["tree", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let parsed: Value = serde_json::from_str(stdout.trim_end()).unwrap();
    assert_eq!(parsed, serde_json::json!({"kinds": {}}));
}
