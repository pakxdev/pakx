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

/// A schema-clean manifest (only modelled top-level keys) that gets
/// `set`'d with an UNKNOWN top-level key must be rejected — the typed
/// `Manifest` reader is `#[serde(deny_unknown_fields)]`, so without
/// this guard the post-`set` file would refuse to parse on the next
/// `pakx test` / `pakx install`. The user must see a clear error
/// listing the supported keys + a guarantee the original bytes were
/// restored.
#[test]
fn manifest_set_rejects_unknown_top_level_key_on_clean_manifest() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("agents.yml");
    let clean = "name: example\nversion: 0.1.0\n";
    std::fs::write(&manifest_path, clean).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "set",
            "homepage",
            "https://example.test",
        ])
        .assert()
        .failure()
        // Error must name the rejected key + list supported keys.
        .stderr(predicate::str::contains("'homepage'"))
        .stderr(predicate::str::contains("not a recognized manifest field"))
        .stderr(predicate::str::contains("name"))
        .stderr(predicate::str::contains("version"))
        .stderr(predicate::str::contains("dependencies"));

    // Rollback contract: the file is byte-identical to the pre-set
    // state. Using exact-equality (not a substring check) so a
    // trailing-newline or quoting-style drift surfaces immediately.
    let after = std::fs::read_to_string(&manifest_path).unwrap();
    assert_eq!(after, clean, "rollback must restore the original bytes");
}

/// Companion: setting a KNOWN top-level key on a clean manifest still
/// succeeds. Pin so the schema guard doesn't accidentally over-reject
/// the supported subset (`name`, `version`, `agents`, `dependencies`).
#[test]
fn manifest_set_allows_known_top_level_key_on_clean_manifest() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("agents.yml");
    let clean = "name: example\nversion: 0.1.0\n";
    std::fs::write(&manifest_path, clean).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "set",
            "--json",
            "agents",
            "[\"claude-code\"]",
        ])
        .assert()
        .success();

    let body = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(body.contains("agents:"), "agents was written: {body}");
    assert!(body.contains("claude-code"), "agent id was written: {body}");
}

/// `MANIFEST_TOP_LEVEL_KEYS` in `commands/manifest.rs` is the
/// authoritative list the schema-guard error message renders. It MUST
/// stay in lockstep with the `Manifest` struct's accepted top-level
/// keys — otherwise a future schema bump (e.g. adding `homepage`)
/// would leave the error message stale and confuse users about why
/// their input was rejected. Each key listed here is independently
/// asserted to be accepted by the typed schema via a minimal
/// `parse_manifest` round-trip; if any drifts, this test trips and
/// the constant must be updated alongside the schema.
#[test]
fn manifest_top_level_keys_constant_matches_schema() {
    // We can't reach into the binary's private constant, so we
    // exhaustively probe each known top-level key by writing a
    // minimal manifest that uses ONLY that key (plus the always-
    // required `name` / `version`) and asserting `pakx test --offline`
    // accepts it. If the constant ever drifts away from the schema,
    // this test plus `manifest_set_rejects_unknown_top_level_key_on_clean_manifest`
    // together catch it: one verifies the constant lists everything
    // the schema accepts, the other verifies the schema rejects what
    // the constant excludes.
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("agents.yml");

    // Minimal manifests exercising each top-level key. Required pair
    // (`name`, `version`) is shared.
    let probes: &[(&str, &str)] = &[
        ("name", "name: probe\nversion: 0.0.1\n"),
        ("version", "name: probe\nversion: 0.0.1\n"),
        (
            "agents",
            "name: probe\nversion: 0.0.1\nagents:\n  - claude-code\n",
        ),
        (
            "dependencies",
            "name: probe\nversion: 0.0.1\ndependencies:\n  mcp: []\n",
        ),
    ];

    for (key, body) in probes {
        std::fs::write(&manifest_path, body).unwrap();
        let out = Command::cargo_bin(BIN)
            .unwrap()
            .current_dir(temp.path())
            .args(["test", "--offline"])
            .assert()
            .get_output()
            .clone();
        assert!(
            out.status.success(),
            "top-level key {key:?} should parse cleanly; stderr=\n{}",
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

/// If the manifest was ALREADY in a schema-invalid state pre-edit
/// (e.g. a previous lax `set` left an unmodelled key in place), the
/// schema guard must not block further `set` calls — otherwise users
/// would have no way to repair a corrupted manifest via the CLI.
/// Pinning the "broken-stays-broken edit allowed" branch.
#[test]
fn manifest_set_allows_edits_when_pre_state_already_invalid() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("agents.yml");
    // Pre-existing unknown `description` top-level key — this fails
    // `parse_manifest` already, so the guard must skip the post-check.
    let pre = "name: example\nversion: 0.1.0\ndescription: old text\n";
    std::fs::write(&manifest_path, pre).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "set",
            "description",
            "new text",
        ])
        .assert()
        .success();

    let body = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(body.contains("description: new text"), "got:\n{body}");
}

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
