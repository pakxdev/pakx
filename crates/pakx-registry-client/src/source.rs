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

    /// Fetch a single package by canonical id within this source.
    async fn fetch(&self, id: &str) -> Result<Package, RegistryError>;
}
