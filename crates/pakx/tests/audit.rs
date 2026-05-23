//! Integration tests for `pakx audit`.
//!
//! Mirrors the `pakx outdated` test layout — a fixture `agents.lock`
//! per scenario, a wiremock server mounting the per-version endpoint,
//! and the real built `pakx` binary driven through `assert_cmd`.
//! Asserts: exit code 0 when no deprecated entries, exit 1 when any
//! entry is `deprecated`; `--json` shape; registry filter; skip /
//! error behaviour.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

const MANIFEST_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const ENTRY_INTEGRITY: &str = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

/// One pakx-registry skill entry pinned at `version`.
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

/// Two-entry lockfile — one pakx, one official-mcp. Used to exercise
/// the skip-on-non-pakx-source path and the `--registry` filter.
fn mixed_lockfile(pakx_v: &str, mcp_id: &str, mcp_v: &str) -> String {
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
  }}
}}}}
"#
    )
}

/// Mount `GET /api/v1/packages/<owner>/<name>/<version>` returning a
/// per-version detail block. `deprecated_at` is the only field the
/// audit looks at; the rest are filled in to mirror a real backend
/// response.
async fn mount_pakx_version(
    server: &MockServer,
    owner: &str,
    name: &str,
    version: &str,
    deprecated_at: Option<&str>,
) {
    Mock::given(method("GET"))
        .and(wm_path(format!(
            "/api/v1/packages/{owner}/{name}/{version}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": format!("{owner}/{name}"),
            "version": version,
            "sha256": "0".repeat(64),
            "sizeBytes": 1024,
            "publishedAt": "2026-04-01T00:00:00Z",
            "deprecatedAt": deprecated_at,
            "tarballUrl": "https://blob.example.com/sig",
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn audit_without_lockfile_prints_hint() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["audit"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no agents.lock"));
}

#[tokio::test]
async fn audit_exits_zero_when_pakx_entry_is_active() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_version(&pakx_registry, "arwenizEr", "hello-world", "0.1.2", None).await;
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.2")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["audit", "--pakx-base-url", &pakx_registry.uri()])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("arwenizEr/hello-world"),
        "human table must list the audited id; got:\n{stdout}"
    );
    assert!(
        stdout.contains("ok"),
        "active version must surface as `ok`; got:\n{stdout}"
    );
}

#[tokio::test]
async fn audit_exits_one_when_pakx_entry_is_deprecated() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_version(
        &pakx_registry,
        "arwenizEr",
        "hello-world",
        "0.1.0",
        Some("2026-04-12T08:00:00Z"),
    )
    .await;
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.0")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["audit", "--pakx-base-url", &pakx_registry.uri()])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("deprecated"),
        "human table must mark status=deprecated; got:\n{stdout}"
    );
    assert!(
        stdout.contains("2026-04-12T08:00:00Z"),
        "human table must print deprecation timestamp; got:\n{stdout}"
    );
}

#[tokio::test]
async fn audit_skips_non_pakx_sources() {
    // official-mcp entries have no per-version deprecation signal —
    // audit must surface them as `skip`, never as `error`, and the
    // single deprecated pakx row still trips exit code 1.
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_version(
        &pakx_registry,
        "arwenizEr",
        "hello-world",
        "0.1.0",
        Some("2026-04-12T08:00:00Z"),
    )
    .await;
    std::fs::write(
        project.path().join("agents.lock"),
        mixed_lockfile("0.1.0", "io.github.acme/cool", "1.2.0"),
    )
    .unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["audit", "--pakx-base-url", &pakx_registry.uri(), "--json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Value = serde_json::from_str(stdout.trim()).expect("json parses");
    let rows = rows.as_array().expect("top-level array");
    assert_eq!(rows.len(), 2, "both entries must be in the JSON output");
    let by_id: std::collections::HashMap<&str, &Value> = rows
        .iter()
        .filter_map(|r| r["id"].as_str().map(|id| (id, r)))
        .collect();
    let pakx_row = by_id
        .get("arwenizEr/hello-world")
        .expect("pakx row present");
    assert_eq!(pakx_row["status"], "deprecated");
    assert_eq!(pakx_row["deprecatedAt"], "2026-04-12T08:00:00Z");
    let mcp_row = by_id.get("io.github.acme/cool").expect("mcp row present");
    assert_eq!(mcp_row["status"], "skip");
    assert!(
        mcp_row["deprecatedAt"].is_null(),
        "skip rows carry deprecatedAt=null: {mcp_row:?}"
    );
}

#[tokio::test]
async fn audit_registry_error_does_not_trip_exit_code() {
    // 404 on the per-version endpoint maps to `Status::Error`, surfaces
    // on stderr, and (per the documented contract) does NOT trip the
    // exit code — only `deprecated` does.
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    // No mounts → every GET 404s.
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.0")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["audit", "--pakx-base-url", &pakx_registry.uri()])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("arwenizEr/hello-world"),
        "stderr must surface the registry error reason; got:\n{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("error"),
        "table must mark the row as `error`; got:\n{stdout}"
    );
}

#[tokio::test]
async fn audit_json_emits_stable_field_names() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_version(
        &pakx_registry,
        "arwenizEr",
        "hello-world",
        "0.1.0",
        Some("2026-04-12T08:00:00Z"),
    )
    .await;
    std::fs::write(project.path().join("agents.lock"), pakx_lockfile("0.1.0")).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["audit", "--pakx-base-url", &pakx_registry.uri(), "--json"])
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
    for field in ["id", "version", "registry", "status", "deprecatedAt"] {
        assert!(
            row.contains_key(field),
            "audit --json row missing field {field}; got: {row:?}"
        );
    }
    assert_eq!(row["id"], "arwenizEr/hello-world");
    assert_eq!(row["version"], "0.1.0");
    assert_eq!(row["registry"], "pakx");
    assert_eq!(row["status"], "deprecated");
    assert_eq!(row["deprecatedAt"], "2026-04-12T08:00:00Z");
}

#[tokio::test]
async fn audit_registry_filter_restricts_to_one_source() {
    // `--registry pakx` must drop the mcp row — only the deprecated
    // pakx entry remains, and the exit code stays 1.
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_version(
        &pakx_registry,
        "arwenizEr",
        "hello-world",
        "0.1.0",
        Some("2026-04-12T08:00:00Z"),
    )
    .await;
    std::fs::write(
        project.path().join("agents.lock"),
        mixed_lockfile("0.1.0", "io.github.acme/cool", "1.2.0"),
    )
    .unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "audit",
            "--pakx-base-url",
            &pakx_registry.uri(),
            "--registry",
            "pakx",
            "--json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Value = serde_json::from_str(stdout.trim()).expect("json parses");
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 1, "registry filter must drop other sources");
    assert_eq!(rows[0]["id"], "arwenizEr/hello-world");
    assert_eq!(rows[0]["status"], "deprecated");
}

#[tokio::test]
async fn audit_json_empty_array_when_no_entries() {
    // Empty lockfile → empty JSON array, exit code 0. Same shape
    // contract as `pakx outdated --json`.
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("agents.lock"),
        format!(
            r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{}}}}
"#
        ),
    )
    .unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["audit", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim_end(), "[]");
}
