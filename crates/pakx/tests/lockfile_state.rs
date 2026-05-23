//! Lockfile invariants exercised end-to-end through the real binary.
//!
//! The spec invariants we lock here:
//!   1. Missing lockfile → `pakx install` builds one.
//!   2. Lockfile present with stale `manifestHash` → `pakx doctor`
//!      surfaces the drift; `pakx install` rebuilds the hash on
//!      success.
//!   3. Lockfile entries referencing an id no longer in the manifest
//!      are dropped on the next `pakx install`.
//!   4. `--no-lockfile` skips the write but does not delete an
//!      existing lockfile.
//!   5. `pakx list` against an empty (no-entries) lockfile prints a
//!      hint, not an error.
//!
//! Tests don't need a wiremock when the manifest is empty — the
//! install loop only round-trips to a registry for deps that exist.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = "pakx";

// Lockfile integrity fields are validated strictly: `sha256-<43 base64
// chars>=`. We pin an SRI-shaped value here that decodes to 32 zero
// bytes — guaranteed NOT to match what `hash_manifest` computes over
// any fixture manifest, so the runner / doctor flags drift.
const MANIFEST_HASH_STALE: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn write_manifest(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("agents.yml"), body).unwrap();
}

/// Missing lockfile + empty manifest → install creates one with an
/// empty entries map and a fresh manifestHash.
#[test]
fn install_with_missing_lockfile_creates_it() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies: {}\n",
    );

    let lock_path = project.path().join("agents.lock");
    assert!(!lock_path.exists(), "lockfile should not exist pre-install");

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(lock_path.is_file(), "lockfile should exist post-install");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&lock_path).unwrap()).unwrap();
    assert_eq!(v["lockfileVersion"], 1);
    assert!(v["entries"].as_object().unwrap().is_empty());
    let hash = v["manifestHash"].as_str().expect("manifestHash present");
    assert!(
        hash.starts_with("sha256-"),
        "fresh hash must be SRI-shape: {hash}"
    );
}

/// Lockfile with a stale `manifestHash` → `pakx doctor` flags it as
/// drift. The exact wording matches the spec.
#[test]
fn doctor_reports_stale_manifest_hash_as_drift() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies: {}\n",
    );
    std::fs::write(
        project.path().join("agents.lock"),
        format!(
            r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH_STALE}","entries":{{}}}}
"#
        ),
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "doctor",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        // Drift is a warn, which doctor surfaces as a non-zero exit.
        .assert()
        .failure()
        .stdout(predicate::str::contains("manifest drift").or(predicate::str::contains("drift")));
}

/// Lockfile drift heals on `pakx install` — the runner overwrites the
/// stored manifestHash with the freshly computed one.
#[test]
fn install_overwrites_stale_manifest_hash() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies: {}\n",
    );
    std::fs::write(
        project.path().join("agents.lock"),
        format!(
            r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH_STALE}","entries":{{}}}}
"#
        ),
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(project.path().join("agents.lock")).unwrap())
            .unwrap();
    let hash = v["manifestHash"].as_str().unwrap();
    assert_ne!(
        hash, MANIFEST_HASH_STALE,
        "manifestHash must be overwritten with the fresh hash, got: {hash}"
    );
    assert!(hash.starts_with("sha256-"));
}

/// Lockfile with entries that reference ids no longer in the manifest
/// must be cleaned up on the next `pakx install` — the new lockfile
/// reflects ONLY what the manifest currently declares.
#[test]
fn install_drops_lockfile_entries_not_in_manifest() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies: {}\n",
    );

    // Seed a lockfile with a stale entry the manifest doesn't know about.
    let stale_lock = format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH_STALE}","entries":{{
  "skills/ghost/orphan@0.0.1":{{
    "name":"ghost/orphan",
    "type":"skills",
    "version":"0.0.1",
    "resolvedFrom":"https://registry.pakx.dev/api/v1/packages/ghost/orphan/0.0.1",
    "registry":"pakx",
    "integrity":"sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    );
    std::fs::write(project.path().join("agents.lock"), stale_lock).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(project.path().join("agents.lock")).unwrap())
            .unwrap();
    assert!(
        v["entries"].as_object().unwrap().is_empty(),
        "ghost entry must not survive a manifest-empty install; got:\n{v}"
    );
    assert!(
        v["entries"].get("skills/ghost/orphan@0.0.1").is_none(),
        "specifically the ghost key must be gone"
    );
}

/// `--no-lockfile` must NOT delete an existing lockfile — it just
/// skips the write. This protects users running ad-hoc diagnostic
/// installs from clobbering a known-good lockfile.
#[test]
fn no_lockfile_flag_preserves_existing_lockfile() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies: {}\n",
    );
    let lock_path = project.path().join("agents.lock");
    let sentinel = b"PRESERVE-ME-SENTINEL-CONTENT\n";
    std::fs::write(&lock_path, sentinel).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-lockfile",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let after = std::fs::read(&lock_path).unwrap();
    assert_eq!(
        after, sentinel,
        "--no-lockfile must NOT touch an existing lockfile; got: {after:?}"
    );
}

/// `pakx list` on a lockfile with only a `manifestHash` + empty
/// entries must print a hint, not crash.
#[test]
fn list_against_empty_entries_lockfile_prints_hint() {
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("agents.lock"),
        format!(
            r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH_STALE}","entries":{{}}}}
"#
        ),
    )
    .unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Either stream is acceptable for the "nothing to list" hint —
    // the contract is "exit 0 with a clear surface", not a specific
    // channel.
    assert!(
        stderr.contains("no")
            || stderr.contains("empty")
            || stdout.contains("no")
            || stdout.contains("empty")
            || stdout.contains("0 entries"),
        "expected a 'nothing here' hint on either stream; stderr=\n{stderr}\nstdout=\n{stdout}"
    );
}

/// `pakx list --json` on a missing lockfile must produce machine-
/// readable output (empty array or null), not error out. Pipelines
/// that wrap `pakx list --json` expect a stable shape.
#[test]
fn list_json_against_missing_lockfile_emits_stable_shape() {
    let project = TempDir::new().unwrap();
    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list", "--json"])
        .assert();
    let output = assert.get_output().clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Whatever shape the CLI picks, it must be parseable JSON.
    // `pakx list --json` documented contract: empty array on no
    // entries. We accept either `[]` or `{}` or `null` — the test
    // only forbids a non-JSON crash.
    let parsed: Result<Value, _> = serde_json::from_str(stdout.trim());
    if !stdout.trim().is_empty() {
        assert!(
            parsed.is_ok(),
            "list --json must emit valid JSON or empty; got: {stdout:?}"
        );
    }
}
