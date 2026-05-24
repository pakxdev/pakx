//! Source impl for the pakx-registry backend.
//!
//! API: <https://registry.pakx.dev>
//!
//! This is the first-party federated source — packages published via
//! `pakx publish` land here. Public read endpoints (search + detail)
//! require no auth; the authed `pakx_backend` module handles the write
//! side.
//!
//! Endpoints used at v0.1:
//!   GET /api/v1/packages?q=<query>                    -> paginated list
//!   GET /api/v1/packages/{owner}/{name}               -> detail (with versions array, **no** tarballUrl)
//!   GET /api/v1/packages/{owner}/{name}/{version}     -> per-version detail (includes signed tarballUrl)

use async_trait::async_trait;
use pakx_core::{http_client, validate_version, RegistrySource};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::cache::CacheDir;
use crate::errors::RegistryError;
use crate::source::Source;
use crate::types::Package;

/// Production base URL for the pakx-registry backend.
pub const DEFAULT_BASE_URL: &str = "https://registry.pakx.dev";

const TAG: &str = "pakx";

#[derive(Debug, Clone)]
pub struct PakxSource {
    http: Client,
    base_url: String,
    cache: CacheDir,
}

impl PakxSource {
    /// Construct against the production registry with the default cache.
    /// Returns `None` if the cache dir cannot be located on this platform.
    #[must_use]
    pub fn new() -> Option<Self> {
        let cache = CacheDir::default_path()?;
        Some(Self::with_parts(http_client(), DEFAULT_BASE_URL, cache))
    }

    /// Explicit constructor for tests + bespoke deployments.
    #[must_use]
    pub fn with_parts(http: Client, base_url: &str, cache: CacheDir) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            cache,
        }
    }

    fn cache_key_search(&self, query: &str) -> String {
        format!("{TAG}@{}:search:{query}", self.base_url)
    }

    fn cache_key_fetch(&self, id: &str) -> String {
        format!("{TAG}@{}:fetch:{id}", self.base_url)
    }

    /// Per-version detail fetch.
    ///
    /// Calls `GET /api/v1/packages/{owner}/{name}/{version}` and returns
    /// the response decoded into [`PackageVersion`]. The list/detail
    /// endpoint used by [`Source::fetch`] omits `tarballUrl` (it would
    /// be wasteful to mint signed URLs for every version on a list
    /// page); the per-version endpoint is the source-of-truth for the
    /// signed download URL the installer needs.
    ///
    /// **No caching.** The signed `tarballUrl` is short-TTL and caching
    /// it would let stale signatures outlive the registry's grace
    /// window, breaking subsequent installs with a 403 from the blob
    /// storage. Every install fires a fresh request.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::NotFound`] when the registry responds 404
    /// (unknown package or unknown version), [`RegistryError::Http`] for
    /// transport-level failures, and [`RegistryError::Decode`] when the
    /// JSON body cannot be deserialized.
    pub async fn fetch_version(
        &self,
        owner: &str,
        name: &str,
        version: &str,
    ) -> Result<PackageVersion, RegistryError> {
        // Shape-guard the `<version>` segment **before** any encoding /
        // network work. `urlencoding_minimal` follows RFC 3986 §2.3 and
        // leaves `.` unreserved — so a version of `..` would
        // percent-encode to a literal `..` segment that a normalising
        // reverse proxy collapses, silently re-routing the GET to a
        // different endpoint. See `pakx_core::validation` for the
        // shared threat model.
        validate_version(version).map_err(|e| RegistryError::Invalid {
            source_tag: TAG,
            reason: e.to_string(),
        })?;
        let url = format!(
            "{}/api/v1/packages/{}/{}/{}",
            self.base_url,
            urlencoding_minimal(owner),
            urlencoding_minimal(name),
            urlencoding_minimal(version),
        );
        debug!(target: "pakx::registry", %url, "pakx fetch_version");
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|source| RegistryError::Http {
                source_tag: TAG,
                source,
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RegistryError::NotFound {
                source_tag: TAG,
                id: format!("{owner}/{name}@{version}"),
            });
        }
        response
            .error_for_status()
            .map_err(|source| RegistryError::Http {
                source_tag: TAG,
                source,
            })?
            .json::<PackageVersion>()
            .await
            .map_err(|source| RegistryError::Decode {
                source_tag: TAG,
                message: source.to_string(),
            })
    }
}

/// Wire-format of `GET /api/v1/packages/{owner}/{name}/{version}`.
///
/// Mirrors the backend route in
/// `app/api/v1/packages/[owner]/[name]/[version]/route.ts` (see lines
/// 57-65 — `id`, `version`, `sha256`, `sizeBytes`, `publishedAt`,
/// `deprecatedAt`, `tarballUrl`). Unknown fields are tolerated via the
/// `extra` flatten so future additive backend fields don't break the
/// CLI.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageVersion {
    /// Canonical `<owner>/<name>` id (echoed from the backend so callers
    /// can log it without re-stringifying).
    #[serde(default)]
    pub id: Option<String>,
    /// Pinned version (echoed from the URL).
    pub version: String,
    /// Hex sha256 of the tarball bytes. The installer recomputes this
    /// locally and compares before any extraction.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Tarball size in bytes. Informational; the installer caps reads
    /// independently.
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// ISO-8601 publish timestamp.
    #[serde(default)]
    pub published_at: Option<String>,
    /// ISO-8601 deprecation timestamp. When set, this version is in the
    /// 30-day grace window of `pakx unpublish`.
    #[serde(default)]
    pub deprecated_at: Option<String>,
    /// Signed (short-TTL) download URL. **This** is the field the install
    /// resolver downloads from — the list/detail endpoint deliberately
    /// omits it.
    #[serde(default)]
    pub tarball_url: Option<String>,
    #[serde(flatten)]
    #[serde(default)]
    pub extra: serde_json::Map<String, Value>,
}

#[async_trait]
impl Source for PakxSource {
    fn tag(&self) -> RegistrySource {
        RegistrySource::Pakx
    }

    async fn search(&self, query: &str) -> Result<Vec<Package>, RegistryError> {
        let key = self.cache_key_search(query);
        let http = self.http.clone();
        let base_url = self.base_url.clone();
        let q = query.to_owned();
        self.cache
            .get_or_fetch::<Vec<Package>, _, _>(&key, move || async move {
                let url = if q.is_empty() {
                    format!("{base_url}/api/v1/packages")
                } else {
                    format!("{base_url}/api/v1/packages?q={}", urlencoding_minimal(&q))
                };
                debug!(target: "pakx::registry", %url, "pakx search");
                let body: ListResponse = http
                    .get(&url)
                    .send()
                    .await
                    .map_err(|source| RegistryError::Http {
                        source_tag: TAG,
                        source,
                    })?
                    .error_for_status()
                    .map_err(|source| RegistryError::Http {
                        source_tag: TAG,
                        source,
                    })?
                    .json::<ListResponse>()
                    .await
                    .map_err(|source| RegistryError::Decode {
                        source_tag: TAG,
                        message: source.to_string(),
                    })?;
                Ok(body.packages.into_iter().map(list_into_package).collect())
            })
            .await
    }

    async fn fetch(&self, id: &str) -> Result<Package, RegistryError> {
        let key = self.cache_key_fetch(id);
        let http = self.http.clone();
        let base_url = self.base_url.clone();
        let id_owned = id.to_owned();
        self.cache
            .get_or_fetch::<Package, _, _>(&key, move || async move {
                let (owner, name) = split_owner_name(&id_owned).ok_or(RegistryError::NotFound {
                    source_tag: TAG,
                    id: id_owned.clone(),
                })?;
                let url = format!(
                    "{base_url}/api/v1/packages/{}/{}",
                    urlencoding_minimal(owner),
                    urlencoding_minimal(name),
                );
                debug!(target: "pakx::registry", %url, "pakx fetch");
                let response =
                    http.get(&url)
                        .send()
                        .await
                        .map_err(|source| RegistryError::Http {
                            source_tag: TAG,
                            source,
                        })?;
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    return Err(RegistryError::NotFound {
                        source_tag: TAG,
                        id: id_owned,
                    });
                }
                let detail: DetailResponse = response
                    .error_for_status()
                    .map_err(|source| RegistryError::Http {
                        source_tag: TAG,
                        source,
                    })?
                    .json::<DetailResponse>()
                    .await
                    .map_err(|source| RegistryError::Decode {
                        source_tag: TAG,
                        message: source.to_string(),
                    })?;
                Ok(detail_into_package(detail, id_owned))
            })
            .await
    }
}

/// Split a federated id into `(owner, name)` at the **first** `/`.
///
/// Historically this helper rejected any id containing more than one
/// `/` — the rule existed to keep the pakx-registry routes (which
/// canonically take `<owner>/<name>` with single-segment names)
/// well-formed. That single-slash hard-cap is too strict in practice:
/// `pakx add io.github.acme/srv-name` is a valid id shape on the
/// federated MCP registry, and the upstream caller in
/// `commands::add::probe_pakx_kind` short-circuits on `NotFound` from
/// the pakx source before falling back to MCP. Returning `NotFound`
/// for the `io.github.acme/srv-name` shape silently broke that
/// fallback whenever a pakx-side `404` would have been the right
/// "skip me" signal.
///
/// The relaxed split takes everything before the first `/` as the
/// owner and everything after as the name. The pakx-registry's own
/// `ownerLogin` validation regex restricts owners to
/// alphanumeric+dash, so a real pakx package can never collide with
/// dotted MCP-style owner segments — the relaxation is unambiguous
/// for actually-pakx ids and harmless for federated ones (the
/// registry will just `404`).
fn split_owner_name(id: &str) -> Option<(&str, &str)> {
    let (owner, rest) = id.split_once('/')?;
    if owner.is_empty() || rest.is_empty() {
        return None;
    }
    Some((owner, rest))
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct ListResponse {
    #[serde(default)]
    packages: Vec<ListEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct ListEntry {
    /// Canonical `<owner>/<name>` id.
    id: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "latestVersion")]
    latest_version: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct DetailResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    versions: Vec<VersionEntry>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct VersionEntry {
    version: String,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

fn list_into_package(raw: ListEntry) -> Package {
    let version = raw.latest_version.unwrap_or_else(|| "0.0.0".to_string());
    let mut hints = raw.extra;
    if let Some(k) = raw.kind {
        hints.insert("kind".into(), Value::String(k));
    }
    Package {
        id: raw.id.clone(),
        source: RegistrySource::Pakx,
        name: raw.id,
        version,
        description: raw.description,
        install_hints: Value::Object(hints),
    }
}

fn detail_into_package(raw: DetailResponse, fallback_id: String) -> Package {
    let id = raw.id.unwrap_or(fallback_id);
    let version = raw
        .versions
        .first()
        .map_or_else(|| "0.0.0".to_string(), |v| v.version.clone());
    let mut hints = raw.extra;
    if let Some(k) = raw.kind {
        hints.insert("kind".into(), Value::String(k));
    }
    let versions_json = raw
        .versions
        .iter()
        .map(|v| {
            let mut m = v.extra.clone();
            m.insert("version".into(), Value::String(v.version.clone()));
            Value::Object(m)
        })
        .collect();
    hints.insert("versions".into(), Value::Array(versions_json));
    Package {
        id: id.clone(),
        source: RegistrySource::Pakx,
        name: id,
        version,
        description: raw.description,
        install_hints: Value::Object(hints),
    }
}

/// Minimal percent-encoder for URL path segments. Avoids the
/// `urlencoding` crate to keep dep count low.
fn urlencoding_minimal(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}
