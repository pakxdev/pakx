//! Source impl for the Smithery registry.
//!
//! API: <https://registry.smithery.ai>
//!
//! v0.1 supports search only: Smithery's connection / config schema
//! differs from the official MCP Registry's `packages[]` shape, so
//! `fetch` returns `NotFound`. Translation + install support land
//! when the Phase A→C `SaaS` roadmap reaches Smithery-side install.

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

/// Production base URL for the Smithery registry.
pub const DEFAULT_BASE_URL: &str = "https://registry.smithery.ai";

const TAG: &str = "smithery";

#[derive(Debug, Clone)]
pub struct SmitherySource {
    http: Client,
    base_url: String,
    cache: CacheDir,
}

impl SmitherySource {
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

    fn cache_key_search(query: &str) -> String {
        format!("{TAG}:search:{query}")
    }
}

#[async_trait]
impl Source for SmitherySource {
    fn tag(&self) -> RegistrySource {
        RegistrySource::Smithery
    }

    async fn search(&self, query: &str) -> Result<Vec<Package>, RegistryError> {
        let key = Self::cache_key_search(query);
        let http = self.http.clone();
        let base_url = self.base_url.clone();
        let q = query.to_owned();
        self.cache
            .get_or_fetch::<Vec<Package>, _, _>(&key, move || async move {
                let url = if q.is_empty() {
                    format!("{base_url}/servers")
                } else {
                    format!("{base_url}/servers?q={}", urlencoding_minimal(&q))
                };
                debug!(target: "pakx::registry", %url, "smithery search");
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
        // Search-only at v0.1. Returning NotFound is honest: the
        // aggregator falls back to the next source.
        Err(RegistryError::NotFound {
            source_tag: TAG,
            id: id.to_owned(),
        })
    }
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct ServerListResponse {
    #[serde(default)]
    servers: Vec<ServerRaw>,
    #[serde(default)]
    #[allow(dead_code)]
    pagination: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct ServerRaw {
    /// Smithery canonical id, e.g. `@modelcontextprotocol/server-filesystem`.
    #[serde(rename = "qualifiedName")]
    qualified_name: String,
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

fn into_package(raw: ServerRaw) -> Package {
    let name = raw
        .display_name
        .unwrap_or_else(|| raw.qualified_name.clone());
    let install_hints = Value::Object(raw.extra);
    Package {
        id: raw.qualified_name.clone(),
        source: RegistrySource::Smithery,
        name,
        // Smithery does not currently expose a stable version at the
        // server level (versions live under `connections[].publishedVersion`).
        // Surface "latest" as a placeholder until Phase A v2 lands.
        version: "latest".to_owned(),
        description: raw.description,
        install_hints,
    }
}

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
