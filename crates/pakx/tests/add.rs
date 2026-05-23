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

// ---------------------------------------------------------------------------
// pakx-registry kind probe (no `-t / --type` supplied)
// ---------------------------------------------------------------------------

/// Spin up a wiremock pakx-registry that reports a given `kind` for the
/// single id `<owner>/<name>`. Mirrors the wire shape of
/// `GET /api/v1/packages/{owner}/{name}` — id, kind, optional versions.
async fn mock_pakx_registry_returns_kind(owner: &str, name: &str, kind: &str) -> MockServer {
    let server = MockServer::start().await;
    let id = format!("{owner}/{name}");
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/packages/{owner}/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id,
            "kind": kind,
            "versions": [{"version": "0.1.0"}],
        })))
        .mount(&server)
        .await;
    server
}

async fn mock_pakx_registry_404() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/packages/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    server
}

/// Regression for the user-reported bug:
///
///   `pakx add arwenizEr/hello-world` → warn-then-misroute to `mcp:`,
///   then `pakx install` fails because the package is actually a
///   pakx-registry skill.
///
/// With the kind probe in place, pakx-registry is consulted first; it
/// reports `kind: "skills"`, the manifest lands the id under `skills:`,
/// and the MCP-registry validation never fires.
#[tokio::test]
async fn add_probes_pakx_registry_and_routes_skill_kind() {
    let temp = TempDir::new().unwrap();
    let pakx = mock_pakx_registry_returns_kind("arwenizEr", "hello-world", "skills").await;

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args([
            "add",
            "arwenizEr/hello-world",
            "--pakx-base-url",
            &pakx.uri(),
        ])
        .assert()
        .success()
        // No "official MCP Registry" warning when pakx-registry hits —
        // the probe is authoritative.
        .stderr(predicate::str::contains("official MCP Registry").not());

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(
        m.dependencies.skills.is_some(),
        "should route to skills section based on pakx-registry kind probe; body=\n{body}"
    );
    assert!(
        m.dependencies.mcp.is_none(),
        "should NOT misroute to mcp; body=\n{body}"
    );
}

/// When pakx-registry returns 404 AND the official MCP Registry also
/// has no record, `pakx add` preserves the historical fallback
/// (`kind = mcp`) but emits the softened warning that names BOTH
/// sources, so the user understands the full search path.
#[tokio::test]
async fn add_softened_warning_when_neither_registry_finds_id() {
    let temp = TempDir::new().unwrap();
    let pakx = mock_pakx_registry_404().await;
    let mcp = mock_mcp_server_404().await;

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(temp.path())
        .args([
            "add",
            "foo/bar",
            "--pakx-base-url",
            &pakx.uri(),
            "--mcp-base-url",
            &mcp.uri(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "not found in pakx-registry or the official MCP Registry",
        ))
        .stderr(predicate::str::contains("override with -t skills"));

    let body = std::fs::read_to_string(temp.path().join("agents.yml")).unwrap();
    let m = parse_manifest(&body, None).unwrap();
    assert!(
        m.dependencies.mcp.is_some(),
        "should preserve historical mcp fallback; body=\n{body}"
    );
    assert!(
        m.dependencies.skills.is_none(),
        "should NOT speculatively add to skills; body=\n{body}"
    );
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

/// Regression for the 2026-05-23 stdout/stderr alignment: the
/// machine-readable success line (`added <id> (<kind>)`) must land on
/// **stdout** so a script piping `pakx add ... | grep added` actually
/// matches. The `→ next:` hint follows on **stderr** — convention
/// across the CLI is "success → stdout, human hint → stderr".
#[tokio::test]
async fn add_routes_success_line_to_stdout_and_hint_to_stderr() {
    let temp = TempDir::new().unwrap();
    let server = mock_mcp_server_ok("a/b").await;
    let assertion = Command::cargo_bin(BIN)
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
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        stdout.contains("added a/b (mcp)"),
        "success line must be on stdout; got stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("added a/b (mcp)"),
        "success line must NOT be duplicated on stderr; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("\u{2192} next: pakx install"),
        "→ next hint must be on stderr; got stderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("\u{2192} next: pakx install"),
        "→ next hint must NOT be on stdout; got stdout:\n{stdout}"
    );
}
