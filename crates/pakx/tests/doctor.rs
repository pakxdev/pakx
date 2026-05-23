//! Integration tests for `pakx doctor`.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = "pakx";

#[test]
fn doctor_fails_when_no_manifest() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "doctor",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("fail"));
}

#[test]
fn doctor_warns_when_manifest_but_no_lockfile() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["init", "--yes", "--name", "demo"])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "doctor",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure() // warn counts as a problem
        .stdout(predicate::str::contains("no agents.lock"));
}

#[test]
fn doctor_json_emits_structured_payload_and_routes_prose_to_stderr() {
    // Fresh project init (manifest, no lockfile yet) → doctor warns
    // about the missing lockfile. `--json` must:
    //   * keep stdout strictly the single-line JSON object,
    //   * mirror the human glyph trail on stderr,
    //   * exit 0 (warnings don't flip ok, only errors do),
    //   * include `checks.manifest.ok: true` + a warning string.
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["init", "--yes", "--name", "demo"])
        .assert()
        .success();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "doctor",
            "--json",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success() // warnings keep exit 0 in --json mode
        .get_output()
        .clone();

    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("stdout must be JSON");
    assert_eq!(parsed["ok"], Value::Bool(true));
    assert_eq!(parsed["checks"]["manifest"]["ok"], Value::Bool(true));
    assert_eq!(parsed["checks"]["lockfile"]["ok"], Value::Bool(false));
    assert!(parsed["errors"].as_array().unwrap().is_empty());
    let warnings = parsed["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("no agents.lock")),
        "expected lockfile warning in warnings[] — got {warnings:?}"
    );

    // Human-readable trail still goes somewhere (stderr now).
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("no agents.lock"),
        "expected human warning on stderr — got:\n{stderr}"
    );
}

#[test]
fn doctor_json_returns_exit_1_when_manifest_missing() {
    // No manifest at all: doctor records `errors` and `ok: false`, exits 1.
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "doctor",
            "--json",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .get_output()
        .clone();

    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("stdout must be JSON");
    assert_eq!(parsed["ok"], Value::Bool(false));
    assert_eq!(parsed["checks"]["manifest"]["ok"], Value::Bool(false));
    assert!(!parsed["errors"].as_array().unwrap().is_empty());
}

#[test]
fn doctor_passes_when_manifest_and_matching_lockfile_present() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    // Init manifest then `install` with no deps so the lockfile is fresh.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["init", "--yes", "--name", "ok"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "doctor",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("all checks passed"));
}
