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
