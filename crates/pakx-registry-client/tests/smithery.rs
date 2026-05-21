//! Integration tests for `SmitherySource` against wiremock.

use pakx_core::RegistrySource;
use pakx_registry_client::{CacheDir, RegistryError, SmitherySource, Source};
use reqwest::Client;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn source_against(server: &MockServer, cache: &TempDir) -> SmitherySource {
    SmitherySource::with_parts(
        Client::new(),
        &server.uri(),
        CacheDir::with_root(cache.path()),
    )
}

#[tokio::test]
async fn search_returns_packages_from_servers_field() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "qualifiedName": "@acme/one",
                    "displayName": "Acme One",
                    "description": "first server"
                },
                {
                    "qualifiedName": "@acme/two",
                    "description": "second server"
                }
            ]
        })))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let results = src.search("").await.unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].id, "@acme/one");
    assert_eq!(results[0].name, "Acme One");
    assert_eq!(results[0].source, RegistrySource::Smithery);
    assert_eq!(results[1].name, "@acme/two", "falls back to qualifiedName");
    assert_eq!(results[0].version, "latest");
}

#[tokio::test]
async fn fetch_returns_not_found_search_only_at_v01() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    let src = source_against(&server, &cache);
    let err = src.fetch("@acme/whatever").await.unwrap_err();
    assert!(matches!(
        err,
        RegistryError::NotFound {
            source_tag: "smithery",
            ..
        }
    ));
}

#[tokio::test]
async fn search_passes_query_to_registry() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                { "qualifiedName": "@acme/match", "description": "hit" }
            ]
        })))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let results = src.search("acme").await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "@acme/match");
}

#[tokio::test]
async fn search_handles_5xx_error() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let src = source_against(&server, &cache);
    let err = src.search("").await.unwrap_err();
    assert!(matches!(err, RegistryError::Http { .. }));
}

#[tokio::test]
async fn search_caches_within_ttl() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [{ "qualifiedName": "@acme/x" }]
        })))
        .expect(1) // would fail if cache didn't kick in for the 2nd call
        .mount(&server)
        .await;
    let src = source_against(&server, &cache);
    let _ = src.search("foo").await.unwrap();
    let r2 = src.search("foo").await.unwrap();
    assert_eq!(r2.len(), 1);
}
