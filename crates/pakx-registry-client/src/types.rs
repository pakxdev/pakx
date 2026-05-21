//! Common types returned by every [`crate::Source`] implementation.

use pakx_core::RegistrySource;
use serde::{Deserialize, Serialize};

/// One package surfaced by a registry.
///
/// `install_hints` carries the raw, source-specific install metadata
/// that the resolver later translates into a concrete
/// [`pakx_core::Skill`] / [`pakx_core::install::McpServer`] payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// Canonical id within this source (e.g. `io.github.microsoft/playwright-mcp`).
    pub id: String,
    pub source: RegistrySource,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Source-specific install metadata. Schema differs per source.
    #[serde(default)]
    pub install_hints: serde_json::Value,
}

impl Package {
    /// Stable string for log lines: `<source>/<id>@<version>`.
    #[must_use]
    pub fn display_id(&self) -> String {
        format!("{:?}/{}@{}", self.source, self.id, self.version)
    }
}
