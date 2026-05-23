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
        .assert()
        .success()
        .stderr(predicate::str::contains("unpublished alice/pdf@1.0.0"));
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
