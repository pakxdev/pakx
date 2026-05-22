//! Integration tests for `pakx remove`.
//!
//! Mirrors the surface of `pakx add` and exercises the kind-inference,
//! ambiguity-rejection paths, `--directory` redirection, and the
//! round-trip property (read then mutate then write preserves remaining
//! entries in order).

use assert_cmd::Command;
use pakx_core::parse_manifest;
use predicates::prelude::*;
use tempfile::TempDir;

const BIN: &str = "pakx";

fn write_manifest(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("agents.yml"), body).unwrap();
}

#[test]
fn remove_drops_mcp_entry_from_manifest() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/cool\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "io.github.acme/cool", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "removed io.github.acme/cool (mcp)",
        ));

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.mcp.is_none(), "mcp section pruned");
}

#[test]
fn remove_drops_skill_entry_from_manifest() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - anthropics/skills/pdf\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "anthropics/skills/pdf", "--yes"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.skills.is_none());
}

#[test]
fn remove_drops_subagent_entry_from_manifest() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  subagents:\n    - acme/agent\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "acme/agent", "--yes"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.subagents.is_none());
}

#[test]
fn remove_drops_prompt_entry_from_manifest() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  prompts:\n    - acme/prompt\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "acme/prompt", "--yes"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.prompts.is_none());
}

#[test]
fn remove_drops_command_entry_from_manifest() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  commands:\n    - acme/cmd\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "acme/cmd", "--yes"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.commands.is_none());
}

#[test]
fn remove_drops_hook_entry_from_manifest() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  hooks:\n    - acme/hook\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "acme/hook", "--yes"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.hooks.is_none());
}

#[test]
fn remove_errors_when_id_missing_from_manifest() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - present/one\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "missing/two", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found in agents.yml"));
}

#[test]
fn remove_errors_on_ambiguous_id_without_kind_flag() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - shared/id\n  skills:\n    - shared/id\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "shared/id", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("declared under multiple sections"))
        .stderr(predicate::str::contains("--kind"));

    // Manifest must be unchanged on the ambiguity error.
    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert_eq!(m.dependencies.mcp.as_ref().unwrap().len(), 1);
    assert_eq!(m.dependencies.skills.as_ref().unwrap().len(), 1);
}

#[test]
fn remove_resolves_ambiguity_with_kind_mcp() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - shared/id\n  skills:\n    - shared/id\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "shared/id", "--kind", "mcp", "--yes"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.mcp.is_none(), "mcp section pruned");
    assert_eq!(
        m.dependencies.skills.as_ref().unwrap().len(),
        1,
        "other section untouched"
    );
}

#[test]
fn remove_errors_when_explicit_kind_does_not_match() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - a/b\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "a/b", "--kind", "skills", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not declared under skills"));
}

#[test]
fn remove_honours_directory_override() {
    let outer = TempDir::new().unwrap();
    let inner = outer.path().join("project");
    std::fs::create_dir_all(&inner).unwrap();
    write_manifest(
        &inner,
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - a/b\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(outer.path())
        .args(["remove", "a/b", "-C", inner.to_str().unwrap(), "--yes"])
        .assert()
        .success();

    let body = std::fs::read_to_string(inner.join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.mcp.is_none());
}

#[test]
fn remove_preserves_order_of_remaining_entries() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - first/one\n    - second/two\n    - third/three\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "second/two", "--yes"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    let names: Vec<String> = m
        .dependencies
        .mcp
        .as_ref()
        .unwrap()
        .iter()
        .map(|d| match d {
            pakx_core::DepSpec::String(s) => s.as_str().to_owned(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(names, vec!["first/one".to_string(), "third/three".into()]);
}

/// Round-trip property: `pakx add` → `pakx remove` on the same id
/// returns the manifest to the same parsed shape it had before `add`.
/// Locks in that the YAML re-serialisation doesn't drift on a no-op
/// (modulo formatting; we compare parsed structure, not bytes).
#[test]
fn remove_roundtrips_clean_after_add() {
    let temp = TempDir::new().unwrap();
    // Seed.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "--yes", "--name", "demo"])
        .assert()
        .success();
    let initial = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let initial_parsed = parse_manifest(&initial, None).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["add", "a/b", "--type", "mcp", "--no-validate"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "a/b", "--yes"])
        .assert()
        .success();

    let after = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let after_parsed = parse_manifest(&after, None).unwrap();
    assert_eq!(after_parsed, initial_parsed);
}

/// `--yes` is the non-interactive opt-out for the confirmation prompt.
/// Without it the run reads from stdin, which is hard to drive in
/// `assert_cmd` without a real PTY (`inquire` reads via crossterm).
/// We document that limitation here rather than skipping the
/// behaviour — manual smoke covers the prompt path.
#[test]
fn remove_yes_flag_skips_prompt() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - a/b\n",
    );

    // No stdin write needed because `--yes` short-circuits the prompt.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "a/b", "--yes"])
        .assert()
        .success();
}
