//! End-to-end tests for login / whoami / pack / publish / unpublish
//! against a wiremock-backed pakx-registry.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
