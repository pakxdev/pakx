//! Integration tests for `pakx manifest get/set/delete`.
//!
//! Each scenario writes a fixture `agents.yml`, drives the real built
//! `pakx` binary via `assert_cmd`, and asserts the stdout / file state.
//! `--manifest <path>` points the CLI at the tempdir manifest so the
//! tests are isolated from the runner's cwd.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value as JsonValue;
use tempfile::TempDir;

const BIN: &str = "pakx";

const FIXTURE: &str = "name: demo\nversion: 0.1.0\ndescription: a demo project\ndependencies:\n  skills:\n    - alice/bob@0.1.0\n    - carol/dave\n  mcp:\n    - registry: official\n      name: filesystem\n";

fn seed(temp: &TempDir) -> std::path::PathBuf {
    let path = temp.path().join("agents.yml");
    std::fs::write(&path, FIXTURE).unwrap();
    path
}

// ----- get ----------------------------------------------------------

#[test]
fn manifest_get_prints_top_level_string_unquoted() {
    let temp = TempDir::new().unwrap();
    let manifest_path = seed(&temp);

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "get",
            "description",
        ])
        .assert()
        .success()
        // Unquoted scalar — script-friendly.
        .stdout("a demo project\n");
}

#[test]
fn manifest_get_resolves_array_index() {
    let temp = TempDir::new().unwrap();
    let manifest_path = seed(&temp);

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "get",
            "dependencies.skills[0]",
        ])
        .assert()
        .success()
        .stdout("alice/bob@0.1.0\n");
}

#[test]
fn manifest_get_missing_path_under_json_prints_null_exits_one() {
    let temp = TempDir::new().unwrap();
    let manifest_path = seed(&temp);

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "get",
            "nope.missing",
            "--json",
        ])
        .assert()
        .code(1)
        // Stable `null` on stdout under --json so `jq` doesn't choke.
        .stdout("null\n")
        // Diagnostic on stderr.
        .stderr(predicate::str::contains("path not found"));
}

// ----- set ----------------------------------------------------------

#[test]
fn manifest_set_writes_string_value_atomically() {
    let temp = TempDir::new().unwrap();
    let manifest_path = seed(&temp);

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "set",
            "description",
            "rewritten",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("set"));

    let body = std::fs::read_to_string(&manifest_path).unwrap();
    // Parse + re-read so we don't depend on a specific YAML formatter
    // emit (key order, quote style).
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&body).unwrap();
    let desc = v
        .as_mapping()
        .and_then(|m| m.get(serde_yaml_ng::Value::String("description".into())))
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(desc, "rewritten");

    // Sanity: untouched fields remain.
    assert!(body.contains("alice/bob@0.1.0"));
    assert!(body.contains("filesystem"));
}

#[test]
fn manifest_set_json_accepts_array_value() {
    let temp = TempDir::new().unwrap();
    let manifest_path = seed(&temp);

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "set",
            "--json",
            "agents",
            "[\"claude-code\",\"cursor\"]",
        ])
        .assert()
        .success();

    let body = std::fs::read_to_string(&manifest_path).unwrap();
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&body).unwrap();
    let agents = v
        .as_mapping()
        .and_then(|m| m.get(serde_yaml_ng::Value::String("agents".into())))
        .and_then(|v| v.as_sequence())
        .unwrap();
    let labels: Vec<&str> = agents.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(labels, vec!["claude-code", "cursor"]);
}

#[test]
fn manifest_set_creates_intermediate_keys_for_deep_path() {
    let temp = TempDir::new().unwrap();
    let manifest_path = seed(&temp);

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "set",
            "metadata.repo.url",
            "https://example.test",
        ])
        .assert()
        .success();

    let body = std::fs::read_to_string(&manifest_path).unwrap();
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&body).unwrap();
    let url = v
        .as_mapping()
        .and_then(|m| m.get(serde_yaml_ng::Value::String("metadata".into())))
        .and_then(|m| m.as_mapping())
        .and_then(|m| m.get(serde_yaml_ng::Value::String("repo".into())))
        .and_then(|m| m.as_mapping())
        .and_then(|m| m.get(serde_yaml_ng::Value::String("url".into())))
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(url, "https://example.test");
}

// ----- delete -------------------------------------------------------

#[test]
fn manifest_delete_removes_existing_array_element() {
    let temp = TempDir::new().unwrap();
    let manifest_path = seed(&temp);

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "delete",
            "dependencies.skills[0]",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed"));

    // The former [1] (`carol/dave`) is now [0]; the prior [0]
    // (`alice/bob@0.1.0`) is gone.
    let body = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(!body.contains("alice/bob@0.1.0"), "got:\n{body}");
    assert!(body.contains("carol/dave"), "got:\n{body}");
}

#[test]
fn manifest_delete_missing_path_is_idempotent_warning() {
    let temp = TempDir::new().unwrap();
    let manifest_path = seed(&temp);
    let before = std::fs::read_to_string(&manifest_path).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "delete",
            "nope.gone",
        ])
        .assert()
        .success() // Idempotent — exit 0.
        .stderr(predicate::str::contains("not present"));

    // File untouched — mtime stays stable so build systems don't see a
    // spurious change.
    let after = std::fs::read_to_string(&manifest_path).unwrap();
    assert_eq!(before, after);
}

// ----- get JSON round-trip ------------------------------------------

#[test]
fn manifest_get_json_emits_quoted_string_scalar() {
    // Locks in the `--json` contract: even for a plain string value,
    // the `--json` render quotes + escapes per JSON rules so callers
    // can pipe the output through `jq` without special-casing scalar
    // vs structured values.
    let temp = TempDir::new().unwrap();
    let manifest_path = seed(&temp);

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "get",
            "description",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: JsonValue = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(parsed, JsonValue::String("a demo project".into()));
}
