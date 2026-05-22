//! Integration tests for `pakx test`.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

fn write_manifest(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("agents.yml"), body).unwrap();
}

#[test]
fn test_fails_when_manifest_missing() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("read manifest"));
}

#[test]
fn test_offline_succeeds_with_no_mcp_deps() {
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "name: example\nversion: 0.1.0\n");
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("manifest"))
        .stdout(predicate::str::contains("parsed"))
        .stdout(predicate::str::contains("all entries ok"));
}

#[test]
fn test_offline_requires_lockfile_entry_for_each_mcp_dep() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/cool\n",
    );
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure()
        // status glyph + id; loosened from the legacy "fail: mcp/..."
        // prefix once `pakx test` switched to the project-wide
        // `[ok] / [fail]` glyphs.
        .stdout(predicate::str::contains("mcp/io.github.acme/cool"))
        .stdout(predicate::str::contains("[fail]"));
}

#[test]
fn test_offline_passes_with_matching_lockfile_entry() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/cool\n",
    );
    std::fs::write(
        project.path().join("agents.lock"),
        r#"{"lockfileVersion":1,"manifestHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","entries":{
  "mcp/io.github.acme/cool@1.2.3":{
    "name":"io.github.acme/cool",
    "type":"mcp",
    "version":"1.2.3",
    "resolvedFrom":"official-mcp:io.github.acme/cool",
    "registry":"official-mcp",
    "integrity":"sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=",
    "agents":["claude-code"],
    "dependencies":[]
  }
}}
"#,
    )
    .unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[ok]"))
        .stdout(predicate::str::contains("mcp/io.github.acme/cool"))
        .stdout(predicate::str::contains("all entries ok"));
}

#[test]
fn test_honors_manifest_override_flag() {
    let project = TempDir::new().unwrap();
    let alt = project.path().join("nested").join("agents-alt.yml");
    std::fs::create_dir_all(alt.parent().unwrap()).unwrap();
    std::fs::write(&alt, "name: alt\nversion: 0.2.0\n").unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--offline",
            "--manifest",
            alt.strip_prefix(project.path()).unwrap().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("name=alt"));
}

#[tokio::test]
async fn test_online_resolves_against_registry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "name": "io.github.acme/cool",
                    "description": "hit",
                    "version_detail": {"version": "1.0.0"}
                }
            ]
        })))
        .mount(&server)
        .await;
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/cool\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[ok]"))
        .stdout(predicate::str::contains("mcp/io.github.acme/cool"));
}

#[tokio::test]
async fn test_online_fails_on_unknown_dep() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&server)
        .await;
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/ghost\n",
    );
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("[fail]"))
        .stdout(predicate::str::contains(
            "mcp/io.github.acme/ghost not found",
        ));
}

/// Federated fallback: when the official MCP Registry has no match,
/// `pakx test` must consult Smithery and pakx-registry — and a hit
/// on either should resolve the dep as `ok`.
#[tokio::test]
async fn test_online_falls_back_to_pakx_registry() {
    let official = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&official)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&official)
        .await;

    let pakx = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packages": [
                { "id": "alice/cool", "kind": "mcp", "latestVersion": "1.0.0" }
            ]
        })))
        .mount(&pakx)
        .await;

    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - alice/cool\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &official.uri(),
            "--pakx-base-url",
            &pakx.uri(),
            "--no-smithery",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[ok]"))
        .stdout(predicate::str::contains("mcp/alice/cool"))
        // The status line must surface which source resolved the dep
        // so users see federated resolution actually happened.
        .stdout(predicate::str::contains("pakx:"));
}

/// Companion: `--no-pakx-registry` (with Smithery also off) re-breaks
/// the resolution. Documents that the flag is wired through.
#[tokio::test]
async fn test_online_with_no_pakx_registry_does_not_fall_back() {
    let official = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&official)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&official)
        .await;

    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - alice/cool\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &official.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("[fail]"))
        .stdout(predicate::str::contains("mcp/alice/cool"));
}

#[test]
fn test_exits_non_zero_on_malformed_yaml() {
    // README sells "exit non-zero on first failure" as the CI contract.
    // Verify that a syntactically broken `agents.yml` triggers the same
    // failure path as a registry resolution failure.
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "name: bad\nversion: [unterminated\n");
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("read manifest"));
}

#[test]
fn test_exits_non_zero_on_unknown_manifest_field() {
    // `Manifest` is `#[serde(deny_unknown_fields)]`. An unknown field
    // (typo'd key) must be rejected — that's the whole point of the
    // deny_unknown_fields contract.
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\nunknwn_field: oops\n",
    );
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("read manifest"));
}

#[test]
fn test_does_not_write_lockfile() {
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "name: example\nversion: 0.1.0\n");
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .success();
    assert!(
        !project.path().join("agents.lock").exists(),
        "pakx test must not write agents.lock"
    );
}
