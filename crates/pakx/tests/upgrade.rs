//! End-to-end coverage for `pakx upgrade` against a wiremock server
//! standing in for the GitHub Releases API.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

fn workspace_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

#[tokio::test]
async fn reports_up_to_date_when_release_matches_workspace() {
    let server = MockServer::start().await;
    let tag = format!("v{}", workspace_version());
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": tag,
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("is the latest release"));
}

#[tokio::test]
async fn reports_newer_release_with_upgrade_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v999.999.999",
            "html_url": "https://example.invalid/release-notes",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("A newer pakx is available"))
        .stdout(predicate::str::contains("999.999.999"))
        .stdout(predicate::str::contains("brew upgrade pakx"))
        .stdout(predicate::str::contains("scoop update pakx"))
        .stdout(predicate::str::contains("--tag v999.999.999"));
}

#[tokio::test]
async fn reports_dev_build_when_local_is_newer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v0.0.1",
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Running a dev build?"));
}

#[tokio::test]
async fn surfaces_http_error_when_releases_api_is_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .failure();
}
