//! Integration tests for `pakx list`.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = "pakx";

const ONE_ENTRY_LOCKFILE: &str = r#"{"lockfileVersion":1,"manifestHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","entries":{
  "mcp/io.github.acme/cool@1.2.3":{
    "name":"io.github.acme/cool",
    "type":"mcp",
    "version":"1.2.3",
    "resolvedFrom":"official-mcp:io.github.acme/cool",
    "registry":"official-mcp",
    "integrity":"sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=",
    "agents":["claude-code"],
    "dependencies":[]
  }
}}
"#;

#[test]
fn list_without_lockfile_prints_hint() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no agents.lock"));
}

#[test]
fn list_empty_lockfile() {
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("agents.lock"),
        r#"{"lockfileVersion":1,"manifestHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","entries":{}}
"#,
    )
    .unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list", "--no-check"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no entries"));
}

#[test]
fn list_shows_entries_from_lockfile() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("agents.lock"), ONE_ENTRY_LOCKFILE).unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list", "--no-check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("io.github.acme/cool"))
        .stdout(predicate::str::contains("1.2.3"));
}

#[test]
fn list_json_emits_valid_array_with_expected_keys() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("agents.lock"), ONE_ENTRY_LOCKFILE).unwrap();
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list", "--no-check", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    // Single line + trailing newline — pipes cleanly into jq.
    assert!(stdout.ends_with('\n'), "stdout must end with newline");
    let body = stdout.trim_end_matches('\n');
    assert!(!body.contains('\n'), "json output must be single-line");

    let parsed: Value = serde_json::from_str(body).expect("json parses");
    let arr = parsed.as_array().expect("top level is an array");
    assert_eq!(arr.len(), 1);
    let entry = &arr[0];
    assert_eq!(entry["id"], "io.github.acme/cool");
    assert_eq!(entry["version"], "1.2.3");
    assert_eq!(entry["type"], "mcp");
    assert_eq!(entry["registry"], "official-mcp");
    assert_eq!(entry["key"], "mcp/io.github.acme/cool@1.2.3");
    assert_eq!(entry["resolved_from"], "official-mcp:io.github.acme/cool");
    assert!(entry["integrity"].as_str().unwrap().starts_with("sha256-"));
    assert_eq!(entry["agents"], serde_json::json!(["claude-code"]));
    // status is `unknown` because we passed --no-check.
    assert_eq!(entry["status"], "unknown");
}

#[test]
fn list_json_without_lockfile_emits_empty_array() {
    let project = TempDir::new().unwrap();
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert_eq!(stdout.trim_end(), "[]");
    let parsed: Value = serde_json::from_str(stdout.trim_end()).unwrap();
    assert_eq!(parsed, serde_json::json!([]));
}

#[test]
fn list_json_empty_lockfile_emits_empty_array() {
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("agents.lock"),
        r#"{"lockfileVersion":1,"manifestHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","entries":{}}
"#,
    )
    .unwrap();
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list", "--no-check", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert_eq!(stdout.trim_end(), "[]");
}
