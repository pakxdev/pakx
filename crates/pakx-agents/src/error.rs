//! Errors raised by [`crate::Adapter`] implementations.

use std::path::PathBuf;

use pakx_core::manifest::PackageType;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    /// Underlying filesystem (or platform) failure.
    #[error("io error{path}: {source}", path = fmt_path(.path.as_ref()))]
    Io {
        #[source]
        source: std::io::Error,
        path: Option<PathBuf>,
    },

    /// The adapter does not support this primitive (e.g. Codex has no
    /// subagents). Not a hard failure — the installer logs and skips.
    #[error("adapter {adapter} does not support {primitive:?}")]
    Unsupported {
        adapter: &'static str,
        primitive: PackageType,
    },

    /// The exact package + version is already installed; no work needed.
    /// Adapters return this for idempotent re-runs, the installer treats it
    /// as success.
    #[error("{id} already installed at the same version")]
    AlreadyInstalled { id: String },

    /// `uninstall` / `list` lookup failed because the package isn't there.
    #[error("{id} is not installed")]
    NotInstalled { id: String },

    /// Computed sha256 didn't match the lockfile-pinned integrity. Indicates
    /// tampering or registry inconsistency — install must abort.
    #[error("integrity mismatch for {id}: expected {expected}, computed {computed}")]
    IntegrityMismatch {
        id: String,
        expected: String,
        computed: String,
    },

    /// The package payload is structurally invalid (e.g. empty files list,
    /// path traversal in a `SkillFile.relative_path`).
    #[error("invalid payload for {id}: {reason}")]
    Invalid { id: String, reason: String },
}

impl AdapterError {
    pub const fn io(source: std::io::Error, path: Option<PathBuf>) -> Self {
        Self::Io { source, path }
    }
}

fn fmt_path(p: Option<&PathBuf>) -> String {
    p.map_or_else(String::new, |path| format!(" at {}", path.display()))
}
