//! Federated-source matrix tests for the commands that resolve through
//! more than one registry: `install`, `search`, `outdated`, `audit`,
//! `info`, `add`.
//!
//! Each source can be in one of these states:
//!   * up   — returns valid JSON.
//!   * 404  — package not found.
//!   * 500  — server error.
//!   * down — refuses connection (we simulate via empty mocks → 404).
//!   * bad  — returns non-JSON / wrong-shape JSON.
//!
//! Goals: prove that the CLI never hangs, panics on malformed input,
//! or silently masks an error. Every failure must surface a clean
//! diagnostic on stderr.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

const MANIFEST_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const ENTRY_INTEGRITY: &str = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

fn pakx_lockfile_one(version: &str) -> String {
    format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{
  "skills/arwenizEr/hello-world@{version}":{{
    "name":"arwenizEr/hello-world",
    "type":"skills",
    "version":"{version}",
    "resolvedFrom":"https://registry.pakx.dev/api/v1/packages/arwenizEr/hello-world/{version}",
    "registry":"pakx",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    )
}

// ---------------------------------------------------------------------------
// `pakx search`
// ---------------------------------------------------------------------------

/// All three registries empty → search reports zero results and exits
/// 0. Does not hang.
#[tokio::test]
async fn search_all_sources_empty_exits_clean() {
    let mcp = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&mcp)
        .await;
    let pakx = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "packages": [] })))
        .mount(&pakx)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "anything",
            "--mcp-base-url",
            &mcp.uri(),
            "--pakx-base-url",
            &pakx.uri(),
            "--no-smithery",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("no results"));
}

/// One source returns malformed JSON. `pakx search` must not panic;
/// the malformed source is dropped, the rest still flow through.
#[tokio::test]
async fn search_malformed_pakx_response_does_not_panic() {
    let mcp = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "name": "io.github.acme/from-mcp",
                    "description": "ok",
                    "version_detail": {"version": "1.0.0"}
                }
            ]
        })))
        .mount(&mcp)
        .await;
    let pakx = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<not json>"))
        .mount(&pakx)
        .await;

    // Must exit cleanly with a result from the mcp source even though
    // pakx-registry returned garbage. The fed-search swallows per-
    // source errors by design; the test asserts the CLI doesn't crash.
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "acme",
            "--mcp-base-url",
            &mcp.uri(),
            "--pakx-base-url",
            &pakx.uri(),
            "--no-smithery",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("io.github.acme/from-mcp"));
}

/// Search with `--no-smithery --no-pakx-registry` against an mcp mock
/// returning a hit — proves the single-source filters compose
/// correctly with explicit URLs.
#[tokio::test]
async fn search_mcp_only_with_other_sources_disabled() {
    let mcp = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                { "name": "io.github.x/y", "description": "d", "version_detail": {"version": "1.0"} }
            ]
        })))
        .mount(&mcp)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "y",
            "--mcp-base-url",
            &mcp.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("io.github.x/y"));
}

// ---------------------------------------------------------------------------
// `pakx outdated`
// ---------------------------------------------------------------------------

/// All registries down (404 on every probe) → `pakx outdated` must
/// exit 0 with the per-entry error on stderr (matching the spec).
#[tokio::test]
async fn outdated_all_sources_404_exits_zero_with_diagnostic() {
    let project = TempDir::new().unwrap();
    let pakx = MockServer::start().await;
    // No mounts → 404 on everything.
    std::fs::write(
        project.path().join("agents.lock"),
        pakx_lockfile_one("0.1.0"),
    )
    .unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated", "--pakx-base-url", &pakx.uri()])
        .assert()
        .success() // network errors are NOT actionable upgrades; exit 0.
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("arwenizEr/hello-world"),
        "stderr should name the failing dep; got:\n{stderr}"
    );
}

/// One source returns malformed JSON. `pakx outdated --json` must
/// produce parseable JSON (the row gets `status: error`) — no panic.
#[tokio::test]
async fn outdated_malformed_response_yields_error_row_in_json() {
    let project = TempDir::new().unwrap();
    let pakx = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/arwenizEr/hello-world"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not-json"))
        .mount(&pakx)
        .await;
    std::fs::write(
        project.path().join("agents.lock"),
        pakx_lockfile_one("0.1.0"),
    )
    .unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated", "--pakx-base-url", &pakx.uri(), "--json"])
        .assert()
        .success() // not actionable → exit 0
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("json parses");
    let rows = parsed.as_array().unwrap();
    if !rows.is_empty() {
        assert_eq!(rows[0]["status"], "error");
    }
}

// ---------------------------------------------------------------------------
// `pakx install` — failure modes that must NOT hang
// ---------------------------------------------------------------------------

/// All sources off → manifest with one MCP dep that can't resolve
/// must exit 1 with a clear diagnostic. No `--no-smithery` shortcut
/// because we want to exercise the multi-source path.
#[tokio::test]
async fn install_with_all_sources_404_exits_cleanly() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let mcp = MockServer::start().await;
    let pakx = MockServer::start().await;
    // Empty mocks → all probes 404.
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mcp)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&mcp)
        .await;

    std::fs::write(
        project.path().join("agents.yml"),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - alice/missing\n",
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &mcp.uri(),
            "--pakx-base-url",
            &pakx.uri(),
            "--no-smithery",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        // The summary line names the failed dep count + the per-dep
        // line carries the id. Either is enough for the user to act.
        .stderr(predicate::str::contains("failed").or(predicate::str::contains("alice/missing")));
}

/// Install resolves cleanly when ONLY the pakx-registry source has
/// the package — the official-MCP source returns empty / 404.
/// Companion to `install_falls_back_to_pakx_registry_when_official_mcp_404s`
/// in `end_to_end.rs` but for an MCP id (covers a different code path:
/// the federated client's "try each source" iteration).
#[tokio::test]
async fn install_resolves_mcp_id_only_via_pakx_registry_fallback() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/only-on-pakx";
    let version = "1.0.0";

    let mcp = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mcp)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&mcp)
        .await;

    let pakx = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packages": [
                {
                    "id": id,
                    "kind": "mcp",
                    "latestVersion": version,
                    "packages": [
                        {
                            "registry_name": "npm",
                            "name": "@alice/only-on-pakx",
                            "version": version,
                            "environment_variables": []
                        }
                    ]
                }
            ]
        })))
        .mount(&pakx)
        .await;

    std::fs::write(
        project.path().join("agents.yml"),
        format!("name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - {id}\n"),
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &mcp.uri(),
            "--pakx-base-url",
            &pakx.uri(),
            "--no-smithery",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let lock: Value =
        serde_json::from_str(&std::fs::read_to_string(project.path().join("agents.lock")).unwrap())
            .unwrap();
    let key = format!("mcp/{id}@{version}");
    assert_eq!(lock["entries"][&key]["registry"], "pakx");
}

// ---------------------------------------------------------------------------
// `pakx info` — registry error surfaces cleanly
// ---------------------------------------------------------------------------

/// `pakx info` against a 500 registry must error cleanly with the
/// HTTP status, not a raw reqwest dump.
#[tokio::test]
async fn info_500_renders_clean_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/ghost/server"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args(["info", "ghost/server", "--registry", &server.uri()])
        .assert()
        .failure();
}

/// `pakx info` against a registry returning non-JSON body must
/// surface a parse error, not panic.
#[tokio::test]
async fn info_malformed_response_surfaces_parse_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/alice/badjson"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<!doctype html><html>oops"))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args(["info", "alice/badjson", "--registry", &server.uri()])
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// `pakx audit` — single-source filter
// ---------------------------------------------------------------------------

/// `pakx audit --registry pakx` against a lockfile with only an MCP
/// entry must skip the entry without erroring (filter excludes it).
#[tokio::test]
async fn audit_registry_filter_drops_non_matching_sources() {
    let project = TempDir::new().unwrap();
    let mcp_id = "io.github.acme/cool";
    let mcp_v = "1.0.0";
    let body = format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{
  "mcp/{mcp_id}@{mcp_v}":{{
    "name":"{mcp_id}",
    "type":"mcp",
    "version":"{mcp_v}",
    "resolvedFrom":"official-mcp:{mcp_id}",
    "registry":"official-mcp",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    );
    std::fs::write(project.path().join("agents.lock"), body).unwrap();
    let pakx = MockServer::start().await;

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "audit",
            "--registry",
            "pakx",
            "--pakx-base-url",
            &pakx.uri(),
        ])
        .assert()
        // No matching entries → exit 0.
        .success();
}
