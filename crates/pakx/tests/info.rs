//! Integration tests for `pakx info` against a wiremock-backed registry.
//!
//! Covers both the package-level render (`pakx info <id>`) and the
//! per-version render added by this round (`pakx info <id> --version
//! <ver>`), in both human and `--json` shapes.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

/// Baseline — the existing `pakx info <id>` render must keep working
/// (no `--version` flag set).
#[tokio::test]
async fn info_without_version_renders_package_metadata_and_versions_table() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/alice/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "kind": "skills",
            "description": "hi there",
            "createdAt": "2026-05-01T00:00:00Z",
            "versions": [
                {
                    "version": "0.1.2",
                    "sha256": "9ac5b75d19827964",
                    "sizeBytes": 968,
                    "publishedAt": "2026-05-22T00:00:00Z",
                    "deprecatedAt": null
                },
                {
                    "version": "0.1.1",
                    "sha256": "aaa",
                    "sizeBytes": 900,
                    "publishedAt": "2026-05-20T00:00:00Z",
                    "deprecatedAt": null
                }
            ]
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args(["info", "alice/hello", "--registry", &server.uri()])
        .assert()
        .success()
        .stdout(predicate::str::contains("alice/hello"))
        .stdout(predicate::str::contains("0.1.2"))
        .stdout(predicate::str::contains("0.1.1"));
}

/// `--version <ver>` must hit the per-version endpoint and surface the
/// human-friendly per-version block (size, sha256, published, tarball,
/// expiry footer, install hint).
///
/// The install hint's `-t <kind>` flag threads through from the
/// package-level endpoint (a second best-effort GET fired by
/// `run_version`) so non-skills packages no longer show the wrong
/// `-t skills` default. This mock mounts both endpoints so the test
/// covers the happy path end-to-end.
#[tokio::test]
async fn info_with_version_renders_per_version_block() {
    let server = MockServer::start().await;
    // Package-level row — supplies the `kind` for the install-hint.
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/alice/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "kind": "skills",
            "description": "hi there",
            "createdAt": "2026-05-01T00:00:00Z",
            "versions": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/alice/hello/0.1.2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "version": "0.1.2",
            "sha256": "9ac5b75d19827964",
            "sizeBytes": 968,
            "publishedAt": "2026-05-22T08:06:19Z",
            "deprecatedAt": null,
            "tarballUrl": "https://private.blob.vercel-storage.com/foo?vercel-blob-delegation=XYZ"
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "info",
            "alice/hello",
            "--version",
            "0.1.2",
            "--registry",
            &server.uri(),
        ])
        .assert()
        .success()
        // Header line echoes the id.
        .stdout(predicate::str::contains("alice/hello"))
        // Per-version detail block.
        .stdout(predicate::str::contains("version:"))
        .stdout(predicate::str::contains("0.1.2"))
        .stdout(predicate::str::contains("sha256:"))
        .stdout(predicate::str::contains("9ac5b75d19827964"))
        .stdout(predicate::str::contains("968 B"))
        .stdout(predicate::str::contains("gzipped tarball"))
        .stdout(predicate::str::contains("2026-05-22T08:06:19Z"))
        .stdout(predicate::str::contains(
            "https://private.blob.vercel-storage.com/foo",
        ))
        // Expiry footer — signed URL is short-TTL.
        .stdout(predicate::str::contains("expires after 1 hour"))
        // Install hint footer threaded through from the package-level kind.
        .stdout(predicate::str::contains(
            "pakx add alice/hello@0.1.2 -t skills",
        ));
}

/// Regression for the `-t skills` hardcode: when the package-level
/// row reports a non-skills kind (e.g. mcp), the install hint must
/// thread the real kind through, not blindly print `-t skills`.
#[tokio::test]
async fn info_with_version_threads_non_skills_kind_into_install_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/acme/server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "acme/server",
            "kind": "mcp",
            "description": "an mcp server",
            "versions": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/acme/server/1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "acme/server",
            "version": "1.0.0",
            "sha256": "abc",
            "sizeBytes": 100,
            "publishedAt": "2026-05-22T00:00:00Z",
            "deprecatedAt": null,
            "tarballUrl": "https://example.com/t.tgz"
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "info",
            "acme/server",
            "--version",
            "1.0.0",
            "--registry",
            &server.uri(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("kind:"))
        .stdout(predicate::str::contains("mcp"))
        // The hint must echo `-t mcp`, NOT `-t skills`.
        .stdout(predicate::str::contains(
            "pakx add acme/server@1.0.0 -t mcp",
        ))
        .stdout(predicate::str::contains("-t skills").not());
}

/// When the package-level kind lookup fails (e.g. registry returns
/// 404 for the package row even though the per-version row exists,
/// or transport hiccup), the install hint must omit `-t <kind>`
/// entirely rather than guessing. Prior behaviour: hardcoded
/// `-t skills` regardless. New behaviour: bare `pakx add <id>@<ver>`.
#[tokio::test]
async fn info_with_version_omits_kind_when_package_lookup_fails() {
    let server = MockServer::start().await;
    // Package-level endpoint deliberately 404s — only the per-version
    // row exists. This shouldn't happen on a healthy backend but the
    // CLI must degrade gracefully when it does.
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/ghost/pkg"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/ghost/pkg/0.0.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "ghost/pkg",
            "version": "0.0.1",
            "sha256": "abc",
            "sizeBytes": 100,
            "publishedAt": "2026-05-22T00:00:00Z",
            "deprecatedAt": null,
            "tarballUrl": "https://example.com/t.tgz"
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "info",
            "ghost/pkg",
            "--version",
            "0.0.1",
            "--registry",
            &server.uri(),
        ])
        .assert()
        .success()
        // Install hint must NOT carry a `-t <kind>` flag.
        .stdout(predicate::str::contains("pakx add ghost/pkg@0.0.1"))
        .stdout(predicate::str::contains("-t skills").not())
        .stdout(predicate::str::contains("-t mcp").not());
}

/// `--version <ver> --json` must emit exactly the per-version API
/// shape — id, version, kind, sha256, sizeBytes, publishedAt,
/// deprecatedAt, tarballUrl. Stable contract for piping into `jq`.
///
/// `kind` threads through from the best-effort package-level GET, so
/// the test mock mounts both endpoints. When the package-level lookup
/// fails, `kind` falls back to `null` (covered by the sibling test
/// `info_with_version_omits_kind_when_package_lookup_fails`).
#[tokio::test]
async fn info_with_version_json_matches_per_version_shape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/alice/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "kind": "skills",
            "versions": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/alice/hello/0.1.2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "version": "0.1.2",
            "sha256": "9ac5b75d19827964",
            "sizeBytes": 968,
            "publishedAt": "2026-05-22T08:06:19Z",
            "deprecatedAt": null,
            "tarballUrl": "https://private.blob.vercel-storage.com/foo?sig=abc"
        })))
        .mount(&server)
        .await;

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "info",
            "alice/hello",
            "--version",
            "0.1.2",
            "--json",
            "--registry",
            &server.uri(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let body: Value = serde_json::from_slice(&out.stdout).expect("info --json must be valid JSON");
    assert_eq!(body["id"], "alice/hello");
    assert_eq!(body["version"], "0.1.2");
    assert_eq!(body["kind"], "skills");
    assert_eq!(body["sha256"], "9ac5b75d19827964");
    assert_eq!(body["sizeBytes"], 968);
    assert_eq!(body["publishedAt"], "2026-05-22T08:06:19Z");
    assert!(body["deprecatedAt"].is_null());
    assert_eq!(
        body["tarballUrl"],
        "https://private.blob.vercel-storage.com/foo?sig=abc"
    );
}

/// 404 on the per-version endpoint must surface as a clean "not found"
/// error, not a raw reqwest dump.
#[tokio::test]
async fn info_with_version_404_renders_clean_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/ghost/server/9.9.9"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "info",
            "ghost/server",
            "--version",
            "9.9.9",
            "--registry",
            &server.uri(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

/// Per-version endpoint can omit `tarballUrl` (the wire format is
/// permissive; the resolver enforces presence at install time). The
/// `--version` render must still render the rest of the block without
/// the expiry-note footer.
#[tokio::test]
async fn info_with_version_handles_missing_tarball_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/alice/hello/0.1.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "version": "0.1.0",
            "sha256": "abc",
            "sizeBytes": 100,
            "publishedAt": "2026-05-22T00:00:00Z",
            "deprecatedAt": null
        })))
        .mount(&server)
        .await;

    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "info",
            "alice/hello",
            "--version",
            "0.1.0",
            "--registry",
            &server.uri(),
        ])
        .assert()
        .success();
    let out = assert.get_output();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("0.1.0"), "stdout:\n{stdout}");
    assert!(stdout.contains("100 B"), "stdout:\n{stdout}");
    // No tarball line means no expiry footer either.
    assert!(
        !stdout.contains("expires after 1 hour"),
        "expiry footer must be gated on tarballUrl presence; stdout:\n{stdout}"
    );
}

/// `pakx info --registry` must vet user-supplied base URLs via
/// `validate_base_url` BEFORE the HTTP probe fires. Even though `info`
/// is read-only, leaking the queried `<owner>/<name>` over plaintext
/// would still hand a network observer the package the user is
/// inspecting (and on a userinfo-smuggled URL, the request body would
/// go to an attacker-controlled host entirely).
#[test]
fn info_rejects_plaintext_http_registry() {
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["info", "alice/hello", "--registry", "http://evil.com"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to use registry base URL",
        ));
}
