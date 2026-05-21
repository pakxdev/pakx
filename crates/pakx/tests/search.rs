//! Integration tests for `pakx search` against a wiremock-backed registry.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

#[tokio::test]
async fn search_lists_packages_from_registry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                { "name": "io.github.acme/one", "description": "first", "version_detail": {"version": "1.0.0"} },
                { "name": "io.github.acme/two", "description": "second", "version_detail": {"version": "2.0.0"} }
            ]
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args(["search", "--mcp-base-url", &server.uri()])
        .assert()
        .success()
        .stdout(predicate::str::contains("io.github.acme/one"))
        .stdout(predicate::str::contains("io.github.acme/two"));
}

#[tokio::test]
async fn search_with_query_passes_through_to_registry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                { "name": "io.github.acme/match", "description": "hit", "version_detail": {"version": "1.0.0"} }
            ]
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args(["search", "acme", "--mcp-base-url", &server.uri()])
        .assert()
        .success()
        .stdout(predicate::str::contains("io.github.acme/match"));
}

#[tokio::test]
async fn search_empty_results_prints_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args(["search", "ghost", "--mcp-base-url", &server.uri()])
        .assert()
        .success()
        .stderr(predicate::str::contains("no results"));
}
