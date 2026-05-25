//! The `Source` trait — one federated registry the client can query.

use async_trait::async_trait;
use pakx_core::RegistrySource;

use crate::errors::RegistryError;
use crate::types::Package;

/// One backing registry (official MCP, Smithery, Glama, GitHub raw, ...).
///
/// Sources are async, dyn-safe (via `async-trait`) so the aggregator
/// holds a `Vec<Box<dyn Source>>` and fans out queries in parallel.
#[async_trait]
pub trait Source: Send + Sync {
    /// Stable tag identifying which registry produced a result.
    fn tag(&self) -> RegistrySource;

    /// Free-text search. Empty `query` should return the first page
    /// (sources cap the result count themselves).
    async fn search(&self, query: &str) -> Result<Vec<Package>, RegistryError>;

    /// Free-text search constrained to a single package `kind`.
    ///
    /// The `kind` token is the canonical plural form
    /// (`skills` / `mcp` / `subagents` / `prompts` / `commands` / `hooks`),
    /// matching the CLI's [`pakx_core::manifest::PackageType`]. Sources that
    /// support a server-side kind filter (the first-party pakx-registry via
    /// `?kind=<kind>`) should forward it; the default implementation falls
    /// back to the unfiltered [`Source::search`] so federated sources with
    /// no kind concept stay correct (the aggregator filters their results
    /// client-side). `kind == None` is identical to [`Source::search`].
    async fn search_kind(
        &self,
        query: &str,
        kind: Option<&str>,
    ) -> Result<Vec<Package>, RegistryError> {
        let _ = kind;
        self.search(query).await
    }

    /// Fetch a single package by canonical id within this source.
    async fn fetch(&self, id: &str) -> Result<Package, RegistryError>;
}
