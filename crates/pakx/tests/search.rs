//! Integration tests for `pakx search` against a wiremock-backed registry.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
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
        .args(["search", "--mcp-base-url", &server.uri(), "--no-smithery"])
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
        .args([
            "search",
            "acme",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("io.github.acme/match"));
}

#[tokio::test]
async fn search_federates_across_official_mcp_and_smithery() {
    // Two separate wiremock servers, one per source.
    let mcp = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                { "name": "io.github.acme/mcp-side", "description": "from mcp", "version_detail": {"version": "1.0.0"} }
            ]
        })))
        .mount(&mcp)
        .await;
    let sm = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                { "qualifiedName": "@acme/smithery-side", "description": "from smithery" }
            ]
        })))
        .mount(&sm)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "--mcp-base-url",
            &mcp.uri(),
            "--smithery-base-url",
            &sm.uri(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("io.github.acme/mcp-side"))
        .stdout(predicate::str::contains("@acme/smithery-side"))
        .stdout(predicate::str::contains("smithery"));
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
        .args([
            "search",
            "ghost",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("no results"));
}

#[tokio::test]
async fn search_json_emits_valid_array_with_expected_keys() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "name": "io.github.acme/one",
                    "description": "first hit",
                    "version_detail": {"version": "1.0.0"}
                },
                {
                    "name": "io.github.acme/two",
                    "description": "second hit",
                    "version_detail": {"version": "2.0.0"}
                }
            ]
        })))
        .mount(&server)
        .await;

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            // Canonical flag name (2026-05 rename from `--no-pakx`).
            // The remaining `--no-pakx` references in this file
            // intentionally exercise the deprecated alias path.
            "--no-pakx-registry",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.ends_with('\n'), "stdout must end with newline");
    let body = stdout.trim_end_matches('\n');
    assert!(!body.contains('\n'), "json output must be single-line");

    let parsed: Value = serde_json::from_str(body).expect("json parses");
    let arr = parsed.as_array().expect("top level is an array");
    assert_eq!(arr.len(), 2);
    let first = &arr[0];
    for key in ["id", "name", "version", "source"] {
        assert!(first.get(key).is_some(), "missing key {key:?} in {first:?}");
    }
    assert_eq!(first["source"], "official-mcp");
    assert!(arr
        .iter()
        .any(|h| h["name"] == "io.github.acme/one" && h["version"] == "1.0.0"));
    assert!(arr
        .iter()
        .any(|h| h["name"] == "io.github.acme/two" && h["version"] == "2.0.0"));
}

/// `--no-pakx` was renamed to `--no-pakx-registry` to match the flag on
/// `pakx install` / `pakx test`. The old spelling is kept as a hidden
/// alias for one release so scripts continue to work. This regression
/// pins both spellings parsing identically — when the alias is removed
/// in v0.2, this test will fail loudly and the removal is documented.
#[tokio::test]
async fn search_accepts_deprecated_no_pakx_alias() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                { "name": "io.github.acme/x", "description": "x", "version_detail": {"version": "1.0.0"} }
            ]
        })))
        .mount(&server)
        .await;

    // Old alias path.
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx",
            "--json",
        ])
        .assert()
        .success();

    // Canonical path.
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
            "--json",
        ])
        .assert()
        .success();
}

#[tokio::test]
async fn search_json_empty_results_emits_empty_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&server)
        .await;

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "ghost",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert_eq!(stdout.trim_end(), "[]");
}

#[tokio::test]
async fn search_json_description_always_present_even_when_upstream_omits_it() {
    // Contract: `description` is always emitted as a string. When the
    // upstream hit has no description (or an explicit `null`), pakx
    // emits an empty string so `jq '.description'` never returns
    // `null` and the field shape is invariant.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "name": "io.github.acme/no-desc",
                    "version_detail": {"version": "1.0.0"}
                },
                {
                    "name": "io.github.acme/null-desc",
                    "description": null,
                    "version_detail": {"version": "1.0.0"}
                },
                {
                    "name": "io.github.acme/empty-desc",
                    "description": "",
                    "version_detail": {"version": "1.0.0"}
                },
                {
                    "name": "io.github.acme/real-desc",
                    "description": "hello",
                    "version_detail": {"version": "1.0.0"}
                }
            ]
        })))
        .mount(&server)
        .await;

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let arr = parsed.as_array().expect("top level is an array");
    for hit in arr {
        let desc = hit
            .get("description")
            .expect("description must always be present");
        assert!(
            desc.is_string(),
            "description must be a string, got {desc:?}",
        );
    }
    let by_name: std::collections::HashMap<&str, &str> = arr
        .iter()
        .map(|h| {
            (
                h["name"].as_str().unwrap(),
                h["description"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(by_name.get("io.github.acme/no-desc"), Some(&""));
    assert_eq!(by_name.get("io.github.acme/null-desc"), Some(&""));
    assert_eq!(by_name.get("io.github.acme/empty-desc"), Some(&""));
    assert_eq!(by_name.get("io.github.acme/real-desc"), Some(&"hello"));
}

#[tokio::test]
async fn search_json_respects_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                { "name": "io.github.acme/a", "description": "a", "version_detail": {"version": "1.0.0"} },
                { "name": "io.github.acme/b", "description": "b", "version_detail": {"version": "1.0.0"} },
                { "name": "io.github.acme/c", "description": "c", "version_detail": {"version": "1.0.0"} }
            ]
        })))
        .mount(&server)
        .await;
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx",
            "--json",
            "-n",
            "2",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 2);
}
