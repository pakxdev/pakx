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
