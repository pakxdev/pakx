//! Integration tests for `pakx doctor`.

use assert_cmd::Command;
use predicates::prelude::*;
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
