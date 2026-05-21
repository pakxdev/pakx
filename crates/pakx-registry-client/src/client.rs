//! Top-level [`RegistryClient`] that fans queries out across every
//! registered [`Source`] and merges results.

use futures::future::join_all;
use pakx_core::RegistrySource;
use tracing::warn;

use crate::errors::RegistryError;
use crate::source::Source;
use crate::types::Package;

pub struct RegistryClient {
    sources: Vec<Box<dyn Source>>,
}

impl RegistryClient {
    /// Construct an empty client. Add sources via [`Self::with_source`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// Builder-style: register one source.
    #[must_use]
    pub fn with_source(mut self, source: Box<dyn Source>) -> Self {
        self.sources.push(source);
        self
    }

    /// How many sources are registered.
    #[must_use]
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Fan a free-text search across every registered source in parallel,
    /// merge results, and dedupe by `(source, id)`. Per-source failures
    /// are logged (`tracing::warn`) and dropped — partial results win.
    pub async fn search(&self, query: &str) -> Vec<Package> {
        let futures = self.sources.iter().map(|s| async move {
            let tag = s.tag();
            (tag, s.search(query).await)
        });
        let results: Vec<(RegistrySource, Result<Vec<Package>, RegistryError>)> =
            join_all(futures).await;

        let mut out: Vec<Package> = Vec::new();
        for (tag, res) in results {
            match res {
                Ok(packages) => out.extend(packages),
                Err(e) => {
                    warn!(target: "pakx::registry", source = ?tag, error = %e, "source search failed");
                }
            }
        }
        dedupe_by_source_id(out)
    }

    /// Fetch a package by `(source, id)`. Returns `NotFound` if no source
    /// matching `tag` is registered, or whatever the source returns.
    pub async fn fetch(&self, tag: RegistrySource, id: &str) -> Result<Package, RegistryError> {
        for source in &self.sources {
            if source.tag() == tag {
                return source.fetch(id).await;
            }
        }
        Err(RegistryError::NotFound {
            source_tag: tag_to_static_str(tag),
            id: id.to_owned(),
        })
    }
}

impl Default for RegistryClient {
    fn default() -> Self {
        Self::new()
    }
}

fn dedupe_by_source_id(mut packages: Vec<Package>) -> Vec<Package> {
    packages.sort_by(|a, b| {
        (a.source, a.id.as_str(), a.version.as_str()).cmp(&(
            b.source,
            b.id.as_str(),
            b.version.as_str(),
        ))
    });
    packages.dedup_by(|a, b| a.source == b.source && a.id == b.id);
    packages
}

const fn tag_to_static_str(tag: RegistrySource) -> &'static str {
    match tag {
        RegistrySource::OfficialMcp => "official-mcp",
        RegistrySource::Smithery => "smithery",
        RegistrySource::Glama => "glama",
        RegistrySource::Github => "github",
        RegistrySource::Git => "git",
    }
}
