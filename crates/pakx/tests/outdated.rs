//! Integration tests for `pakx outdated`.
//!
//! Each scenario writes a fixture `agents.lock`, mounts a `wiremock`
//! server with the expected federated-registry responses, and drives
//! the real built `pakx` binary through `assert_cmd`. Asserts:
//!   - exit code 0 when up-to-date, 1 when anything is outdated.
//!   - human + `--json` output shapes.
//!   - registry-unreachable case surfaces on stderr but doesn't fail.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

const MANIFEST_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const ENTRY_INTEGRITY: &str = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

/// Build a lockfile body with one pakx-registry skill entry pinned at
/// `version`. The id is fixed at `arwenizEr/hello-world` to match the
/// live-smoke smoke target.
fn pakx_lockfile(version: &str) -> String {
    let hash = MANIFEST_HASH;
    let integ = ENTRY_INTEGRITY;
    format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{hash}","entries":{{
  "skills/arwenizEr/hello-world@{version}":{{
    "name":"arwenizEr/hello-world",
    "type":"skills",
    "version":"{version}",
    "resolvedFrom":"https://registry.pakx.dev/api/v1/packages/arwenizEr/hello-world/{version}",
    "registry":"pakx",
    "integrity":"{integ}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    )
}

/// Build a lockfile body with one official-mcp entry pinned at
/// `version`.
fn mcp_lockfile(id: &str, version: &str) -> String {
    let hash = MANIFEST_HASH;
    let integ = ENTRY_INTEGRITY;
    format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{hash}","entries":{{
  "mcp/{id}@{version}":{{
    "name":"{id}",
    "type":"mcp",
    "version":"{version}",
    "resolvedFrom":"official-mcp:{id}",
    "registry":"official-mcp",
    "integrity":"{integ}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    )
}

/// Build a multi-entry lockfile with three rows: one pakx-skill, one
/// MCP entry, one second pakx-skill the registry will 404 on. Pinned
/// versions are passed in so each scenario can drive a different
/// outcome from the same shape.
fn three_entry_lockfile(
    pakx_v: &str,
    mcp_id: &str,
    mcp_v: &str,
    missing_id: &str,
    missing_v: &str,
) -> String {
    let hash = MANIFEST_HASH;
    let integ = ENTRY_INTEGRITY;
    format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{hash}","entries":{{
  "skills/arwenizEr/hello-world@{pakx_v}":{{
    "name":"arwenizEr/hello-world",
    "type":"skills",
    "version":"{pakx_v}",
    "resolvedFrom":"https://registry.pakx.dev/api/v1/packages/arwenizEr/hello-world/{pakx_v}",
    "registry":"pakx",
    "integrity":"{integ}",
    "agents":["claude-code"],
    "dependencies":[]
  }},
  "mcp/{mcp_id}@{mcp_v}":{{
    "name":"{mcp_id}",
    "type":"mcp",
    "version":"{mcp_v}",
    "resolvedFrom":"official-mcp:{mcp_id}",
    "registry":"official-mcp",
    "integrity":"{integ}",
    "agents":["claude-code"],
    "dependencies":[]
  }},
  "skills/{missing_id}@{missing_v}":{{
    "name":"{missing_id}",
    "type":"skills",
    "version":"{missing_v}",
    "resolvedFrom":"https://registry.pakx.dev/api/v1/packages/{missing_id}/{missing_v}",
    "registry":"pakx",
    "integrity":"{integ}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    )
}

/// Mount `GET /api/v1/packages/arwenizEr/hello-world` returning a
/// detail response with the given `versions[]` (highest → lowest).
async fn mount_pakx_detail(server: &MockServer, versions: &[(&str, bool)]) {
    let versions: Vec<Value> = versions
        .iter()
        .map(|(v, deprecated)| {
            json!({
                "version": v,
                "sha256": "0".repeat(64),
                "sizeBytes": 1024,
                "publishedAt": "2026-05-22T00:00:00Z",
                "deprecatedAt": if *deprecated { Some("2026-05-23T00:00:00Z") } else { None },
            })
        })
        .collect();
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/arwenizEr/hello-world"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "arwenizEr/hello-world",
            "kind": "skills",
            "description": "outdated e2e",
            "versions": versions,
        })))
        .mount(server)
        .await;
}

/// Mount `GET /v0/servers?search=<id>` returning a single entry at
/// `version`. The detail endpoint 404s to mirror the 2025-12-11
/// upstream schema where per-server detail was dropped.
async fn mount_mcp_search(server: &MockServer, id: &str, version: &str) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "name": id,
                    "description": "outdated mcp e2e",
                    "version_detail": { "version": version },
                    "packages": []
                }
            ]
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn outdated_without_lockfile_prints_hint() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no agents.lock"));
}

/// `pakx outdated --help` must document the exit-code contract,
/// including the "no lockfile" 0-exit clarification (round 39). Pin
/// the language so a future help rewrite that drops the clarification
/// trips this test loudly.
#[test]
fn outdated_help_documents_no_lockfile_exit_code() {
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args(["outdated", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();
    assert!(
        help.contains("no lockfile") || help.contains("no drift can exist"),
        "outdated --help should call out the no-lockfile exit-code contract: {help}",
    );
}

#[tokio::test]
async fn outdated_empty_lockfile_prints_hint() {
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("agents.lock"),
        format!(
            r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{}}}}
"#
        ),
    )
    .unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no entries"));
}

#[tokio::test]
async fn outdated_exits_zero_when_pakx_entry_matches_registry_latest() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false)]).await;
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.2")).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated", "--pakx-base-url", &pakx_registry.uri()])
        .assert()
        .success()
        .stderr(predicate::str::contains("up to date"));
}

#[tokio::test]
async fn outdated_exits_one_when_pakx_entry_is_outdated() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    // Two versions, 0.1.2 latest non-deprecated.
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.0")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated", "--pakx-base-url", &pakx_registry.uri()])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("arwenizEr/hello-world"),
        "stdout must list outdated id; got:\n{stdout}"
    );
    assert!(
        stdout.contains("0.1.0") && stdout.contains("0.1.2"),
        "stdout must show current + latest; got:\n{stdout}"
    );
    assert!(
        stdout.contains("upgrade"),
        "stdout must mark status=upgrade; got:\n{stdout}"
    );
}

#[tokio::test]
async fn outdated_skips_deprecated_versions_when_picking_latest() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    // Newest version is deprecated → outdated must NOT recommend it.
    // 0.1.1 is the latest non-deprecated; the lockfile pins 0.1.0,
    // so the upgrade target is 0.1.1, not 0.1.2.
    mount_pakx_detail(
        &pakx_registry,
        &[("0.1.2", true), ("0.1.1", false), ("0.1.0", false)],
    )
    .await;
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.0")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "outdated",
            "--pakx-base-url",
            &pakx_registry.uri(),
            "--json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Value = serde_json::from_str(stdout.trim()).expect("json parses");
    let row = &arr.as_array().expect("array")[0];
    assert_eq!(row["latest"], "0.1.1", "deprecated 0.1.2 must be skipped");
    assert_eq!(row["status"], "upgrade");
}

#[tokio::test]
async fn outdated_json_emits_stable_field_names() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.0")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "outdated",
            "--pakx-base-url",
            &pakx_registry.uri(),
            "--json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.ends_with('\n'), "stdout must end with newline");
    let body = stdout.trim_end_matches('\n');
    assert!(!body.contains('\n'), "json output must be single-line");
    let arr: Value = serde_json::from_str(body).expect("json parses");
    let rows = arr.as_array().expect("top-level array");
    assert_eq!(rows.len(), 1);
    let row = rows[0].as_object().expect("row is object");
    for field in ["id", "current", "latest", "registry", "status"] {
        assert!(
            row.contains_key(field),
            "outdated --json row missing field {field}; got: {row:?}"
        );
    }
    assert_eq!(row["id"], "arwenizEr/hello-world");
    assert_eq!(row["current"], "0.1.0");
    assert_eq!(row["latest"], "0.1.2");
    assert_eq!(row["registry"], "pakx");
    assert_eq!(row["status"], "upgrade");
}

#[tokio::test]
async fn outdated_json_empty_array_when_all_up_to_date() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false)]).await;
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.2")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "outdated",
            "--pakx-base-url",
            &pakx_registry.uri(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim_end(), "[]");
}

#[tokio::test]
async fn outdated_registry_error_does_not_fail_command() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    // No mounts at all → every GET 404s. The check must surface the
    // error on stderr but still exit 0 (no actionable rows).
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.0")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated", "--pakx-base-url", &pakx_registry.uri()])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("arwenizEr/hello-world"),
        "stderr must surface registry error reason; got:\n{stderr}"
    );
}

#[tokio::test]
async fn outdated_registry_filter_restricts_to_one_source() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    let mcp = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;
    let mcp_id = "io.github.acme/cool";
    mount_mcp_search(&mcp, mcp_id, "1.3.0").await;
    // Three-entry lockfile: pakx-outdated, mcp-outdated, pakx-missing.
    std::fs::write(
        project.path().join("agents.lock"),
        three_entry_lockfile("0.1.0", mcp_id, "1.2.0", "ghost/unknown", "9.9.9"),
    )
    .unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "outdated",
            "--pakx-base-url",
            &pakx_registry.uri(),
            "--mcp-base-url",
            &mcp.uri(),
            "--registry",
            "official-mcp",
            "--json",
        ])
        .assert()
        // mcp dep alone is outdated → exit 1.
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Value = serde_json::from_str(stdout.trim()).expect("json parses");
    let rows = rows.as_array().expect("array");
    // Only the mcp row should appear — pakx entries filtered out.
    assert_eq!(rows.len(), 1, "registry filter must drop other sources");
    assert_eq!(rows[0]["id"], mcp_id);
    assert_eq!(rows[0]["registry"], "official-mcp");
    assert_eq!(rows[0]["status"], "upgrade");
}

#[tokio::test]
async fn outdated_three_entry_mixed_outcome_exits_one() {
    // Covers the spec's "wiremock'd flow with 3 deps: one upgrade,
    // one up-to-date, one error" requirement in a single test.
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    let mcp = MockServer::start().await;
    // pakx: 0.1.0 pinned, 0.1.2 latest → upgrade.
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;
    let mcp_id = "io.github.acme/cool";
    // mcp: 1.2.0 pinned, 1.2.0 latest → up-to-date.
    mount_mcp_search(&mcp, mcp_id, "1.2.0").await;
    // ghost/unknown intentionally not mounted → error.
    std::fs::write(
        project.path().join("agents.lock"),
        three_entry_lockfile("0.1.0", mcp_id, "1.2.0", "ghost/unknown", "9.9.9"),
    )
    .unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "outdated",
            "--pakx-base-url",
            &pakx_registry.uri(),
            "--mcp-base-url",
            &mcp.uri(),
            "--json",
        ])
        .assert()
        // One actionable upgrade → exit 1.
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Value = serde_json::from_str(stdout.trim()).expect("json parses");
    let rows = rows.as_array().expect("array");
    // upgrade + error → 2 rows in JSON (up-to-date excluded by contract).
    assert_eq!(rows.len(), 2, "expected upgrade + error; got {rows:?}");
    let by_id: std::collections::HashMap<&str, &Value> = rows
        .iter()
        .filter_map(|r| r["id"].as_str().map(|id| (id, r)))
        .collect();
    let pakx = by_id
        .get("arwenizEr/hello-world")
        .expect("pakx row present");
    assert_eq!(pakx["status"], "upgrade");
    let ghost = by_id.get("ghost/unknown").expect("error row present");
    assert_eq!(ghost["status"], "error");
}

#[tokio::test]
async fn outdated_marks_lower_registry_version_as_drift() {
    // Lockfile pins 0.1.5 but the registry only knows about 0.1.2 —
    // the published version regressed (or the lockfile holds a stale
    // pin against an unpublished version). The spec calls for `drift`
    // on this exact case.
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false)]).await;
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.5")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "outdated",
            "--pakx-base-url",
            &pakx_registry.uri(),
            "--json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Value = serde_json::from_str(stdout.trim()).expect("json parses");
    let row = &arr.as_array().expect("array")[0];
    assert_eq!(row["status"], "drift");
    assert_eq!(row["current"], "0.1.5");
    assert_eq!(row["latest"], "0.1.2");
}

#[tokio::test]
async fn outdated_mcp_only_lockfile_resolves_via_official_mcp() {
    let project = TempDir::new().unwrap();
    let mcp = MockServer::start().await;
    let id = "io.github.acme/cool";
    mount_mcp_search(&mcp, id, "1.3.0").await;
    std::fs::write(
        project.path().join("agents.lock"),
        mcp_lockfile(id, "1.2.0"),
    )
    .unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated", "--mcp-base-url", &mcp.uri(), "--json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Value = serde_json::from_str(stdout.trim()).expect("json parses");
    let row = &arr.as_array().expect("array")[0];
    assert_eq!(row["id"], id);
    assert_eq!(row["current"], "1.2.0");
    assert_eq!(row["latest"], "1.3.0");
    assert_eq!(row["registry"], "official-mcp");
    assert_eq!(row["status"], "upgrade");
}
