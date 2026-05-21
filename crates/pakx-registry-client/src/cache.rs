//! File-backed cache for federated-source responses.
//!
//! - Storage root: `~/.pakx/cache/` by default (override-able for tests).
//! - Key derivation: `sha256(source_tag || ':' || query_or_id)` →
//!   first 16 bytes base64url for a short, filesystem-safe filename.
//! - TTL: 1 hour (per master prompt), enforced via file `mtime`.
//! - On TTL expiry the file is removed and the fetcher re-runs.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::errors::RegistryError;

/// One-hour default TTL.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone)]
pub struct CacheDir {
    root: PathBuf,
    ttl: Duration,
}

impl CacheDir {
    /// Construct against `~/.pakx/cache/`. Returns `None` if no home dir
    /// can be resolved on this platform.
    #[must_use]
    pub fn default_path() -> Option<Self> {
        dirs::home_dir().map(|h| Self::with_root(h.join(".pakx").join("cache")))
    }

    #[must_use]
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ttl: DEFAULT_TTL,
        }
    }

    #[must_use]
    pub const fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the cached value for `key` if present and within TTL.
    /// Otherwise runs `fetcher` and stores its result.
    pub async fn get_or_fetch<T, F, Fut>(&self, key: &str, fetcher: F) -> Result<T, RegistryError>
    where
        T: Serialize + DeserializeOwned + Send + Sync,
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<T, RegistryError>> + Send,
    {
        let path = self.path_for(key);

        if let Some(hit) = self.read_if_fresh::<T>(&path).await? {
            return Ok(hit);
        }

        let fresh = fetcher().await?;
        self.write(&path, &fresh).await?;
        Ok(fresh)
    }

    /// Force-evict a single key. Used by tests and `pakx doctor --reset-cache`.
    pub async fn invalidate(&self, key: &str) -> Result<(), RegistryError> {
        let path = self.path_for(key);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(RegistryError::Cache {
                source,
                path: Some(path),
            }),
        }
    }

    fn path_for(&self, key: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        let digest = hasher.finalize();
        let short = B64URL.encode(&digest[..16]);
        self.root.join(format!("{short}.json"))
    }

    async fn read_if_fresh<T>(&self, path: &Path) -> Result<Option<T>, RegistryError>
    where
        T: DeserializeOwned,
    {
        let meta = match fs::metadata(path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(RegistryError::Cache {
                    source,
                    path: Some(path.to_path_buf()),
                });
            }
        };
        let mtime = meta.modified().map_err(|source| RegistryError::Cache {
            source,
            path: Some(path.to_path_buf()),
        })?;
        let age = SystemTime::now()
            .duration_since(mtime)
            .unwrap_or(Duration::ZERO);
        if age > self.ttl {
            // Expired — clean up best-effort, ignore failure.
            let _ = fs::remove_file(path).await;
            return Ok(None);
        }

        let bytes = fs::read(path)
            .await
            .map_err(|source| RegistryError::Cache {
                source,
                path: Some(path.to_path_buf()),
            })?;
        if let Ok(v) = serde_json::from_slice::<T>(&bytes) {
            return Ok(Some(v));
        }
        // Corrupt or schema-incompatible cache entry. Drop it so
        // the fetcher re-populates with a fresh body.
        let _ = fs::remove_file(path).await;
        Ok(None)
    }

    async fn write<T: Serialize + Sync>(
        &self,
        path: &Path,
        value: &T,
    ) -> Result<(), RegistryError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|source| RegistryError::Cache {
                    source,
                    path: Some(parent.to_path_buf()),
                })?;
        }
        let bytes = serde_json::to_vec(value).map_err(|e| RegistryError::Cache {
            source: std::io::Error::other(e),
            path: Some(path.to_path_buf()),
        })?;
        fs::write(path, bytes)
            .await
            .map_err(|source| RegistryError::Cache {
                source,
                path: Some(path.to_path_buf()),
            })
    }
}
