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
                let url = format!("{base_url}/v0/servers/{}", urlencoding_minimal(&id_owned));
                debug!(target: "pakx::registry", %url, "official-mcp fetch");
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

                let raw: ServerRaw = response
                    .error_for_status()
                    .map_err(|source| RegistryError::Http {
                        source_tag: TAG,
                        source,
                    })?
                    .json::<ServerRaw>()
                    .await
                    .map_err(|source| RegistryError::Decode {
                        source_tag: TAG,
                        message: source.to_string(),
                    })?;
                Ok(into_package(raw))
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
struct ServerRaw {
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

#[derive(Debug, Clone, Deserialize)]
struct VersionDetail {
    #[serde(default)]
    version: Option<String>,
}

fn into_package(raw: ServerRaw) -> Package {
    let version = raw
        .version
        .or_else(|| raw.version_detail.and_then(|v| v.version))
        .unwrap_or_else(|| "0.0.0".to_string());
    let install_hints = Value::Object(raw.extra);
    Package {
        id: raw.name.clone(),
        source: RegistrySource::OfficialMcp,
        name: raw.name,
        version,
        description: raw.description,
        install_hints,
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
