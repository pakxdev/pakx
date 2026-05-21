//! End-to-end tests for `pakx install` against wiremock + temp project root.

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

fn npm_stdio_server(id: &str, version: &str) -> Value {
    json!({
        "name": id,
        "description": "test mcp",
        "version_detail": { "version": version },
        "packages": [
            {
                "registry_name": "npm",
                "name": "@acme/mcp",
                "version": version,
                "package_arguments": [],
                "environment_variables": [
                    { "name": "API_KEY" }
                ]
            }
        ]
    })
}

async fn mock_registry(id: &str, version: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(npm_stdio_server(id, version)))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn install_resolves_mcp_dep_and_writes_lockfile_and_mcp_json() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "io.github.acme/cool";
    let server = mock_registry(id, "1.2.3").await;

    // Seed manifest.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["init", "--yes", "--name", "demo"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", id, "--type", "mcp", "--no-validate"])
        .assert()
        .success();

    // Install.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    // .mcp.json was written.
    let mcp_body = std::fs::read_to_string(project.path().join(".mcp.json")).unwrap();
    let mcp: Value = serde_json::from_str(&mcp_body).unwrap();
    assert_eq!(mcp["mcpServers"]["cool"]["command"], "npx");
    assert_eq!(mcp["mcpServers"]["cool"]["args"][1], "@acme/mcp");
    assert_eq!(mcp["mcpServers"]["cool"]["env"]["API_KEY"], "");

    // agents.lock was written.
    let lock_body = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let lock: Value = serde_json::from_str(&lock_body).unwrap();
    assert_eq!(lock["lockfileVersion"], 1);
    let key = format!("mcp/{id}@1.2.3");
    let entry = &lock["entries"][&key];
    assert_eq!(entry["name"], id);
    assert_eq!(entry["version"], "1.2.3");
    assert_eq!(entry["registry"], "official-mcp");
    assert!(entry["integrity"].as_str().unwrap().starts_with("sha256-"));
    assert_eq!(entry["agents"][0], "claude-code");
}

#[tokio::test]
async fn install_idempotent_second_run_marks_as_skipped() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "io.github.acme/idem";
    let server = mock_registry(id, "1.0.0").await;

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["init", "--yes", "--name", "demo"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", id, "--type", "mcp", "--no-validate"])
        .assert()
        .success();

    let run_install = || {
        Command::cargo_bin(BIN)
            .unwrap()
            .current_dir(project.path())
            .args([
                "install",
                "--mcp-base-url",
                &server.uri(),
                "--claude-home",
                claude_home.path().to_str().unwrap(),
            ])
            .assert()
            .success()
            .get_output()
            .clone()
    };

    let first = run_install();
    let second = run_install();
    let second_stderr = String::from_utf8_lossy(&second.stderr).into_owned();
    let first_stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(
        first_stderr.contains("installed:"),
        "first stderr=\n{first_stderr}"
    );
    assert!(
        second_stderr.contains("skipped"),
        "second stderr=\n{second_stderr}"
    );
}

#[tokio::test]
async fn install_fails_when_registry_returns_404() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["init", "--yes", "--name", "demo"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", "missing/server", "--type", "mcp", "--no-validate"])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
}

#[tokio::test]
async fn install_with_no_lockfile_skips_lock_write() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "io.github.acme/nolock";
    let server = mock_registry(id, "1.0.0").await;

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["init", "--yes", "--name", "demo"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", id, "--type", "mcp", "--no-validate"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-lockfile",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(
        !project.path().join("agents.lock").exists(),
        "no lockfile written"
    );
}

#[tokio::test]
async fn install_no_deps_writes_empty_lockfile() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["init", "--yes", "--name", "empty"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    let lock = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let v: Value = serde_json::from_str(&lock).unwrap();
    assert_eq!(v["lockfileVersion"], 1);
    assert!(v["entries"].as_object().unwrap().is_empty());
}
