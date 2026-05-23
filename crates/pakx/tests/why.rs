//! Integration tests for `pakx why`.
//!
//! Each scenario writes an `agents.yml` + `agents.lock` fixture and
//! drives the real built binary through `assert_cmd`. Asserts the
//! human + `--json` shapes, the multi-kind / `--kind` filter
//! behaviour, and the exit-code discipline (1 on miss in human mode,
//! 0 on miss in JSON mode — same pattern as `pakx outdated`).

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = "pakx";

const MANIFEST_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const ENTRY_INTEGRITY: &str = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

fn write_files(dir: &std::path::Path, manifest: &str, lockfile: &str) {
    std::fs::write(dir.join("agents.yml"), manifest).unwrap();
    std::fs::write(dir.join("agents.lock"), lockfile).unwrap();
}

fn skill_lockfile() -> String {
    format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{
  "skills/arwenizEr/hello-world@0.1.2":{{
    "name":"arwenizEr/hello-world",
    "type":"skills",
    "version":"0.1.2",
    "resolvedFrom":"pakx:arwenizEr/hello-world",
    "registry":"pakx",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    )
}

const SKILL_MANIFEST: &str = r"name: smoke
version: 0.0.0
dependencies:
  skills:
    - arwenizEr/hello-world@0.1.2
";

const MULTI_KIND_MANIFEST: &str = r"name: smoke
version: 0.0.0
dependencies:
  skills:
    - shared/dep@0.1.0
  mcp:
    - shared/dep@0.1.0
";

fn multi_kind_lockfile() -> String {
    format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{
  "skills/shared/dep@0.1.0":{{
    "name":"shared/dep",
    "type":"skills",
    "version":"0.1.0",
    "resolvedFrom":"pakx:shared/dep",
    "registry":"pakx",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":["claude-code"],
    "dependencies":[]
  }},
  "mcp/shared/dep@0.1.0":{{
    "name":"shared/dep",
    "type":"mcp",
    "version":"0.1.0",
    "resolvedFrom":"official-mcp:shared/dep",
    "registry":"official-mcp",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    )
}

#[test]
fn why_renders_found_dep_with_full_context() {
    let project = TempDir::new().unwrap();
    write_files(project.path(), SKILL_MANIFEST, &skill_lockfile());
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["--color", "never", "why", "arwenizEr/hello-world"])
        .assert()
        .success()
        .stdout(predicate::str::contains("arwenizEr/hello-world"))
        .stdout(predicate::str::contains(
            "found in agents.yml under `skills:`",
        ))
        .stdout(predicate::str::contains("pinned in agents.lock at 0.1.2"))
        .stdout(predicate::str::contains(
            "registry: pakx (https://registry.pakx.dev/api/v1/packages/arwenizEr/hello-world)",
        ))
        .stdout(predicate::str::contains("adapter: wired (skills)"));
}

#[test]
fn why_with_version_suffix_still_resolves() {
    // Contract: typing `pakx why owner/name@0.1.2` strips the
    // version before matching against the manifest shorthand. This
    // pins that behaviour against accidental regressions in
    // `split_shorthand` callers.
    let project = TempDir::new().unwrap();
    write_files(project.path(), SKILL_MANIFEST, &skill_lockfile());
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["--color", "never", "why", "arwenizEr/hello-world@0.1.2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("found in agents.yml"));
}

#[test]
fn why_unknown_id_exits_non_zero_in_human_mode() {
    let project = TempDir::new().unwrap();
    write_files(project.path(), SKILL_MANIFEST, &skill_lockfile());
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["why", "missing/pkg"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn why_unknown_id_json_emits_empty_array_and_exits_zero() {
    // Mirrors the `pakx outdated --json` discipline: empty array,
    // exit 0 — `jq` pipelines don't break on a miss.
    let project = TempDir::new().unwrap();
    write_files(project.path(), SKILL_MANIFEST, &skill_lockfile());
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["why", "missing/pkg", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert_eq!(stdout.trim_end(), "[]");
}

#[test]
fn why_multi_kind_returns_every_match() {
    let project = TempDir::new().unwrap();
    write_files(project.path(), MULTI_KIND_MANIFEST, &multi_kind_lockfile());
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["why", "shared/dep", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected one match per kind");
    // Canonical order: skills before mcp (matches PACKAGE_TYPES).
    assert_eq!(arr[0]["kind"], "skills");
    assert_eq!(arr[1]["kind"], "mcp");
    assert_eq!(arr[0]["lockedVersion"], "0.1.0");
    assert_eq!(arr[1]["registry"], "official-mcp");
    // Only the pakx-registry row carries a `registryUrl` — the
    // official-mcp source has no per-package canonical URL exposed.
    assert!(arr[0]["registryUrl"].is_string());
    assert!(arr[1]["registryUrl"].is_null());
}

#[test]
fn why_kind_filter_narrows_to_one_section() {
    let project = TempDir::new().unwrap();
    write_files(project.path(), MULTI_KIND_MANIFEST, &multi_kind_lockfile());
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["why", "shared/dep", "--kind", "mcp", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["kind"], "mcp");
}

#[test]
fn why_works_without_manifest() {
    // Contract: a lockfile-only project (CI dropped `agents.yml` for
    // some reason) still answers `pakx why`. `manifestSource` is
    // null in JSON, the lockfile half of the row is intact.
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("agents.lock"), skill_lockfile()).unwrap();
    // Intentionally NO `agents.yml`.
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["why", "arwenizEr/hello-world", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert!(arr[0]["manifestSource"].is_null());
    assert_eq!(arr[0]["lockedVersion"], "0.1.2");
}
