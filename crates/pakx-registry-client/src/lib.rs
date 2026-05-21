//! Federated registry client for pakx.
//!
//! Queries the official MCP Registry, Smithery, Glama, and GitHub raw
//! sources in parallel, merging and deduping by content hash.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Re-exports the core crate version this client targets.
pub const SUPPORTED_CORE: &str = pakx_core::VERSION;
