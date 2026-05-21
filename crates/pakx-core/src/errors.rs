//! Typed error variants for parsing and validating manifests / lockfiles.
//!
//! Both error enums carry an optional `path` (set by the caller when the
//! source originated from a file) and either a wrapped parser error or a
//! `Schema` variant for validation failures. No panics across crate
//! boundaries; library code returns `Result<T, ManifestError | LockfileError>`.

use std::path::PathBuf;

use thiserror::Error;

/// Failures returned from parsing or validating `agents.yml`.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// The source text was not valid YAML.
    #[error("agents.yml is not valid YAML{path}: {source}", path = fmt_path(.path.as_ref()))]
    ParseYaml {
        #[source]
        source: serde_yml::Error,
        path: Option<PathBuf>,
    },
    /// The YAML parsed but did not match the manifest schema.
    #[error("agents.yml failed schema validation{path}: {message}", path = fmt_path(.path.as_ref()))]
    Schema {
        message: String,
        path: Option<PathBuf>,
    },
}

impl ManifestError {
    #[must_use]
    pub fn with_path(mut self, p: impl Into<PathBuf>) -> Self {
        let new_path = p.into();
        match &mut self {
            Self::ParseYaml { path, .. } | Self::Schema { path, .. } => {
                *path = Some(new_path);
            }
        }
        self
    }

    pub const fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::ParseYaml { path, .. } | Self::Schema { path, .. } => path.as_ref(),
        }
    }
}

/// Failures returned from parsing or validating `agents.lock`.
#[derive(Debug, Error)]
pub enum LockfileError {
    /// The source text was not valid JSON.
    #[error("agents.lock is not valid JSON{path}: {source}", path = fmt_path(.path.as_ref()))]
    ParseJson {
        #[source]
        source: serde_json::Error,
        path: Option<PathBuf>,
    },
    /// The JSON parsed but did not match the lockfile schema.
    #[error("agents.lock failed schema validation{path}: {message}", path = fmt_path(.path.as_ref()))]
    Schema {
        message: String,
        path: Option<PathBuf>,
    },
}

impl LockfileError {
    #[must_use]
    pub fn with_path(mut self, p: impl Into<PathBuf>) -> Self {
        let new_path = p.into();
        match &mut self {
            Self::ParseJson { path, .. } | Self::Schema { path, .. } => {
                *path = Some(new_path);
            }
        }
        self
    }

    pub const fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::ParseJson { path, .. } | Self::Schema { path, .. } => path.as_ref(),
        }
    }
}

fn fmt_path(p: Option<&PathBuf>) -> String {
    p.map_or_else(String::new, |path| format!(" at {}", path.display()))
}
