//! Integration tests for `OfficialMcpSource` against a wiremock server.

use pakx_core::RegistrySource;
use pakx_registry_client::{CacheDir, OfficialMcpSource, RegistryError, Source};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn source_with_mock(base_url: &str, cache_root: &std::path::Path) -> OfficialMcpSource {
    OfficialMcpSource::with_parts(Client::new(), base_url, CacheDir::with_root(cache_root))
}

#[tokio::test]
async fn search_returns_empty_when_registry_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "servers": [], "next": null })),
        )
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let source = source_with_mock(&server.uri(), temp.path());

    let results = source.search("").await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn search_decodes_listed_servers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "name": "io.github.modelcontextprotocol/server-filesystem",
                    "description": "Local filesystem access",
                    "version_detail": { "version": "1.2.3" },
                    "packages": [{ "registry": "npm", "name": "@mcp/server-filesystem" }]
                },
                {
                    "name": "io.github.acme/playwright",
                    "version": "0.5.0"
                }
            ],
            "next": null
        })))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let source = source_with_mock(&server.uri(), temp.path());

    let results = source.search("").await.unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].id,
        "io.github.modelcontextprotocol/server-filesystem"
    );
    assert_eq!(results[0].version, "1.2.3");
    assert_eq!(results[0].source, RegistrySource::OfficialMcp);
    assert_eq!(results[1].id, "io.github.acme/playwright");
    assert_eq!(results[1].version, "0.5.0");
}

#[tokio::test]
async fn search_with_query_forwards_query_param() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .and(query_param("search", "filesystem"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [{ "name": "io.github.modelcontextprotocol/server-filesystem" }]
        })))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let source = source_with_mock(&server.uri(), temp.path());
    let results = source.search("filesystem").await.unwrap();
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn fetch_returns_single_package() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/v0/servers/io.github.modelcontextprotocol/server-filesystem",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "io.github.modelcontextprotocol/server-filesystem",
            "description": "Local fs",
            "version_detail": { "version": "1.2.3" }
        })))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let source = source_with_mock(&server.uri(), temp.path());

    let pkg = source
        .fetch("io.github.modelcontextprotocol/server-filesystem")
        .await
        .unwrap();
    assert_eq!(pkg.id, "io.github.modelcontextprotocol/server-filesystem");
    assert_eq!(pkg.version, "1.2.3");
}

#[tokio::test]
async fn fetch_404_returns_not_found_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers/ghost/server"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let source = source_with_mock(&server.uri(), temp.path());
    let err = source.fetch("ghost/server").await.unwrap_err();
    assert!(matches!(err, RegistryError::NotFound { .. }));
}

#[tokio::test]
async fn search_malformed_body_returns_decode_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let source = source_with_mock(&server.uri(), temp.path());
    let err = source.search("").await.unwrap_err();
    assert!(matches!(err, RegistryError::Decode { .. }));
}

#[tokio::test]
async fn second_search_is_served_from_cache() {
    let server = MockServer::start().await;
    // Mount with `expect(1)` so any second hit on the real upstream fails.
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [{ "name": "x/y", "version": "1.0.0" }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let source = source_with_mock(&server.uri(), temp.path());

    let r1 = source.search("").await.unwrap();
    let r2 = source.search("").await.unwrap();
    assert_eq!(r1, r2);
    // wiremock verifies the expect(1) on drop.
}

#[tokio::test]
async fn search_decodes_wrapped_schema() {
    // 2025-12-11 schema wraps each list entry as `{server, _meta}`.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "server": {
                        "name": "io.github.modelcontextprotocol/server-filesystem",
                        "description": "Local fs",
                        "version": "1.2.3",
                        "remotes": [{ "type": "streamable-http", "url": "https://x" }]
                    },
                    "_meta": {
                        "io.modelcontextprotocol.registry/official": {
                            "status": "active",
                            "isLatest": true
                        }
                    }
                },
                {
                    "server": { "name": "io.github.acme/playwright", "version": "0.5.0" },
                    "_meta": {}
                }
            ],
            "next": null
        })))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let source = source_with_mock(&server.uri(), temp.path());

    let results = source.search("").await.unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].id,
        "io.github.modelcontextprotocol/server-filesystem"
    );
    assert_eq!(results[0].version, "1.2.3");
    assert_eq!(results[0].description.as_deref(), Some("Local fs"));
    // `_meta` is preserved on `install_hints` so the resolver can inspect it.
    assert!(results[0].install_hints.get("_meta").is_some());
    // `remotes` from the inner `server` is also preserved on `install_hints`.
    assert!(results[0].install_hints.get("remotes").is_some());
    assert_eq!(results[1].id, "io.github.acme/playwright");
}

#[tokio::test]
async fn fetch_decodes_wrapped_schema() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers/io.github.foo/bar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "server": {
                "name": "io.github.foo/bar",
                "version_detail": { "version": "2.0.0" }
            },
            "_meta": { "io.modelcontextprotocol.registry/official": { "status": "active" } }
        })))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let source = source_with_mock(&server.uri(), temp.path());
    let pkg = source.fetch("io.github.foo/bar").await.unwrap();
    assert_eq!(pkg.id, "io.github.foo/bar");
    assert_eq!(pkg.version, "2.0.0");
    assert!(pkg.install_hints.get("_meta").is_some());
}

#[tokio::test]
async fn cache_ttl_expiry_refetches() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [{ "name": "x/y", "version": "1.0.0" }]
        })))
        // Expect at least 2 hits — first populates cache, second triggers
        // because TTL has expired by then.
        .expect(2)
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path()).with_ttl(Duration::from_millis(50));
    let source = OfficialMcpSource::with_parts(Client::new(), &server.uri(), cache);

    let _ = source.search("").await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    let _ = source.search("").await.unwrap();
}
