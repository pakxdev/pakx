//! Integration tests for `PakxSource` against wiremock.

use pakx_core::RegistrySource;
use pakx_registry_client::{CacheDir, PakxSource, RegistryError, Source};
use reqwest::Client;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn source_against(server: &MockServer, cache: &TempDir) -> PakxSource {
    PakxSource::with_parts(
        Client::new(),
        &server.uri(),
        CacheDir::with_root(cache.path()),
    )
}

#[tokio::test]
async fn search_returns_empty_when_registry_empty() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "packages": [] })),
        )
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let results = src.search("").await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn search_maps_list_entries() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packages": [
                {
                    "id": "acme/one",
                    "kind": "skill",
                    "description": "first",
                    "latestVersion": "1.2.3"
                },
                {
                    "id": "acme/two",
                    "kind": "mcp",
                    "description": null,
                    "latestVersion": null
                }
            ]
        })))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let results = src.search("").await.unwrap();
    assert_eq!(results.len(), 2);

    assert_eq!(results[0].id, "acme/one");
    assert_eq!(results[0].name, "acme/one");
    assert_eq!(results[0].source, RegistrySource::Pakx);
    assert_eq!(results[0].version, "1.2.3");
    assert_eq!(results[0].description.as_deref(), Some("first"));
    assert_eq!(
        results[0].install_hints.get("kind").and_then(|v| v.as_str()),
        Some("skill")
    );

    // Missing latestVersion → fallback "0.0.0".
    assert_eq!(results[1].version, "0.0.0");
    assert!(results[1].description.is_none());
}

#[tokio::test]
async fn search_forwards_query_param() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages"))
        .and(query_param("q", "foo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packages": [{ "id": "acme/foo", "latestVersion": "0.1.0" }]
        })))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let results = src.search("foo").await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "acme/foo");
}

#[tokio::test]
async fn fetch_returns_detail_with_latest_version() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/acme/one"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "acme/one",
            "kind": "skill",
            "description": "the one",
            "createdAt": "2026-05-21T00:00:00Z",
            "versions": [
                { "version": "1.2.3", "sha256": "abc", "sizeBytes": 1024 },
                { "version": "1.2.2", "sha256": "def", "sizeBytes": 1000 }
            ]
        })))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let pkg = src.fetch("acme/one").await.unwrap();
    assert_eq!(pkg.id, "acme/one");
    assert_eq!(pkg.version, "1.2.3");
    assert_eq!(pkg.source, RegistrySource::Pakx);
    // Versions are preserved in install_hints for the resolver.
    let versions = pkg
        .install_hints
        .get("versions")
        .and_then(|v| v.as_array())
        .expect("versions array");
    assert_eq!(versions.len(), 2);
    assert_eq!(
        versions[0].get("sha256").and_then(|v| v.as_str()),
        Some("abc")
    );
}

#[tokio::test]
async fn fetch_404_returns_not_found_error() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/ghost/server"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let err = src.fetch("ghost/server").await.unwrap_err();
    assert!(matches!(
        err,
        RegistryError::NotFound {
            source_tag: "pakx",
            ..
        }
    ));
}

#[tokio::test]
async fn fetch_rejects_malformed_id() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    let src = source_against(&server, &cache);

    for bad in ["", "no-slash", "/leading", "trailing/", "a/b/c"] {
        let err = src.fetch(bad).await.unwrap_err();
        assert!(
            matches!(err, RegistryError::NotFound { source_tag: "pakx", .. }),
            "id {bad:?} should be rejected as malformed"
        );
    }
    // No request should have been made to the mock server.
    assert!(server.received_requests().await.unwrap().is_empty());
}
