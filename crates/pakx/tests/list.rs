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

// Skill-shaped lockfile fixture for status tests: the adapter only
// reconciles entries it can see on disk under `skills/<owner>/<name>/`,
// so the `ok` / `drift` JSON contract is exercised against this shape.
const SKILLS_LOCKFILE: &str = r#"{"lockfileVersion":1,"manifestHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","entries":{
  "skills/anthropic/pdf@1.4.0":{
    "name":"anthropic/pdf",
    "type":"skills",
    "version":"1.4.0",
    "resolvedFrom":"pakx:anthropic/pdf",
    "registry":"pakx",
    "integrity":"sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=",
    "agents":["claude-code"],
    "dependencies":[]
  }
}}
"#;

/// Write a minimal `SKILL.md` under `<home>/skills/<owner>/<name>/` so
/// `ClaudeCodeAdapter::list` discovers it with the supplied version.
fn write_installed_skill(home: &std::path::Path, owner: &str, name: &str, version: &str) {
    let dir = home.join("skills").join(owner).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let body = format!("---\nname: {name}\nversion: {version}\n---\n# {name}\nbody\n");
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
}

#[test]
fn list_json_status_is_ok_when_installed_skill_matches_lockfile() {
    // Contract: `status` serializes as the exact string `"ok"` when the
    // adapter sees a matching skill on disk. Downstream pipelines key on
    // these strings — they are public contract, lock them in.
    let project = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    std::fs::write(project.path().join("agents.lock"), SKILLS_LOCKFILE).unwrap();
    write_installed_skill(home.path(), "anthropic", "pdf", "1.4.0");

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "list",
            "--json",
            "--claude-home",
            home.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["status"], "ok");
}

#[test]
fn list_json_status_is_drift_when_lockfile_pins_uninstalled_skill() {
    // Contract: `status` serializes as the exact string `"drift"` when
    // the lockfile pins an entry the adapter cannot find on disk.
    let project = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    std::fs::write(project.path().join("agents.lock"), SKILLS_LOCKFILE).unwrap();
    // Intentionally NO `write_installed_skill` — drift expected.
    // Touch `<home>/skills/` so the adapter walks an empty dir instead
    // of bailing on a missing root.
    std::fs::create_dir_all(home.path().join("skills")).unwrap();

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "list",
            "--json",
            "--claude-home",
            home.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["status"], "drift");
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
