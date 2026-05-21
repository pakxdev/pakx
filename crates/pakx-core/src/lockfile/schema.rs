//! Strongly-typed representation of `agents.lock`.
//!
//! The lockfile pins every resolved dep to a content hash + source URL +
//! version, with transitive deps as forward references to other entries.
//! JSON storage chosen over YAML for determinism (no key-order ambiguity)
//! and tooling support.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::manifest::{AgentId, PackageType};

/// Current on-disk lockfile schema version. Bump on incompatible changes.
pub const LOCKFILE_VERSION: u32 = 1;

/// Source registry that produced an entry. `git` and `github` are direct
/// fetches (no intermediary index); the others are federated public APIs
/// queried by the registry-client in v0.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrySource {
    OfficialMcp,
    Smithery,
    Glama,
    Github,
    Git,
    /// The pakx-registry backend (registry.pakx.dev) — first-party
    /// federated source for packages published through the CLI.
    Pakx,
}

/// All registry-source variants in canonical order.
pub const REGISTRY_SOURCES: [RegistrySource; 6] = [
    RegistrySource::OfficialMcp,
    RegistrySource::Smithery,
    RegistrySource::Glama,
    RegistrySource::Github,
    RegistrySource::Git,
    RegistrySource::Pakx,
];

impl RegistrySource {
    /// Stable kebab-case tag. Matches the serde representation (so a
    /// round-trip through `serde_json` / `serde_yaml_ng` produces the
    /// same string), but available without serializing. Used by the CLI
    /// for human + JSON output and is part of the documented JSON
    /// contract — only add new variants, never rename existing ones.
    pub const fn as_tag(self) -> &'static str {
        match self {
            Self::OfficialMcp => "official-mcp",
            Self::Smithery => "smithery",
            Self::Glama => "glama",
            Self::Github => "github",
            Self::Git => "git",
            Self::Pakx => "pakx",
        }
    }
}

// ---------------------------------------------------------------------------
// Integrity (SRI-style sha256)
// ---------------------------------------------------------------------------

static INTEGRITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^sha256-[A-Za-z0-9+/]{43}=$").expect("static regex compiles"));

/// SRI integrity string: `sha256-<base64>` (RFC 6920). The 44 char body is
/// 32 raw bytes = a sha256 digest. Newtype keeps malformed values out of
/// other lockfile fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Integrity(String);

impl Integrity {
    pub fn parse(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        if INTEGRITY_RE.is_match(&s) {
            Ok(Self(s))
        } else {
            Err(format!(
                "invalid integrity {s:?}: must be `sha256-<43 base64 chars>=`"
            ))
        }
    }

    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for Integrity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Entry key
// ---------------------------------------------------------------------------

static ENTRY_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(skills|mcp|subagents|prompts|commands|hooks)/[^@\s]+@[^\s]+$")
        .expect("static regex compiles")
});

/// Validate a flat lockfile entry key.
///
/// Format: `<type>/<canonical-id>@<version>` — for example
/// `skills/anthropics/pdf@1.2.0` or `mcp/smithery/github-mcp@0.5.1`.
pub fn is_valid_entry_key(key: &str) -> bool {
    ENTRY_KEY_RE.is_match(key)
}

// ---------------------------------------------------------------------------
// Lock entry + lockfile
// ---------------------------------------------------------------------------

/// One resolved package pinned into the lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LockEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: PackageType,
    pub version: String,
    /// Fully resolved fetch location (URL, git+ref, registry URI).
    pub resolved_from: String,
    pub registry: RegistrySource,
    pub integrity: Integrity,
    /// Agent ids this entry was installed into.
    #[serde(default)]
    pub agents: Vec<AgentId>,
    /// Transitive lockfile-entry keys this entry depends on.
    #[serde(default)]
    pub dependencies: Vec<String>,
}

/// On-disk `agents.lock`. `BTreeMap` gives deterministic key order for free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Lockfile {
    pub lockfile_version: u32,
    /// sha256 of the canonicalised manifest at lock time. Drift signal for
    /// `pakx doctor`.
    pub manifest_hash: Integrity,
    pub entries: BTreeMap<String, LockEntry>,
}

#[cfg(test)]
mod tests {
    use super::{RegistrySource, REGISTRY_SOURCES};

    /// `RegistrySource::as_tag` is the single source of truth for the
    /// kebab-case representation used by the CLI's human + JSON output.
    /// It MUST match the serde representation so a round-trip through
    /// JSON / YAML produces the same string — locking that in here.
    #[test]
    fn as_tag_matches_serde_kebab_case() {
        for src in REGISTRY_SOURCES {
            let via_serde = serde_json::to_string(&src).expect("serialize variant");
            let trimmed = via_serde.trim_matches('"');
            assert_eq!(trimmed, src.as_tag(), "as_tag must match serde for {src:?}");
        }
    }

    #[test]
    fn as_tag_returns_documented_strings() {
        assert_eq!(RegistrySource::OfficialMcp.as_tag(), "official-mcp");
        assert_eq!(RegistrySource::Smithery.as_tag(), "smithery");
        assert_eq!(RegistrySource::Glama.as_tag(), "glama");
        assert_eq!(RegistrySource::Github.as_tag(), "github");
        assert_eq!(RegistrySource::Git.as_tag(), "git");
        assert_eq!(RegistrySource::Pakx.as_tag(), "pakx");
    }
}
