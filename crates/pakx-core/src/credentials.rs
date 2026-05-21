//! Credential store for `pakx login` / `pakx publish` / `pakx whoami`.
//!
//! Storage: `~/.pakx/credentials.json` (per-user, lazily created). One
//! struct per known registry — a single user can be logged in to
//! multiple pakx-registry deployments at once, keyed by base URL.
//!
//! File permissions: on unix the file is created with 0600. On Windows
//! we rely on the user-profile ACL — pakx does not mutate ACLs to keep
//! the implementation portable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_REGISTRY_URL: &str = "https://registry.pakx.dev";
pub const CREDENTIALS_FILENAME: &str = "credentials.json";

#[derive(Debug, Error)]
pub enum CredentialsError {
    #[error("could not resolve home directory")]
    NoHomeDir,
    #[error("credentials io error{path}: {source}", path = fmt_path(.path.as_ref()))]
    Io {
        #[source]
        source: std::io::Error,
        path: Option<PathBuf>,
    },
    #[error("credentials file malformed{path}: {source}", path = fmt_path(.path.as_ref()))]
    Parse {
        #[source]
        source: serde_json::Error,
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// Map of `<registry_base_url>` → entry.
    #[serde(default)]
    pub registries: BTreeMap<String, Entry>,
}

impl Credentials {
    /// Default path: `~/.pakx/credentials.json`.
    pub fn default_path() -> Result<PathBuf, CredentialsError> {
        let home = dirs::home_dir().ok_or(CredentialsError::NoHomeDir)?;
        Ok(home.join(".pakx").join(CREDENTIALS_FILENAME))
    }

    /// Read from disk. Returns an empty store if the file is absent.
    pub fn read_from(path: &Path) -> Result<Self, CredentialsError> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| CredentialsError::Parse {
                source,
                path: Some(path.to_path_buf()),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(CredentialsError::Io {
                source,
                path: Some(path.to_path_buf()),
            }),
        }
    }

    /// Write to disk. Creates the parent directory (and on unix, 0600s
    /// the resulting file).
    pub fn write_to(&self, path: &Path) -> Result<(), CredentialsError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CredentialsError::Io {
                source,
                path: Some(parent.to_path_buf()),
            })?;
        }
        let body = serde_json::to_vec_pretty(self).expect("BTreeMap<String, Entry> serializes");
        std::fs::write(path, body).map_err(|source| CredentialsError::Io {
            source,
            path: Some(path.to_path_buf()),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, perms).map_err(|source| CredentialsError::Io {
                source,
                path: Some(path.to_path_buf()),
            })?;
        }
        Ok(())
    }

    /// Convenience: read from the default location.
    pub fn read_default() -> Result<Self, CredentialsError> {
        let path = Self::default_path()?;
        Self::read_from(&path)
    }

    /// Look up the token for a given registry URL. Trailing slashes
    /// are normalised so callers do not have to.
    #[must_use]
    pub fn get(&self, registry_url: &str) -> Option<&Entry> {
        let normalised = normalise(registry_url);
        self.registries.get(&normalised)
    }

    /// Add or replace an entry. Returns the previous value.
    pub fn set(&mut self, registry_url: &str, entry: Entry) -> Option<Entry> {
        self.registries.insert(normalise(registry_url), entry)
    }

    /// Remove an entry. Returns the previous value.
    pub fn remove(&mut self, registry_url: &str) -> Option<Entry> {
        self.registries.remove(&normalise(registry_url))
    }
}

fn normalise(url: &str) -> String {
    url.trim_end_matches('/').to_lowercase()
}

fn fmt_path(p: Option<&PathBuf>) -> String {
    p.map_or_else(String::new, |path| format!(" at {}", path.display()))
}
