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
        .stderr(predicate::str::contains(
            "no `skills` entry named `a/b` in agents.yml",
        ));
}

/// Cross-command parity: `pakx update --kind` and `pakx remove --kind`
/// both surface the same "no `<kind>` entry named `<id>` in agents.yml"
/// message when the requested kind doesn't actually hold the id.
/// Locks the wording so the error stays predictable across the two
/// commands.
#[test]
fn remove_kind_flag_error_matches_update_kind_flag_wording() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - alpha/dep\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "alpha/dep", "--kind", "skills", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no `skills` entry named `alpha/dep` in agents.yml",
        ));

    // Manifest must be unchanged — the failure is at lookup time,
    // before any rewrite happens.
    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert_eq!(m.dependencies.mcp.as_ref().unwrap().len(), 1);
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

/// Regression for the non-TTY hang: `pakx remove <id>` WITHOUT `--yes`
/// used to call `inquire::Confirm` unconditionally, which blocks forever
/// reading stdin when there is no terminal (CI / a piped shell). It must
/// instead fail fast with an actionable "not a TTY" hint. `assert_cmd`
/// runs the child with a non-TTY stdin by default, so this exercises the
/// non-interactive branch and returns immediately rather than hanging.
#[test]
fn remove_without_yes_and_no_tty_bails_instead_of_hanging() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - a/b\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        // Empty stdin → closed pipe, not a TTY. The guard must fire
        // before any blocking read.
        .write_stdin("")
        .args(["remove", "a/b"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("stdin is not a TTY"))
        .stderr(predicate::str::contains("--yes"));

    // The manifest must be untouched — the bail happens before any write.
    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert_eq!(m.dependencies.mcp.as_ref().unwrap().len(), 1);
}

/// Removing the LAST dependency under a kind must not leave a dangling
/// empty `mcp:` key that would later break `pakx test` validation. The
/// core `remove_shorthand` drops the now-empty section, so the rewritten
/// `agents.yml` parses clean. We prove it end-to-end by running
/// `pakx test --offline` on the result and asserting it passes.
#[test]
fn remove_last_dep_in_kind_leaves_manifest_that_pakx_test_accepts() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - only/one\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "only/one", "--yes"])
        .assert()
        .success();

    // No dangling `mcp:` key in the serialized YAML.
    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    assert!(
        !body.contains("mcp:"),
        "empty kind key must be dropped, not left dangling; got:\n{body}"
    );
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.mcp.is_none());

    // The rewritten manifest must still validate. `--offline` skips the
    // registry round-trip; with no deps left there is nothing to resolve
    // and `pakx test` exits clean.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["test", "--offline"])
        .assert()
        .success();
}

/// Regression for the 2026-05-23 stdout/stderr alignment: the success
/// line must remain on **stdout** (the pre-existing behaviour) and the
/// `→ next: pakx install` hint must move to **stderr** so a script
/// piping `pakx remove ... | grep removed` doesn't pick up the hint
/// alongside the success line.
#[test]
fn remove_routes_success_line_to_stdout_and_hint_to_stderr() {
    let temp = TempDir::new().unwrap();
    // TWO deps so one remains after removal — the `→ next: pakx install`
    // hint is only printed while the manifest still has dependencies
    // left to reconcile (see the zero-deps companion below).
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/cool\n    - io.github.acme/keep\n",
    );
    let assertion = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "io.github.acme/cool", "--yes"])
        .assert()
        .success();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        stdout.contains("removed io.github.acme/cool (mcp)"),
        "success line must be on stdout; got stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("\u{2192} next: pakx install"),
        "→ next hint must NOT remain on stdout; got stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("\u{2192} next: pakx install"),
        "→ next hint must be on stderr; got stderr:\n{stderr}"
    );
}

/// When removing the LAST dependency leaves the manifest empty, the
/// `→ next: pakx install` hint must be suppressed — there is nothing
/// left for `pakx install` to reconcile, so the hint would only send the
/// user to a no-op and read as if more work remained.
#[test]
fn remove_suppresses_install_hint_when_no_deps_remain() {
    let temp = TempDir::new().unwrap();
    write_manifest(
        temp.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/cool\n",
    );
    let assertion = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["remove", "io.github.acme/cool", "--yes"])
        .assert()
        .success();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    // Removal itself still reported.
    assert!(
        stdout.contains("removed io.github.acme/cool (mcp)"),
        "success line must be on stdout; got stdout:\n{stdout}"
    );
    // Hint suppressed on BOTH streams now that the manifest is empty.
    assert!(
        !stdout.contains("\u{2192} next: pakx install"),
        "hint must be suppressed on stdout; got stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains("\u{2192} next: pakx install"),
        "hint must be suppressed on stderr when no deps remain; got stderr:\n{stderr}"
    );
}
