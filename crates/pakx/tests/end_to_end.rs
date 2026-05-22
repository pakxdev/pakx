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
            "--no-smithery",
            "--no-pakx-registry",
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
            "--no-smithery",
            "--no-pakx-registry",
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
            "--no-smithery",
            "--no-pakx-registry",
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

/// Regression: README + CHANGELOG claim `pakx install` resolves
/// against the federated registries (official MCP + Smithery +
/// pakx-registry). Previously the install loop only called
/// `OfficialMcpSource::fetch` — `--no-smithery` / `--no-pakx-registry`
/// were dead flags. This test seeds an MCP id that exists ONLY on the
/// pakx-registry mock and asserts the install succeeds, the lockfile
/// entry records `registry: pakx`, and `--no-pakx-registry` re-breaks
/// the resolution.
#[tokio::test]
async fn install_falls_back_to_pakx_registry_when_official_mcp_404s() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/cool";

    // Official MCP: 404 for the per-server detail AND empty search.
    let official = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&official)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&official)
        .await;

    // pakx-registry: search hits with a real package containing the
    // same npm-stdio shape as the MCP Registry uses.
    let pakx = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packages": [
                {
                    "id": id,
                    "kind": "mcp",
                    "latestVersion": "1.0.0",
                    "packages": [
                        {
                            "registry_name": "npm",
                            "name": "@alice/mcp-cool",
                            "version": "1.0.0",
                            "environment_variables": []
                        }
                    ]
                }
            ]
        })))
        .mount(&pakx)
        .await;

    // Seed project.
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

    // Run install with Smithery disabled so only OfficialMcp + Pakx
    // are queried. The fallback to pakx-registry should fire.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &official.uri(),
            "--pakx-base-url",
            &pakx.uri(),
            "--no-smithery",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let lock_body = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let lock: Value = serde_json::from_str(&lock_body).unwrap();
    let key = format!("mcp/{id}@1.0.0");
    let entry = &lock["entries"][&key];
    assert_eq!(
        entry["registry"], "pakx",
        "lockfile must record the pakx-registry source on fallback"
    );
    assert_eq!(
        entry["resolvedFrom"], "pakx:alice/cool",
        "resolvedFrom must reflect the resolving source"
    );
}

/// Companion to the test above: passing `--no-pakx-registry` (with
/// Smithery also off) must re-introduce the failure mode. Documents
/// that the flag is wired up end-to-end.
#[tokio::test]
async fn install_with_no_pakx_registry_does_not_fall_back() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/cool";

    let official = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&official)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&official)
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
        .args(["add", id, "--type", "mcp", "--no-validate"])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &official.uri(),
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
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

/// Regression: `pakx install`'s `--mcp-base-url` (and the matching
/// `--smithery-base-url` / `--pakx-base-url`) must reject the
/// userinfo-smuggling bypass that PR #29 closed for `pakx test`. The
/// previous code only validated on `test`, leaving `install` open to
/// `http://localhost:8080@evil.com/` — the substring before the path
/// looks loopback-like but the real host is `evil.com`. Validation
/// must happen **before any HTTP work fires**, so even a wiremock at
/// the loopback host should never see a request.
///
/// Asserts the command exits non-zero with the registry-URL refusal
/// message; the exact format is shared with `pakx test` via
/// `crate::registry_url::validate_base_url`.
#[tokio::test]
async fn install_rejects_userinfo_smuggling_base_url() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["init", "--yes", "--name", "demo"])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            "http://localhost:8080@evil.com/",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "refusing to use registry base URL",
        ));
}

/// Regression: `--no-smithery` + `--smithery-base-url` is a
/// contradiction — the user is asking us to both skip Smithery and
/// configure it. Previously the override was silently dropped because
/// `runner.rs` only consulted the URL inside the `!no_smithery` arm.
/// `conflicts_with` makes clap reject the combination at parse time,
/// so the user sees the mistake instead of debugging a phantom
/// "smithery wasn't queried" later. Same goes for the pakx pair.
#[test]
fn install_rejects_no_smithery_with_smithery_base_url() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--smithery-base-url",
            "https://example.test",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used with"));
}

#[test]
fn install_rejects_no_pakx_registry_with_pakx_base_url() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-pakx-registry",
            "--pakx-base-url",
            "https://example.test",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used with"));
}

/// Regression: a failed install must NOT overwrite an existing
/// `agents.lock`. Previously the runner wrote the lockfile
/// unconditionally even when `report.failed` was non-empty, leaving a
/// half-pinned lockfile on disk alongside a non-zero exit code. The
/// next `pakx test` / `pakx list` / `pakx doctor` then saw an
/// incomplete state and the user had to `rm agents.lock` to retry
/// from a clean slate.
///
/// Reproducer: seed a sentinel `agents.lock`, add a dep that 404s,
/// run install, and assert the lockfile bytes are byte-identical
/// afterwards.
#[tokio::test]
async fn install_failure_does_not_overwrite_existing_lockfile() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();

    // 404 on every detail + search hit so the dep can't resolve.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers.*"))
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

    // Seed a sentinel lockfile. The body is intentionally malformed
    // for the on-disk schema — the runner has no business reading it
    // when its only job here is to not overwrite. Bytes must survive
    // the failed run untouched.
    let lock_path = project.path().join("agents.lock");
    let sentinel = b"PRE-EXISTING SENTINEL CONTENT\n";
    std::fs::write(&lock_path, sentinel).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure();

    let after =
        std::fs::read(&lock_path).expect("lockfile should still exist after failed install");
    assert_eq!(
        after, sentinel,
        "failed install must not rewrite agents.lock; got: {after:?}",
    );
}
