//! Resolved install payloads — what an adapter actually writes to disk.
//!
//! These types are produced by the resolver (Step 6+) from a manifest
//! `DepSpec` plus registry metadata, and consumed by adapter trait methods.
//! `Skill` and `McpServer` are fleshed out for v0.1; the other primitives
//! are opaque markers that get full schemas as each adapter step lands.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lockfile::Integrity;
use crate::manifest::PackageType;

/// One file inside a skill bundle (e.g. `SKILL.md`, `reference/usage.md`).
/// The path is *relative* to the skill's install root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFile {
    pub relative_path: String,
    pub contents: Vec<u8>,
}

/// A resolved skill ready to install. Identity is `owner/name@version`;
/// `integrity` covers the concatenated, sorted file contents (see
/// [`compute_integrity`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub owner: String,
    pub name: String,
    pub version: String,
    pub files: Vec<SkillFile>,
    pub integrity: Integrity,
}

impl Skill {
    /// Canonical `<owner>/<name>` identifier used in lockfile keys and
    /// install paths.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Lockfile entry key for this skill.
    #[must_use]
    pub fn lockfile_key(&self) -> String {
        format!(
            "{}/{}/{}@{}",
            PackageType::Skills.as_str(),
            self.owner,
            self.name,
            self.version
        )
    }

    /// Recompute integrity from this skill's files.
    #[must_use]
    pub fn computed_integrity(&self) -> Integrity {
        compute_integrity(&self.files)
    }

    /// Convenience: `self.computed_integrity() == self.integrity`.
    #[must_use]
    pub fn integrity_matches(&self) -> bool {
        self.computed_integrity() == self.integrity
    }
}

/// Compute an [`Integrity`] over a slice of skill files. Stable across runs
/// and machines because files are sorted by relative path first.
#[must_use]
pub fn compute_integrity(files: &[SkillFile]) -> Integrity {
    let mut sorted: Vec<&SkillFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    let mut hasher = Sha256::new();
    for f in sorted {
        hasher.update(f.relative_path.as_bytes());
        hasher.update([0u8]);
        hasher.update((f.contents.len() as u64).to_le_bytes());
        hasher.update(&f.contents);
    }
    let digest = hasher.finalize();
    let b64 = BASE64_STANDARD.encode(digest);
    Integrity::parse(format!("sha256-{b64}"))
        .expect("base64 of sha256 always matches integrity regex")
}

// ---------------------------------------------------------------------------
// MCP server payload + transport
// ---------------------------------------------------------------------------

/// How an MCP server is launched / reached. `BTreeMap` keeps env + headers
/// in deterministic order for hashing and write-out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpTransport {
    /// Locally-spawned subprocess that speaks MCP over stdio. By far the
    /// most common transport today (npm/pypi/binary packages).
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    /// Hosted MCP server reachable over HTTP/SSE.
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

/// A resolved MCP server ready to install. The adapter writes the transport
/// into the agent's MCP config (e.g. `.mcp.json` for Claude Code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    /// Canonical id from the source registry, e.g.
    /// `io.github.modelcontextprotocol/server-filesystem`.
    pub id: String,
    pub version: String,
    pub transport: McpTransport,
}

impl McpServer {
    /// Lockfile entry key.
    #[must_use]
    pub fn lockfile_key(&self) -> String {
        format!("{}/{}@{}", PackageType::Mcp.as_str(), self.id, self.version)
    }

    /// Sha256 over the canonical JSON of the transport config. Used as the
    /// lockfile integrity and as the on-disk-drift detector.
    #[must_use]
    pub fn computed_integrity(&self) -> Integrity {
        let bytes = serde_json::to_vec(&self.transport)
            .expect("McpTransport with String keys serializes infallibly");
        let mut hasher = Sha256::new();
        hasher.update(self.id.as_bytes());
        hasher.update([0u8]);
        hasher.update(self.version.as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        let digest = hasher.finalize();
        let b64 = BASE64_STANDARD.encode(digest);
        Integrity::parse(format!("sha256-{b64}"))
            .expect("base64 of sha256 always matches integrity regex")
    }

    /// Short name used as the key inside agent-side config files
    /// (`.mcp.json` etc.). Last `/`-separated segment of the id, lowercased.
    #[must_use]
    pub fn short_name(&self) -> String {
        self.id
            .rsplit('/')
            .next()
            .unwrap_or(&self.id)
            .to_lowercase()
    }
}

// ---------------------------------------------------------------------------
// Opaque markers for primitives not yet implemented
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subagent {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub id: String,
}
