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
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "packages": [] })))
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
        results[0]
            .install_hints
            .get("kind")
            .and_then(|v| v.as_str()),
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

/// Regression for the 2026-05 federated-search incident: against
/// production, `pakx search hello-world --json` returned 10 Smithery
/// hits and **zero** pakx-registry hits even though
/// `arwenizEr/hello-world@0.1.1` was live. After the registry-side
/// `latestVersion` subquery fix, the list endpoint now returns the
/// shape pinned below — `id`, `kind`, `description`, `visibility`,
/// `latestVersion` per package — and the CLI must surface the entry
/// with the registry-supplied version (not the `"0.0.0"` fallback).
///
/// `visibility` is not a field the CLI decodes directly but rides
/// through the `extra` flatten so additive backend fields don't break
/// the decoder. Pinning the full prod shape here means future backend
/// renames trip this test, not silently degrade the federated merge.
#[tokio::test]
async fn search_surfaces_prod_list_shape_with_latest_version() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages"))
        .and(query_param("q", "hello-world"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packages": [
                {
                    "id": "arwenizEr/hello-world",
                    "kind": "skill",
                    "description": "first published pakx skill",
                    "visibility": "public",
                    "latestVersion": "0.1.1"
                }
            ]
        })))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let results = src.search("hello-world").await.unwrap();
    assert_eq!(results.len(), 1, "list endpoint hit must surface");
    let pkg = &results[0];
    assert_eq!(pkg.id, "arwenizEr/hello-world");
    assert_eq!(pkg.name, "arwenizEr/hello-world");
    assert_eq!(pkg.source, RegistrySource::Pakx);
    assert_eq!(
        pkg.version, "0.1.1",
        "must use registry latestVersion, not 0.0.0 fallback"
    );
    assert_eq!(
        pkg.description.as_deref(),
        Some("first published pakx skill")
    );
    // `kind` rides into install_hints alongside the flatten-captured
    // `visibility` field.
    assert_eq!(
        pkg.install_hints.get("kind").and_then(|v| v.as_str()),
        Some("skill")
    );
    assert_eq!(
        pkg.install_hints.get("visibility").and_then(|v| v.as_str()),
        Some("public")
    );
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

/// `GET /api/v1/packages/{owner}/{name}` includes a `sponsors:` array
/// in the body since Phase X2a. `PakxSource::fetch` doesn't model the
/// field directly (it lives on `info.rs`'s decoder), but the
/// `#[serde(flatten)] extra` capture means sponsors must ride through
/// `install_hints` unchanged so downstream consumers (e.g. the
/// `pakx-web` detail page in Phase 2c) can read them off the federated
/// search hits without a second API roundtrip.
#[tokio::test]
async fn fetch_carries_sponsors_through_install_hints() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/alice/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "kind": "skill",
            "description": "hi",
            "sponsors": [
                { "kind": "github", "url": "https://github.com/sponsors/alice" },
                { "kind": "url", "url": "https://opencollective.com/alice" }
            ],
            "versions": [ { "version": "0.1.0", "sha256": "abc", "sizeBytes": 100 } ]
        })))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let pkg = src.fetch("alice/hello").await.unwrap();
    let sponsors = pkg
        .install_hints
        .get("sponsors")
        .and_then(|v| v.as_array())
        .expect("sponsors array surfaces via extra flatten");
    assert_eq!(sponsors.len(), 2);
    assert_eq!(
        sponsors[0].get("kind").and_then(|v| v.as_str()),
        Some("github")
    );
    assert_eq!(
        sponsors[0].get("url").and_then(|v| v.as_str()),
        Some("https://github.com/sponsors/alice"),
    );
    assert_eq!(
        sponsors[1].get("kind").and_then(|v| v.as_str()),
        Some("url")
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
async fn fetch_version_returns_per_version_metadata_with_tarball_url() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/alice/hello/0.1.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "version": "0.1.1",
            "sha256": "abc123",
            "sizeBytes": 4321,
            "publishedAt": "2026-05-22T00:00:00Z",
            "deprecatedAt": null,
            "tarballUrl": "https://blob.example/abc?sig=XYZ"
        })))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let pv = src.fetch_version("alice", "hello", "0.1.1").await.unwrap();
    assert_eq!(pv.version, "0.1.1");
    assert_eq!(pv.sha256.as_deref(), Some("abc123"));
    assert_eq!(pv.size_bytes, Some(4321));
    assert_eq!(
        pv.tarball_url.as_deref(),
        Some("https://blob.example/abc?sig=XYZ")
    );
}

#[tokio::test]
async fn fetch_version_404_returns_not_found_error() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/ghost/server/9.9.9"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let err = src
        .fetch_version("ghost", "server", "9.9.9")
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RegistryError::NotFound {
            source_tag: "pakx",
            ..
        }
    ));
}

#[tokio::test]
async fn fetch_version_tolerates_missing_tarball_url() {
    // The wire-format tolerates a missing `tarballUrl` (the field is
    // `Option<String>`); the resolver enforces the "must be present"
    // contract at a higher layer with a precise error message. This
    // test pins that the per-version decode itself is permissive so
    // the resolver — not the deserializer — owns the diagnostic.
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/alice/hello/0.1.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "version": "0.1.0",
            "sha256": "abc",
            "sizeBytes": 100
        })))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let pv = src.fetch_version("alice", "hello", "0.1.0").await.unwrap();
    assert!(pv.tarball_url.is_none());
    assert_eq!(pv.sha256.as_deref(), Some("abc"));
}

#[tokio::test]
async fn fetch_rejects_malformed_id() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    let src = source_against(&server, &cache);

    // Note: `a/b/c` (multi-slash) is no longer rejected pre-network.
    // The split takes everything before the first `/` as owner; the
    // remainder is the name. See the `split_owner_name` doc comment
    // for the rationale — short-circuiting multi-slash ids broke the
    // `pakx add` federated MCP fallback for dotted owner shapes like
    // `io.github.acme/srv-name`. Multi-slash ids are forwarded to the
    // registry which `404`s them naturally.
    for bad in ["", "no-slash", "/leading", "trailing/"] {
        let err = src.fetch(bad).await.unwrap_err();
        assert!(
            matches!(
                err,
                RegistryError::NotFound {
                    source_tag: "pakx",
                    ..
                }
            ),
            "id {bad:?} should be rejected as malformed"
        );
    }
    // No request should have been made to the mock server.
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Regression test for the round-47 split-owner-name relaxation.
///
/// Ids with more than one `/` (e.g. `io.github.acme/srv-name`) used
/// to be rejected pre-network as malformed, which caused
/// `commands::add::probe_pakx_kind` to short-circuit and skip the
/// MCP-fallback fire. The relaxation forwards the request and lets
/// the registry return its own `404` (or `200` if the id happens to
/// match a real pakx package — but the registry's owner-login
/// validation regex makes that impossible for dotted owners).
///
/// Asserts:
///   - The request actually reaches the mock server with the full id
///     percent-encoded into the URL.
///   - The `404` response surfaces as `RegistryError::NotFound` so
///     the upstream `Ok(None)` mapping fires unchanged.
#[tokio::test]
async fn fetch_forwards_multi_slash_id_and_surfaces_registry_404() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    // The first `/` splits owner from name; the rest of the id is the
    // name (which gets percent-encoded for the URL — `/` → `%2F`).
    Mock::given(method("GET"))
        .and(path("/api/v1/packages/io.github.acme/srv-name%2Fextra"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let src = source_against(&server, &cache);
    let err = src
        .fetch("io.github.acme/srv-name/extra")
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RegistryError::NotFound {
            source_tag: "pakx",
            ..
        }
    ));
    // The request must have actually fired — that is the whole point
    // of the relaxation.
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

/// `fetch_version` must shape-guard the `<version>` URL segment BEFORE
/// firing any HTTP request. A version of `..` percent-encodes to a
/// literal `..` segment (RFC 3986 leaves dots alone) that a CDN /
/// normalising reverse proxy would collapse upward, silently rerouting
/// the GET to the package-detail endpoint instead of the version
/// endpoint. Pin the guard with a hostile input set + assert the mock
/// server saw zero traffic.
#[tokio::test]
async fn fetch_version_rejects_hostile_version_pre_network() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    let src = source_against(&server, &cache);

    for bad in ["", "..", "../etc", "1..0", "-1.0.0", "1.0 0", "1.0/0"] {
        let err = src.fetch_version("alice", "demo", bad).await.unwrap_err();
        assert!(
            matches!(
                err,
                RegistryError::Invalid {
                    source_tag: "pakx",
                    ..
                }
            ),
            "version {bad:?} must be rejected as invalid, got {err:?}",
        );
    }
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "validator must fire before any HTTP request",
    );
}

/// Owner-half companion of the version guard above. The encoder leaves
/// `.` unreserved per RFC 3986 so a manifest with `owner: ".."` would
/// otherwise reach the wire as `GET /api/v1/packages/../<name>/<ver>` —
/// a normalising CDN collapses the segment upward and silently re-routes
/// the GET to a different endpoint. Pin the guard with a hostile input
/// set + assert the mock server saw zero traffic.
#[tokio::test]
async fn fetch_version_rejects_hostile_owner_pre_network() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    let src = source_against(&server, &cache);

    for bad in [
        "", ".", "..", "../foo", ".hidden", "foo..bar", "a/b", "a\\b", "a\nb",
    ] {
        let err = src.fetch_version(bad, "demo", "1.0.0").await.unwrap_err();
        assert!(
            matches!(
                err,
                RegistryError::Invalid {
                    source_tag: "pakx",
                    ..
                }
            ),
            "owner {bad:?} must be rejected as invalid, got {err:?}",
        );
    }
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "validator must fire before any HTTP request",
    );
}

/// Light-weight traversal guard on the NAME half of `Source::fetch`.
/// The round-47 split-owner-name relaxation forwards multi-slash ids
/// like `io.github.acme/srv-name/extra` to the registry so MCP fallback
/// works — `urlencoding_minimal` encodes the embedded `/` to `%2F`, so
/// the multi-slash name reaches the wire as a single percent-encoded
/// segment. But `.` is RFC 3986 unreserved and survives the encoder, so
/// a literal `..` substring in the name would still reach the wire and
/// a normalising CDN would collapse it upward. This narrow guard rejects
/// just the `..` shape while keeping the multi-slash relaxation intact.
#[tokio::test]
async fn fetch_rejects_dotdot_in_name_pre_network() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    let src = source_against(&server, &cache);

    for bad in ["acme/..", "acme/.", "acme/../foo", "acme/foo..bar"] {
        let err = src.fetch(bad).await.unwrap_err();
        assert!(
            matches!(
                err,
                RegistryError::Invalid {
                    source_tag: "pakx",
                    ..
                }
            ),
            "id {bad:?} must be rejected as invalid, got {err:?}",
        );
    }
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "validator must fire before any HTTP request",
    );
}

/// Same owner-half threat model applied to `Source::fetch` — the
/// list/detail endpoint also has an `<owner>` segment that a hostile
/// `agents.yml` shorthand pin (`pakx/../foo/bar`) could otherwise probe.
/// The fetch path is wrapped in a cache layer so the guard must fire
/// before the cache key is consulted (a poisoned `..` key would otherwise
/// persist on disk).
#[tokio::test]
async fn fetch_rejects_hostile_owner_pre_network() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    let src = source_against(&server, &cache);

    for bad in [
        "../foo/bar",
        "..hidden/name",
        ".hidden/name",
        "a\\b/demo",
        "a\nb/demo",
    ] {
        let err = src.fetch(bad).await.unwrap_err();
        assert!(
            matches!(
                err,
                RegistryError::Invalid {
                    source_tag: "pakx",
                    ..
                }
            ),
            "id {bad:?} must be rejected as invalid, got {err:?}",
        );
    }
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "validator must fire before any HTTP request",
    );
}
