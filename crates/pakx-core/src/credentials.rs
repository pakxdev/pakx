//! Credential store for `pakx login` / `pakx publish` / `pakx whoami`.
//!
//! Storage: `~/.pakx/credentials.json` (per-user, lazily created). One
//! struct per known registry — a single user can be logged in to
//! multiple pakx-registry deployments at once, keyed by base URL.
//!
//! File permissions: on unix the file is created with mode `0600` at
//! the `open` call (not as a post-write chmod) — the previous
//! `std::fs::write` then `set_permissions` flow briefly exposed the
//! token at the default umask (typically `0o644`), readable by any
//! other local user on a multi-user box.
//!
//! Atomicity: the body is written to `credentials.json.tmp` and
//! renamed into place so a crash mid-write does not leave a
//! half-written file. On Windows we still rely on the user-profile
//! ACL — pakx does not mutate ACLs to keep the implementation
//! portable.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
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

/// `deny_unknown_fields`: a typo in `credentials.json` surfaces.
///
/// Without it, a future-version field we don't model yet would be
/// silently dropped on round-trip — and losing the `token` field is
/// catastrophic, so we want strict parsing here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

    /// Write to disk. Creates the parent directory. On unix, the file
    /// is created with mode `0600` directly via `OpenOptions::mode` —
    /// not via a post-write `chmod` — so the token is never on disk at
    /// the default umask.
    ///
    /// The write is atomic: the body lands in `<path>.tmp` first, then
    /// `rename` swaps it into place. A crash mid-write leaves either
    /// the old file untouched or the new file complete; never a
    /// half-written `credentials.json`.
    pub fn write_to(&self, path: &Path) -> Result<(), CredentialsError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CredentialsError::Io {
                source,
                path: Some(parent.to_path_buf()),
            })?;
        }
        let body = serde_json::to_vec_pretty(self).expect("BTreeMap<String, Entry> serializes");

        let tmp_path = tmp_path_for(path);

        let mut opts = OpenOptions::new();
        // `create_new(true)` instead of `create(true).truncate(true)`:
        // `OpenOptions::mode(0o600)` is **ignored on existing files**, so
        // a stale `<path>.tmp` from a prior crash — or one pre-planted by
        // a co-process — would keep its prior permission bits (often
        // `0o644` at the default umask) and the subsequent `rename` would
        // install `credentials.json` at the wrong mode, defeating the
        // security guarantee in exactly the crash-recovery scenario the
        // atomic-write was meant to handle. `create_new` errors out on
        // pre-existing `.tmp`; we unlink + retry once on `AlreadyExists`
        // so an honest stale `.tmp` does not wedge the user.
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // 0600 = owner read/write, no group, no other. Setting the
            // mode at `open` time is the atomicity guarantee — a
            // subsequent `set_permissions` would leave a window where
            // the file existed at the default umask.
            opts.mode(0o600);
        }

        let mut file = match opts.open(&tmp_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Stale `.tmp` (prior crash, or a co-process). Unlink and
                // retry exactly once — never loop, so an adversary
                // racing us cannot cause an indefinite spin.
                std::fs::remove_file(&tmp_path).map_err(|source| CredentialsError::Io {
                    source,
                    path: Some(tmp_path.clone()),
                })?;
                opts.open(&tmp_path)
                    .map_err(|source| CredentialsError::Io {
                        source,
                        path: Some(tmp_path.clone()),
                    })?
            }
            Err(source) => {
                return Err(CredentialsError::Io {
                    source,
                    path: Some(tmp_path),
                });
            }
        };
        file.write_all(&body)
            .map_err(|source| CredentialsError::Io {
                source,
                path: Some(tmp_path.clone()),
            })?;
        file.sync_all().map_err(|source| CredentialsError::Io {
            source,
            path: Some(tmp_path.clone()),
        })?;
        drop(file);

        std::fs::rename(&tmp_path, path).map_err(|source| {
            // On rename failure clean up the tmp so we don't leak a
            // stale tmp file. Ignore cleanup errors — surfacing the
            // original failure is more useful.
            let _ = std::fs::remove_file(&tmp_path);
            CredentialsError::Io {
                source,
                path: Some(path.to_path_buf()),
            }
        })?;
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

/// Compute the temp path used by [`Credentials::write_to`]. Splitting
/// this out lets us unit-test the rename target shape without going
/// through the filesystem.
fn tmp_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

fn fmt_path(p: Option<&PathBuf>) -> String {
    p.map_or_else(String::new, |path| format!(" at {}", path.display()))
}
