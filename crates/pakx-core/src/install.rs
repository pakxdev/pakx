//! Resolved install payloads — what an adapter actually writes to disk.
//!
//! These types are produced by the resolver (Step 6+) from a manifest
//! `DepSpec` plus registry metadata, and consumed by adapter trait methods.
//! Only [`Skill`] is fleshed out for v0.1; the other primitives are opaque
//! markers that get full schemas as each adapter step lands.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
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
/// `pakx-core/src/install/integrity.rs` once that lands).
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

    /// Recompute integrity from this skill's files. Determinism: files are
    /// sorted by `relative_path` before hashing, each entry hashed as
    /// `<path>\0<len:u64-le><bytes>`. Verifies against [`Skill::integrity`].
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
// Opaque markers for primitives not yet implemented
// ---------------------------------------------------------------------------
// These exist so the Adapter trait can name a parameter type today; the real
// payload struct replaces them when their adapter step lands.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub id: String,
}

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
