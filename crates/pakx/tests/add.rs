//! Integration tests for `pakx add`.

use assert_cmd::Command;
use pakx_core::parse_manifest;
use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

async fn mock_mcp_server_ok(id: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": id,
            "version_detail": { "version": "1.2.3" }
        })))
        .mount(&server)
        .await;
    server
}

async fn mock_mcp_server_404() -> MockServer {
    let server = MockServer::start().await;
    // Per-server detail endpoint: 404 to mimic the 2025-12-11 schema drop.
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    // Search fallback: empty hits so the client resolves cleanly to
    // `NotFound` instead of bubbling up a real HTTP failure.
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn add_writes_to_manifest_when_missing() {
    let temp = TempDir::new().unwrap();
    let mcp_id = "io.github.acme/cool-server";
    let server = mock_mcp_server_ok(mcp_id).await;

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args([
            "add",
            mcp_id,
            "--type",
            "mcp",
            "--mcp-base-url",
            &server.uri(),
        ])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    let mcp = m.dependencies.mcp.as_ref().expect("mcp list present");
    assert_eq!(mcp.len(), 1);
    assert!(body.contains(mcp_id), "body=\n{body}");
}

#[tokio::test]
async fn add_appends_to_existing_manifest() {
    let temp = TempDir::new().unwrap();
    // Seed with init.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "--yes", "--name", "seed"])
        .assert()
        .success();

    let server = mock_mcp_server_ok("a/b").await;
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args([
            "add",
            "a/b",
            "--type",
            "mcp",
            "--mcp-base-url",
            &server.uri(),
        ])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert_eq!(m.name, "seed", "init-seeded name preserved");
    assert_eq!(m.dependencies.mcp.unwrap().len(), 1);
}

#[tokio::test]
async fn add_idempotent_does_not_duplicate() {
    let temp = TempDir::new().unwrap();
    let server = mock_mcp_server_ok("a/b").await;

    for _ in 0..2 {
        Command::cargo_bin(BIN)
            .unwrap()
            .current_dir(temp.path())
            .args([
                "add",
                "a/b",
                "--type",
                "mcp",
                "--mcp-base-url",
                &server.uri(),
            ])
            .assert()
            .success();
    }

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert_eq!(m.dependencies.mcp.unwrap().len(), 1);
}

#[tokio::test]
async fn add_with_no_validate_skips_network() {
    let temp = TempDir::new().unwrap();
    // No mock server — would fail any network call.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["add", "a/b", "--type", "mcp", "--no-validate"])
        .assert()
        .success();
    assert!(temp.path().join("agents.yml").is_file());
}

#[tokio::test]
async fn add_warns_but_succeeds_when_id_not_in_registry() {
    let temp = TempDir::new().unwrap();
    let server = mock_mcp_server_404().await;

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args([
            "add",
            "ghost/server",
            "--type",
            "mcp",
            "--mcp-base-url",
            &server.uri(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("not in the official MCP Registry"));

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    assert!(body.contains("ghost/server"));
}

#[test]
fn add_infers_skills_kind_from_id_shape() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["add", "anthropics/skills/pdf", "--no-validate"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(
        m.dependencies.skills.is_some(),
        "should classify as skill: body=\n{body}"
    );
    assert!(m.dependencies.mcp.is_none(), "should not be mcp");
}

#[test]
fn add_rejects_invalid_id_shape() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["add", "has spaces", "--no-validate"])
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// Dual positional form: `pakx add <kind> <id>`
// ---------------------------------------------------------------------------

/// Two-positional form `pakx add mcp foo/bar` must behave identically
/// to `pakx add foo/bar -t mcp`. This is the path users naturally try
/// because every other package manager works that way; pakx pre-#34
/// errored with `unexpected argument 'foo/bar'`.
#[tokio::test]
async fn add_dual_positional_mcp_form() {
    let temp = TempDir::new().unwrap();
    let server = mock_mcp_server_ok("foo/bar").await;
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["add", "mcp", "foo/bar", "--mcp-base-url", &server.uri()])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    let mcp = m.dependencies.mcp.expect("mcp list populated");
    assert_eq!(mcp.len(), 1);
    assert!(m.dependencies.skills.is_none());
}

/// Two-positional `pakx add skills <id>` must land in the skills
/// section, not the MCP one — proving the leading `<kind>` token
/// actually overrides the `infer_kind` heuristic.
#[test]
fn add_dual_positional_skills_form_routes_to_skills() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["add", "skills", "foo/bar", "--no-validate"])
        .assert()
        .success();

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(m.dependencies.skills.is_some(), "should route to skills");
    assert!(m.dependencies.mcp.is_none(), "should NOT route to mcp");
}

/// Mixing the two-positional form with `-t/--type` is ambiguous —
/// reject with a specific error so the user understands which input
/// to drop.
#[test]
fn add_dual_positional_with_type_flag_rejected() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["add", "mcp", "foo/bar", "--type", "skills", "--no-validate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("kind specified twice"));
}

/// First positional must be a valid kind token in the two-positional
/// form, otherwise we'd silently treat junk as the id.
#[test]
fn add_dual_positional_invalid_kind_rejected() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args(["add", "notakind", "foo/bar", "--no-validate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a valid kind"));
}

/// `--mcp-base-url` must vet via `validate_base_url` BEFORE the
/// validation probe fires. Mirrors `pakx install` / `pakx test` —
/// a userinfo-smuggled URL must never see an HTTP request, even
/// though the validation probe itself is anonymous.
#[test]
fn add_rejects_plaintext_http_mcp_base_url() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args([
            "add",
            "mcp",
            "io.github.acme/cool",
            "--mcp-base-url",
            "http://evil.com",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to use registry base URL",
        ));
}
