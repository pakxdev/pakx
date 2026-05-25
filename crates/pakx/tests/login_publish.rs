//! End-to-end tests for login / whoami / pack / publish / unpublish
//! against a wiremock-backed pakx-registry.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const BIN: &str = "pakx";
const VALID_TOKEN: &str = "pakx_v1_TEST_TOKEN";

fn write_skill(dir: &TempDir, name: &str, version: &str) {
    std::fs::write(
        dir.path().join("SKILL.md"),
        format!("---\nname: {name}\nversion: {version}\n---\n# {name}\n"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("reference")).unwrap();
    std::fs::write(dir.path().join("reference/usage.md"), b"usage docs\n").unwrap();
}

async fn mock_registry(login: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/whoami"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "id": "u_1", "login": login, "email": null })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "pkg_1",
            "owner": login,
            "name": "pdf",
            "kind": "skills",
            "created": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": format!("{login}/pdf"),
            "version": "1.0.0",
            "sha256": "0".repeat(64),
            "sizeBytes": 123,
            "tarballUrl": "https://example.com/tarball.tgz"
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn login_stores_credentials() {
    let temp = TempDir::new().unwrap();
    let creds = temp.path().join("creds.json");
    let server = mock_registry("alice").await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("alice"));

    let body = std::fs::read_to_string(&creds).unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    let reg_url = server.uri().to_lowercase();
    let entry = &v["registries"][&reg_url];
    assert_eq!(entry["token"], VALID_TOKEN);
    assert_eq!(entry["login"], "alice");
}

#[tokio::test]
async fn login_rejects_non_pakx_v1_tokens() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            "https://x.test",
            "--token",
            "wrong-prefix",
            "--credentials-file",
            temp.path().join("c.json").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pakx_v1_"));
}

#[tokio::test]
async fn whoami_prints_login() {
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;

    // Seed creds via login.
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "whoami",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("alice"));
}

#[tokio::test]
async fn whoami_json_online_emits_id_and_email() {
    // The online JSON path round-trips the full backend payload — id,
    // email, login — and tags the payload with `"source": "online"` so
    // pipelines can distinguish a live whoami from a cached fallback.
    // Backend fixture returns a non-null `email` here so we cover the
    // happy-path serialisation (the offline / not-logged-in paths emit
    // `email: null` and are exercised by the sibling tests below).
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/whoami"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "u_42",
            "login": "alice",
            "email": "alice@example.com",
        })))
        .mount(&server)
        .await;

    // Seed creds via login. Login itself hits whoami, so the same mock
    // serves both calls.
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "whoami",
            "--json",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let line = String::from_utf8(output).unwrap();
    // Single newline-terminated JSON line — match the `pakx list --json`
    // style contract.
    assert!(
        line.ends_with('\n'),
        "expected newline terminator: {line:?}"
    );
    assert_eq!(
        line.lines().count(),
        1,
        "expected single-line JSON: {line:?}"
    );
    let v: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["login"], "alice");
    assert_eq!(v["id"], "u_42");
    assert_eq!(v["email"], "alice@example.com");
    assert_eq!(v["source"], "online");
    assert_eq!(v["registry"], server.uri());
}

#[tokio::test]
async fn whoami_json_offline_uses_cached_login() {
    // `--offline` short-circuits the network round-trip: emits the
    // stored login + `"source": "cached"`, and id/email are absent
    // because the cache never persisted them.
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "whoami",
            "--json",
            "--offline",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let line = String::from_utf8(output).unwrap();
    let v: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["login"], "alice");
    assert!(v["id"].is_null(), "cached id should be null: {v}");
    assert!(v["email"].is_null(), "cached email should be null: {v}");
    assert_eq!(v["source"], "cached");
    assert_eq!(v["registry"], server.uri());
}

#[tokio::test]
async fn whoami_json_not_logged_in_emits_none_payload_and_exits_1() {
    // No stored credentials for the targeted registry — the human path
    // errors with "not logged in", but `--json` consumers want a
    // structured payload. We emit `{login: null, source: "none"}` and
    // still exit 1 so the exit code is a fast-path discriminator.
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json"); // never created
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "whoami",
            "--json",
            "--registry",
            "https://registry.pakx.dev",
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let line = String::from_utf8(output).unwrap();
    let v: Value = serde_json::from_str(line.trim()).unwrap();
    assert!(v["login"].is_null());
    assert!(v["id"].is_null());
    assert!(v["email"].is_null());
    assert_eq!(v["source"], "none");
    assert_eq!(v["registry"], "https://registry.pakx.dev");
}

#[tokio::test]
async fn whoami_json_network_failure_degrades_to_cached() {
    // The JSON contract treats a transient network failure as
    // equivalent to `--offline`: a pipeline shouldn't break on a
    // DNS-blocked host. The cached payload is distinguishable from the
    // online payload via `"source": "cached"`, so callers that care
    // can detect the degradation. The human (non-JSON) path still
    // surfaces the network error.
    //
    // To simulate a network failure deterministically we seed creds
    // against a live mock, then run `whoami --json` against a
    // separate URL that points at a closed port on loopback. We pick
    // a high port that is statistically unlikely to be in use and
    // that the OS will reject with ECONNREFUSED — fast and cross-
    // platform (Windows + unix). Using port 1 / 0 is unreliable on
    // Windows (port 0 means "auto-assign"); a high port works.
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    // Manually rewrite the credentials map so the entry's key matches
    // the dead-port URL we will hit next. Skips wiring a second mock
    // just to seed creds at the right key.
    let dead_url = "http://127.0.0.1:1";
    let body = std::fs::read_to_string(&creds_path).unwrap();
    let mut v: Value = serde_json::from_str(&body).unwrap();
    let live_key = server.uri().to_lowercase();
    let entry = v["registries"][&live_key].clone();
    let registries = v["registries"].as_object_mut().unwrap();
    registries.insert(dead_url.to_string(), entry);
    registries.remove(&live_key);
    std::fs::write(&creds_path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "whoami",
            "--json",
            "--registry",
            dead_url,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let line = String::from_utf8(output).unwrap();
    let v: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["login"], "alice");
    assert_eq!(v["source"], "cached");
}

#[tokio::test]
async fn whoami_offline_uses_stored_login() {
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "whoami",
            "--offline",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("alice"));
}

#[test]
fn pack_writes_tarball() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill(&src, "pdf", "1.0.0");

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--out",
            out.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    let tgz = out.path().join("pdf-1.0.0.tgz");
    assert!(tgz.is_file(), "expected {} to exist", tgz.display());
    let size = std::fs::metadata(&tgz).unwrap().len();
    assert!(size > 0, "tarball is empty");
}

#[test]
fn pack_rejects_missing_skill_md() {
    let src = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["pack", src.path().to_str().unwrap()])
        .assert()
        .failure();
}

#[tokio::test]
async fn publish_runs_full_pack_create_upload_flow() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;

    write_skill(&src, "pdf", "1.0.0");

    // Login first.
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("uploaded"));
}

#[tokio::test]
async fn publish_short_sha256_does_not_panic() {
    // Regression: the human "uploaded … sha256 <…>" line truncated the
    // registry-returned sha256 with a raw byte-slice `[..16]`, which
    // panicked when the server returned a string shorter than 16 bytes —
    // AND it panicked AFTER the upload succeeded, so the user saw a
    // crash despite the package being live. The fix uses
    // `.get(..16).unwrap_or(&sha256)`; a short sha must now render the
    // whole string and exit cleanly.
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");

    // Build a server whose PUT returns a 3-char sha256 ("abc").
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/whoami"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "id": "u_1", "login": "alice", "email": null })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "pkg_1",
            "owner": "alice",
            "name": "pdf",
            "kind": "skills",
            "created": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/pdf",
            "version": "1.0.0",
            // Deliberately shorter than 16 bytes.
            "sha256": "abc",
            "sizeBytes": 123,
            "tarballUrl": "https://example.com/tarball.tgz"
        })))
        .mount(&server)
        .await;

    write_skill(&src, "pdf", "1.0.0");

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        // No panic; clean exit. The short sha renders whole.
        .success()
        .stderr(predicate::str::contains("uploaded"))
        .stderr(predicate::str::contains("sha256 abc"));
}

#[tokio::test]
async fn publish_dry_run_skips_upload() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;

    write_skill(&src, "pdf", "1.0.0");
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("dry-run"));
}

#[tokio::test]
async fn unpublish_calls_delete() {
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let assertion = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "unpublish",
            "alice/pdf@1.0.0",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
            // `unpublish` now prompts for confirmation; `--yes` is
            // required on a non-TTY (test harness) to proceed.
            "--yes",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("unpublished alice/pdf@1.0.0"));
    // Round 39 copy correction: the post-unpublish hint must NOT
    // promise the aspirational "30-day soft-delete grace; resolves to
    // 404" semantics (no hard-delete cron exists on the registry), and
    // MUST explain the real behaviour ("still resolvable for existing
    // pins but hidden from list endpoints").
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        !stderr.contains("30-day soft-delete grace"),
        "old aspirational copy must be gone: {stderr}",
    );
    assert!(
        stderr.contains("still resolvable for existing pins"),
        "new accurate copy must be present: {stderr}",
    );
}

#[tokio::test]
async fn unpublish_without_yes_on_non_tty_bails_with_hint() {
    // A destructive soft-delete must not run unconfirmed. On a non-TTY
    // (the assert_cmd harness has no terminal) and without `--yes`, the
    // command must FAIL FAST with an actionable hint — never hang on a
    // prompt that can't be answered, and never silently unpublish.
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "unpublish",
            "alice/pdf@1.0.0",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        // No `--yes`; non-TTY harness → bail.
        .assert()
        .failure()
        .stderr(predicate::str::contains("stdin is not a TTY"))
        .stderr(predicate::str::contains("--yes"))
        // The DELETE must NOT have fired: no success line.
        .stderr(predicate::str::contains("unpublished").not());
}

/// Publish-emit shape: when the manifest declares sponsors, the POST
/// body to `/api/v1/packages` must include the `sponsors` field as a
/// JSON array. When the manifest omits the field, the POST body must
/// **not** include a `sponsors` key (the registry treats absent as "no
/// change"; an explicit `[]` would clear existing sponsors on a
/// republish). This pins both contract halves.
#[tokio::test]
async fn publish_emits_sponsors_when_manifest_declares_them() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = MockServer::start().await;

    // SKILL.md with two sponsors — one github, one escape-hatch url.
    std::fs::write(
        src.path().join("SKILL.md"),
        "---\nname: pdf\nversion: 1.0.0\nsponsors:\n  - kind: github\n    url: https://github.com/sponsors/alice\n  - kind: url\n    url: https://opencollective.com/alice\n---\n# pdf\n",
    )
    .unwrap();

    // whoami / POST package / PUT version — POST is the one we inspect.
    Mock::given(method("GET"))
        .and(path("/api/v1/whoami"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "id": "u_1", "login": "alice", "email": null })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "pkg_1",
            "owner": "alice",
            "name": "pdf",
            "kind": "skills",
            "created": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/pdf",
            "version": "1.0.0",
            "sha256": "0".repeat(64),
            "sizeBytes": 123,
            "tarballUrl": "https://example.com/tarball.tgz"
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let posts = post_packages_bodies(&server).await;
    assert_eq!(posts.len(), 1, "expected exactly one POST /api/v1/packages");
    let body = &posts[0];
    let sponsors = body
        .get("sponsors")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected sponsors array in POST body, got: {body}"));
    assert_eq!(sponsors.len(), 2);
    assert_eq!(sponsors[0]["kind"], "github");
    assert_eq!(sponsors[0]["url"], "https://github.com/sponsors/alice");
    assert_eq!(sponsors[1]["kind"], "url");
    assert_eq!(sponsors[1]["url"], "https://opencollective.com/alice");
}

#[tokio::test]
async fn publish_omits_sponsors_when_manifest_declares_none() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;

    write_skill(&src, "pdf", "1.0.0");

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let posts = post_packages_bodies(&server).await;
    assert_eq!(posts.len(), 1);
    let body = &posts[0];
    assert!(
        body.get("sponsors").is_none(),
        "sponsors must be omitted (not `null`, not `[]`) when manifest declares none — \
         the registry treats absent as no-change but `[]` as clear; got: {body}"
    );
}

/// Pull every `POST /api/v1/packages` request body the server received
/// and decode each as JSON. Used by the sponsor-emit tests to inspect
/// the publish wire shape without depending on wiremock's matcher DSL
/// (which is awkward for shape-of-array assertions).
async fn post_packages_bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .expect("wiremock recorder enabled")
        .into_iter()
        .filter(|r: &Request| {
            r.method == wiremock::http::Method::POST && r.url.path() == "/api/v1/packages"
        })
        .map(|r| serde_json::from_slice::<Value>(&r.body).expect("POST body is valid json"))
        .collect()
}

/// Pull every `PUT /api/v1/packages/...` (version upload) request the
/// server received. Returns the raw `Request` so callers can inspect
/// both the binary tarball body and headers like `x-pakx-readme-b64`.
async fn put_version_requests(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("wiremock recorder enabled")
        .into_iter()
        .filter(|r: &Request| {
            // `/api/v1/packages/<owner>/<name>/<version>` —
            // `.split('/')` on the leading-slash path yields 7 segments
            // (leading empty + "api" + "v1" + "packages" + 3 dynamic
            // parts). Match exactly so the PATCH/GET on
            // `/api/v1/packages/<owner>/<name>` (6 segments) never gets
            // counted here.
            r.method == wiremock::http::Method::PUT
                && r.url.path().starts_with("/api/v1/packages/")
                && r.url.path().split('/').count() == 7
        })
        .collect()
}

/// A bundle that ships a `README.md` alongside `SKILL.md` must forward
/// the markdown to the registry on both the POST upsert (JSON body
/// `readme` field) and the PUT version upload (`x-pakx-readme-b64`
/// header). The split exists because the PUT body is the raw tarball;
/// the README rides as a base64-encoded header so it can travel
/// alongside the binary payload without moving to multipart.
#[tokio::test]
async fn publish_forwards_readme_in_post_body_and_put_header() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;

    // Same minimal skill the other publish tests use, plus a README.
    write_skill(&src, "pdf", "1.0.0");
    let readme_body = "# pdf\n\nLong-form usage docs.\n\n```\npakx add alice/pdf\n```\n";
    std::fs::write(src.path().join("README.md"), readme_body).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // POST body must carry the README verbatim — the upsert path is
    // the canonical store for `packages.readme`.
    let posts = post_packages_bodies(&server).await;
    assert_eq!(posts.len(), 1, "expected exactly one POST /api/v1/packages");
    let body = &posts[0];
    let posted_readme = body
        .get("readme")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected `readme` string in POST body, got: {body}"));
    assert_eq!(posted_readme, readme_body);

    // PUT must carry the README via the base64 header. The binary
    // body is still the raw tarball so the registry's Content-Length
    // pre-check keeps working; the header is the only practical
    // piggyback path.
    let puts = put_version_requests(&server).await;
    assert_eq!(puts.len(), 1, "expected exactly one PUT version request");
    let header_b64 = puts[0]
        .headers
        .get("x-pakx-readme-b64")
        .unwrap_or_else(|| panic!("expected x-pakx-readme-b64 header on PUT"))
        .to_str()
        .expect("header is ascii base64");
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        header_b64.as_bytes(),
    )
    .expect("header value decodes as base64");
    let decoded_text = String::from_utf8(decoded).expect("decoded readme is utf-8");
    assert_eq!(decoded_text, readme_body);
}

/// A bundle without a `README.md` must NOT send `readme` in the POST
/// body and NOT send the `x-pakx-readme-b64` header. The registry
/// treats omission as "no change", so silently sending an empty string
/// would wipe a previously-published README on republish.
#[tokio::test]
async fn publish_omits_readme_when_bundle_has_none() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;

    write_skill(&src, "pdf", "1.0.0");

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let posts = post_packages_bodies(&server).await;
    assert_eq!(posts.len(), 1);
    assert!(
        posts[0].get("readme").is_none(),
        "readme must be omitted (not null, not empty) when bundle has no README.md — \
         the registry treats absent as no-change; got body: {body}",
        body = posts[0]
    );

    let puts = put_version_requests(&server).await;
    assert_eq!(puts.len(), 1);
    assert!(
        puts[0].headers.get("x-pakx-readme-b64").is_none(),
        "x-pakx-readme-b64 header must be absent when bundle has no README.md"
    );
}

#[test]
fn unpublish_rejects_bad_spec() {
    let temp = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "unpublish",
            "no-at-sign",
            "--registry",
            "https://x.test",
            "--credentials-file",
            temp.path().join("c.json").to_str().unwrap(),
        ])
        .assert()
        .failure();
}

/// `pakx publish --json` must keep all progress on stderr and emit a
/// **single** newline-terminated JSON object on stdout once upload
/// completes. The shape is part of the documented contract — pin every
/// stable field name here so any future churn shows up in CI.
#[tokio::test]
async fn publish_json_emits_stable_shape_on_stdout() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;
    write_skill(&src, "pdf", "1.0.0");

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.ends_with('\n'), "stdout must end with newline");
    let body = stdout.trim_end_matches('\n');
    assert!(
        !body.contains('\n'),
        "json output must be single-line: {body}"
    );
    let v: Value = serde_json::from_str(body).expect("stdout is valid json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["name"], "alice/pdf");
    assert_eq!(v["version"], "1.0.0");
    assert_eq!(v["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(v["sizeBytes"], 123);
    assert_eq!(v["tarballUrl"], "https://example.com/tarball.tgz");
    let registry_url = v["registryUrl"].as_str().expect("registryUrl string");
    assert!(
        registry_url.contains("/p/pakx/alice/pdf/1.0.0"),
        "registryUrl must point at the dashboard route: {registry_url}"
    );
    // `publishedAt` is reserved on the JSON contract for when the
    // backend wires it through the upload response — emit null today.
    assert!(
        v["publishedAt"].is_null(),
        "publishedAt should be null until the backend response carries it"
    );
    assert!(
        v["warnings"].as_array().is_some(),
        "warnings array must always be present, even when empty"
    );

    // Stdout MUST NOT carry any of the human progress lines that go to
    // stderr (`packed`, `uploaded`, etc.). The JSON contract reserves
    // stdout exclusively for the payload object.
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("uploaded") || stderr.contains("published"),
        "human progress must remain on stderr: {stderr}"
    );
    assert!(
        !body.contains("uploaded"),
        "stdout json payload must not contain human progress: {body}"
    );
}

/// `--dry-run` short-circuits before the registry round-trip. The JSON
/// contract still applies: emit `{ok: true, dryRun: true}` and skip
/// registry-side fields. Pin the contract so future churn shows up.
#[tokio::test]
async fn publish_json_dry_run_emits_dry_run_flag() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = mock_registry("alice").await;
    write_skill(&src, "pdf", "1.0.0");

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
            "--json",
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("stdout is valid json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["dryRun"], true);
    assert_eq!(v["name"], "pdf");
    assert_eq!(v["version"], "1.0.0");
    assert!(
        v.get("tarballUrl").is_none(),
        "tarballUrl must be absent on dry-run"
    );
    assert!(
        v.get("registryUrl").is_none(),
        "registryUrl must be absent on dry-run"
    );
    assert!(
        v["warnings"].as_array().is_some(),
        "warnings array must always be present"
    );
}

// ---------------------------------------------------------------------------
// Negative tests: token-sending subcommands must reject plaintext-HTTP /
// userinfo-smuggled `--registry` overrides BEFORE any HTTP work fires.
// The contract is enforced by `crate::registry_url::validate_base_url`,
// shared with `pakx install` / `pakx test` / `pakx outdated` / `pakx
// audit` / `pakx login`. A regression here means a token / package id /
// bearer-authed payload could leak over a network observer's wire.
// ---------------------------------------------------------------------------

#[test]
fn publish_rejects_plaintext_http_registry() {
    let temp = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    write_skill(&src, "pdf", "1.0.0");
    let creds_path = temp.path().join("creds.json");
    // Seed a credentials file pointed at the same plaintext URL so the
    // command doesn't short-circuit on a missing entry — the URL check
    // must reject BEFORE the credentials lookup.
    std::fs::write(
        &creds_path,
        r#"{"registries":{"http://evil.com":{"token":"pakx_v1_x"}}}"#,
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            "http://evil.com",
            "--credentials-file",
            creds_path.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to use registry base URL",
        ));
}

#[test]
fn unpublish_rejects_plaintext_http_registry() {
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("creds.json");
    std::fs::write(
        &creds_path,
        r#"{"registries":{"http://evil.com":{"token":"pakx_v1_x"}}}"#,
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "unpublish",
            "alice/pdf@1.0.0",
            "--registry",
            "http://evil.com",
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to use registry base URL",
        ));
}

#[test]
fn whoami_rejects_plaintext_http_registry() {
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("creds.json");
    std::fs::write(
        &creds_path,
        r#"{"registries":{"http://evil.com":{"token":"pakx_v1_x"}}}"#,
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "whoami",
            "--registry",
            "http://evil.com",
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to use registry base URL",
        ));
}

// ---------------------------------------------------------------------------
// PM-2: `pakx publish` failure-mode hints
//
// Each of the codes the registry's PUT version handler + POST upsert
// handler actually emits gets mapped to a multi-line CLI hint and (in
// `--json` mode) a structured `{errorKind, fixHint, upstreamCode}`
// block. The tests below pin both halves:
//   - non-json: assert the stderr contains the publisher-facing fix
//     hint substring
//   - json: assert stdout carries a single-line JSON envelope with the
//     CLI-stable `errorKind` discriminator and the upstream status code
//
// Wiremock is mounted with the LOGIN happy-path first (so the CLI
// reaches the publish call), then the PUT or POST is overridden to
// return the failure code under test. Each test exits non-zero.
// ---------------------------------------------------------------------------

/// Helper: minimal happy-path `whoami` so login succeeds before the
/// publish step trips the failure mock. Returns the live server handle
/// for the caller to mount further routes on.
async fn whoami_only_server(login: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/whoami"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "id": "u_1", "login": login, "email": null })),
        )
        .mount(&server)
        .await;
    server
}

/// Helper: mount a working POST `/api/v1/packages` upsert so the test
/// reaches the version PUT (where most failure codes live).
async fn mount_successful_post(server: &MockServer, login: &str) {
    Mock::given(method("POST"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "pkg_1",
            "owner": login,
            "name": "pdf",
            "kind": "skills",
            "created": true
        })))
        .mount(server)
        .await;
}

/// Helper: seed the credentials file via `pakx login` against the
/// supplied registry. Asserts success.
fn login_into(creds_path: &std::path::Path, registry: &str) {
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--registry",
            registry,
            "--token",
            VALID_TOKEN,
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();
}

/// Helper: invoke `pakx publish` with the supplied flags, expect a
/// non-zero exit, and return the full output for further inspection.
fn publish_expect_failure(
    src: &std::path::Path,
    creds_path: &std::path::Path,
    registry: &str,
    extra: &[&str],
) -> std::process::Output {
    let mut args: Vec<&str> = vec![
        "publish",
        src.to_str().unwrap(),
        "--registry",
        registry,
        "--credentials-file",
        creds_path.to_str().unwrap(),
    ];
    args.extend(extra);
    Command::cargo_bin(BIN)
        .unwrap()
        .args(&args)
        .assert()
        .failure()
        .get_output()
        .clone()
}

/// 413 tarball-too-large — registry returns the cap in `maxBytes`. Hint
/// must quote the cap in human-readable MiB; JSON must carry both
/// `maxBytes` and `upstreamCode: 413`.
#[tokio::test]
async fn publish_413_tarball_too_large_emits_hint_and_json() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    let server = whoami_only_server("alice").await;
    mount_successful_post(&server, "alice").await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(413).set_body_json(json!({
            "error": "too-large",
            "maxBytes": 50 * 1024 * 1024
        })))
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    // Non-json: stderr hint must surface "Tarball too large" and quote
    // the 50 MiB cap.
    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("Tarball too large"),
        "expected tarball-too-large hint: {stderr}"
    );
    assert!(
        stderr.contains("50 MiB"),
        "expected cap quoted in MiB: {stderr}"
    );

    // JSON: stdout carries the structured envelope.
    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["ok"], false);
    assert_eq!(v["errorKind"], "tarball-too-large");
    assert_eq!(v["upstreamCode"], 413);
    assert_eq!(v["maxBytes"], 50 * 1024 * 1024_u64);
    assert!(v["fixHint"].is_string());
}

/// 411 length-required — registry emits this when the request lacks a
/// parseable Content-Length. CLI hint must point at the proxy / network
/// layer as the likely cause, not at the bundle contents.
#[tokio::test]
async fn publish_411_length_required_emits_hint_and_json() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    let server = whoami_only_server("alice").await;
    mount_successful_post(&server, "alice").await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(411).set_body_json(json!({
            "error": "length-required"
        })))
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("Content-Length"),
        "expected Content-Length in hint: {stderr}"
    );
    assert!(stderr.contains("411"), "expected status quoted: {stderr}");

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["errorKind"], "length-required");
    assert_eq!(v["upstreamCode"], 411);
}

/// 409 version-exists — the registry's PUT version path emits this
/// when re-publishing a version that already exists. Hint must tell the
/// user to bump `version:`.
#[tokio::test]
async fn publish_409_version_exists_emits_hint_and_json() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    let server = whoami_only_server("alice").await;
    mount_successful_post(&server, "alice").await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": "version-exists"
        })))
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("already published"),
        "expected version-exists hint: {stderr}"
    );
    assert!(
        stderr.contains("bump"),
        "expected bump-version action: {stderr}"
    );

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["errorKind"], "version-exists");
    assert_eq!(v["upstreamCode"], 409);
}

/// 409 kind-mismatch — the registry's POST upsert path emits this
/// when a republish attempts to change the package kind. JSON must
/// surface both stored + received.
#[tokio::test]
async fn publish_409_kind_mismatch_emits_hint_and_json() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    let server = whoami_only_server("alice").await;
    // POST is the failure source on this code, not the PUT — override
    // POST to return 409 with the stored/received body shape.
    Mock::given(method("POST"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": "kind-mismatch",
            "stored": "mcp",
            "received": "skills",
            "hint": "A package's kind is immutable."
        })))
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("kind conflict") || stderr.contains("kind"),
        "expected kind-mismatch hint: {stderr}"
    );
    assert!(
        stderr.contains("mcp"),
        "expected stored kind quoted: {stderr}"
    );
    assert!(
        stderr.contains("skills"),
        "expected received kind quoted: {stderr}"
    );

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["errorKind"], "kind-mismatch");
    assert_eq!(v["upstreamCode"], 409);
    assert_eq!(v["stored"], "mcp");
    assert_eq!(v["received"], "skills");
}

/// 429 rate-limited — registry sets `Retry-After` via
/// `withRateLimitHeaders`. The CLI hint must quote the header value;
/// JSON must carry `retryAfterSeconds`.
#[tokio::test]
async fn publish_429_rate_limited_emits_hint_and_json() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    let server = whoami_only_server("alice").await;
    mount_successful_post(&server, "alice").await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "42")
                .set_body_json(json!({ "error": "too-many-requests" })),
        )
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("Rate limited"),
        "expected rate-limited hint: {stderr}"
    );
    assert!(
        stderr.contains("42"),
        "expected retry-after value quoted: {stderr}"
    );

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["errorKind"], "rate-limited");
    assert_eq!(v["upstreamCode"], 429);
    assert_eq!(v["retryAfterSeconds"], 42);
}

/// 401 unauthorized — token expired or revoked. Hint must point at
/// `pakx login`.
#[tokio::test]
async fn publish_401_unauthorized_emits_hint_and_json() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    // whoami needs to succeed so login can seed creds; afterward we
    // need POST to 401. Mock whoami first, then POST.
    let server = whoami_only_server("alice").await;
    Mock::given(method("POST"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "unauthorized" })))
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("Token expired"),
        "expected token-expired hint: {stderr}"
    );
    assert!(
        stderr.contains("pakx login"),
        "expected `pakx login` action: {stderr}"
    );

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["errorKind"], "unauthorized");
    assert_eq!(v["upstreamCode"], 401);
}

/// 403 forbidden — caller doesn't own the package name. Hint must tell
/// them to pick a different name.
#[tokio::test]
async fn publish_403_forbidden_emits_hint_and_json() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    let server = whoami_only_server("alice").await;
    Mock::given(method("POST"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "error": "forbidden" })))
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("don't own") || stderr.contains("different `name:"),
        "expected forbidden hint: {stderr}"
    );

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["errorKind"], "forbidden");
    assert_eq!(v["upstreamCode"], 403);
}

/// 400 invalid-request — the POST upsert path emits this on zod
/// refusal. Detail field must echo back into the hint and the JSON
/// payload.
#[tokio::test]
async fn publish_400_invalid_emits_hint_with_detail_and_json() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    let server = whoami_only_server("alice").await;
    Mock::given(method("POST"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid",
            "detail": "readme too large (max 256 KiB)"
        })))
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("readme too large"),
        "expected detail surfaced in hint: {stderr}"
    );

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["errorKind"], "invalid-request");
    assert_eq!(v["upstreamCode"], 400);
    assert_eq!(v["detail"], "readme too large (max 256 KiB)");
}

/// 500 internal — registry's `internalError()` helper redacts the body
/// in production. Hint must say "retry / file an issue if persistent".
#[tokio::test]
async fn publish_500_internal_emits_hint_and_json() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    let server = whoami_only_server("alice").await;
    mount_successful_post(&server, "alice").await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({ "error": "internal" })))
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("500"),
        "expected 500 quoted in hint: {stderr}"
    );
    assert!(
        stderr.contains("Retry") || stderr.contains("issue"),
        "expected retry/issue guidance: {stderr}"
    );

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["errorKind"], "registry-internal");
    assert_eq!(v["upstreamCode"], 500);
}

/// Unmapped status — the registry shouldn't emit it, but if a future
/// code lands the CLI must surface the upstream body verbatim instead
/// of being smothered by a generic hint. Use 418 as the canonical
/// "this never happens" code.
#[tokio::test]
async fn publish_unmapped_status_surfaces_upstream_body() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    write_skill(&src, "pdf", "1.0.0");
    let server = whoami_only_server("alice").await;
    mount_successful_post(&server, "alice").await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(418).set_body_string("teapot"))
        .mount(&server)
        .await;

    login_into(&creds_path, &server.uri());

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("418"), "expected status quoted: {stderr}");
    assert!(
        stderr.contains("teapot"),
        "expected upstream body surfaced verbatim: {stderr}"
    );
    assert!(
        stderr.contains("file an issue"),
        "expected file-an-issue prompt: {stderr}"
    );

    let out = publish_expect_failure(src.path(), &creds_path, &server.uri(), &["--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["errorKind"], "unmapped");
    assert_eq!(v["upstreamCode"], 418);
    assert_eq!(v["detail"], "teapot");
}

#[test]
fn publish_rejects_userinfo_smuggling_registry() {
    let temp = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    write_skill(&src, "pdf", "1.0.0");
    let creds_path = temp.path().join("creds.json");
    let smuggled = "http://localhost:8080@evil.com/";
    std::fs::write(
        &creds_path,
        format!(r#"{{"registries":{{"{smuggled}":{{"token":"pakx_v1_x"}}}}}}"#),
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            smuggled,
            "--credentials-file",
            creds_path.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to use registry base URL",
        ));
}
