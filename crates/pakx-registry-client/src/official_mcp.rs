//! Source impl for the official Model Context Protocol Registry.
//!
//! API: <https://registry.modelcontextprotocol.io>
//!
//! Endpoints used at v0.1:
//!   GET /v0/servers              -> paginated list of registered servers
//!   GET /v0/servers/{id}         -> single server, full detail
//!
//! Schema is decoded permissively: the registry adds fields over time,
//! and we only care about id, name, version, description for v0.1
//! search results. Everything else lives in `install_hints` as raw JSON.

use async_trait::async_trait;
use pakx_core::RegistrySource;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::cache::CacheDir;
use crate::errors::RegistryError;
use crate::source::Source;
use crate::types::Package;

/// Production base URL for the official MCP Registry.
pub const DEFAULT_BASE_URL: &str = "https://registry.modelcontextprotocol.io";

const TAG: &str = "official-mcp";

#[derive(Debug, Clone)]
pub struct OfficialMcpSource {
    http: Client,
    base_url: String,
    cache: CacheDir,
}

impl OfficialMcpSource {
    /// Construct against the production registry with the default cache.
    /// Returns `None` if the cache dir cannot be located on this platform.
    #[must_use]
    pub fn new() -> Option<Self> {
        let cache = CacheDir::default_path()?;
        Some(Self::with_parts(Client::new(), DEFAULT_BASE_URL, cache))
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
}

#[async_trait]
impl Source for OfficialMcpSource {
    fn tag(&self) -> RegistrySource {
        RegistrySource::OfficialMcp
    }

    async fn search(&self, query: &str) -> Result<Vec<Package>, RegistryError> {
        let key = self.cache_key_search(query);
        let http = self.http.clone();
        let base_url = self.base_url.clone();
        let q = query.to_owned();
        self.cache
            .get_or_fetch::<Vec<Package>, _, _>(&key, move || async move {
                let url = if q.is_empty() {
                    format!("{base_url}/v0/servers")
                } else {
                    format!("{base_url}/v0/servers?search={}", urlencoding_minimal(&q))
                };
                debug!(target: "pakx::registry", %url, "official-mcp search");
                let body: ServerListResponse = http
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
                    .json::<ServerListResponse>()
                    .await
                    .map_err(|source| RegistryError::Decode {
                        source_tag: TAG,
                        message: source.to_string(),
                    })?;

                Ok(body.servers.into_iter().map(into_package).collect())
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
                // Try the per-server detail endpoint first. The 2025-12-11
                // schema dropped it (404 for every id), but older
                // deployments still expose it. If we get 200, decode and
                // return. Otherwise fall through to search-and-filter.
                let direct = format!("{base_url}/v0/servers/{}", urlencoding_minimal(&id_owned));
                debug!(target: "pakx::registry", url=%direct, "official-mcp fetch");
                let response =
                    http.get(&direct)
                        .send()
                        .await
                        .map_err(|source| RegistryError::Http {
                            source_tag: TAG,
                            source,
                        })?;
                if response.status().is_success() {
                    let raw: ServerRaw = response.json::<ServerRaw>().await.map_err(|source| {
                        RegistryError::Decode {
                            source_tag: TAG,
                            message: source.to_string(),
                        }
                    })?;
                    return Ok(into_package(raw));
                }
                if response.status() != reqwest::StatusCode::NOT_FOUND
                    && response.status() != reqwest::StatusCode::METHOD_NOT_ALLOWED
                {
                    let _ = response
                        .error_for_status()
                        .map_err(|source| RegistryError::Http {
                            source_tag: TAG,
                            source,
                        })?;
                }

                // Fallback: hit /v0/servers?search=<id> and pick the
                // entry whose canonical name equals `id`. The search
                // endpoint still works against the 2025-12-11 schema.
                let search_url = format!(
                    "{base_url}/v0/servers?search={}",
                    urlencoding_minimal(&id_owned)
                );
                debug!(target: "pakx::registry", url=%search_url, "official-mcp fetch fallback");
                let body: ServerListResponse = http
                    .get(&search_url)
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
                    .json::<ServerListResponse>()
                    .await
                    .map_err(|source| RegistryError::Decode {
                        source_tag: TAG,
                        message: source.to_string(),
                    })?;
                // Collect every server whose canonical id matches.
                // The 2025-12-11 schema can return multiple entries with
                // the same name (different versions, different sub-pages
                // of the result set). Previously we kept the first one,
                // which made re-fetches non-deterministic and could pin
                // a `0.0.0` placeholder when a real version was also
                // available. Prefer entries with a non-placeholder
                // version, tie-breaking on lexicographic version desc
                // so a stable highest-version entry wins.
                let mut matches: Vec<Package> = body
                    .servers
                    .into_iter()
                    .map(into_package)
                    .filter(|p| p.id == id_owned)
                    .collect();
                matches.sort_by(|a, b| {
                    let a_placeholder = a.version == "0.0.0";
                    let b_placeholder = b.version == "0.0.0";
                    a_placeholder
                        .cmp(&b_placeholder)
                        .then_with(|| b.version.cmp(&a.version))
                });
                matches.into_iter().next().ok_or(RegistryError::NotFound {
                    source_tag: TAG,
                    id: id_owned,
                })
            })
            .await
    }
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct ServerListResponse {
    #[serde(default)]
    servers: Vec<ServerRaw>,
    // `next` cursor token; pagination is intentionally not used at v0.1.
    #[serde(default)]
    #[allow(dead_code)]
    next: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ServerCore {
    /// Canonical id. The MCP Registry sends this as `name` (e.g.
    /// `io.github.modelcontextprotocol/server-filesystem`); older or
    /// alternate deployments may send `id`.
    #[serde(alias = "id")]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    version_detail: Option<VersionDetail>,
    #[serde(default)]
    version: Option<String>,
    /// Everything we don't model explicitly is preserved for the resolver.
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

/// Wire format for a single server entry. The 2025-12-11 schema wraps
/// every entry in `{ "server": <core>, "_meta": {...} }`; older
/// deployments still send the flat core directly. Accept both.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ServerRaw {
    Wrapped {
        server: ServerCore,
        #[serde(rename = "_meta", default)]
        meta: Option<Value>,
    },
    Flat(ServerCore),
}

impl ServerRaw {
    fn into_parts(self) -> (ServerCore, Option<Value>) {
        match self {
            Self::Wrapped { server, meta } => (server, meta),
            Self::Flat(core) => (core, None),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct VersionDetail {
    #[serde(default)]
    version: Option<String>,
}

fn into_package(raw: ServerRaw) -> Package {
    let (core, meta) = raw.into_parts();
    let version = core
        .version
        .or_else(|| core.version_detail.and_then(|v| v.version))
        .unwrap_or_else(|| "0.0.0".to_string());
    let mut extra = core.extra;
    if let Some(m) = meta {
        extra.insert("_meta".to_owned(), m);
    }
    Package {
        id: core.name.clone(),
        source: RegistrySource::OfficialMcp,
        name: core.name,
        version,
        description: core.description,
        install_hints: Value::Object(extra),
    }
}

/// Minimal percent-encoder for URL paths and query values. Avoids the
/// `urlencoding` crate to keep dep count low; only encodes the handful
/// of characters that actually break a URL.
fn urlencoding_minimal(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b'@' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}
