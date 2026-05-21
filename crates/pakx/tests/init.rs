//! Integration tests for `pakx init`.

use assert_cmd::Command;
use pakx_core::parse_manifest;
use predicates::prelude::*;
use tempfile::TempDir;

const BIN: &str = "pakx";

#[test]
fn yes_creates_manifest_in_cwd() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "--yes"])
        .assert()
        .success();

    let path = temp.path().join("agents.yml");
    assert!(path.is_file(), "agents.yml should be written");
}

#[test]
fn yes_refuses_to_overwrite_without_force() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("agents.yml");
    std::fs::write(&path, "existing\n").unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    // Existing content untouched.
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, "existing\n");
}

#[test]
fn yes_force_overwrites() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("agents.yml");
    std::fs::write(&path, "existing\n").unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "--yes", "--force"])
        .assert()
        .success();

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("name:"),
        "rewritten with manifest, got: {after}"
    );
}

#[test]
fn name_flag_overrides_default() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "--yes", "--name", "custom-name"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    assert!(body.contains("name: custom-name"), "got: {body}");
}

#[test]
fn agent_flag_emits_agents_list() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args([
            "init",
            "--yes",
            "--name",
            "p",
            "--agent",
            "claude-code",
            "--agent",
            "cursor",
        ])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    assert!(body.contains("agents:"));
    assert!(body.contains("- claude-code"));
    assert!(body.contains("- cursor"));
}

#[test]
fn yes_without_agents_omits_agents_key() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "--yes", "--name", "p"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    assert!(
        !body.contains("agents:"),
        "missing agents key means 'all detected'"
    );
}

#[test]
fn written_manifest_parses_back_through_pakx_core() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args([
            "init",
            "--yes",
            "--name",
            "round-trip",
            "--manifest-version",
            "0.1.0",
        ])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let parsed = parse_manifest(&body, None).unwrap();
    assert_eq!(parsed.name, "round-trip");
    assert_eq!(parsed.version, "0.1.0");
}

#[test]
fn rejects_invalid_agent_id() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "--yes", "--agent", "1NotValid"])
        .assert()
        .failure();
}
