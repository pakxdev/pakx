//! Federated registry client for pakx.
//!
//! Queries the official MCP Registry, Smithery, Glama, and GitHub raw
//! sources in parallel, merging and deduping by `(source, id)`. v0.1
//! ships only the official MCP source; others are stub modules added
//! as their adapters land.

pub mod cache;
pub mod client;
pub mod errors;
pub mod official_mcp;
pub mod smithery;
pub mod source;
pub mod types;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Re-exports the core crate version this client targets.
pub const SUPPORTED_CORE: &str = pakx_core::VERSION;

pub use cache::{CacheDir, DEFAULT_TTL};
pub use client::RegistryClient;
pub use errors::RegistryError;
pub use official_mcp::{OfficialMcpSource, DEFAULT_BASE_URL as OFFICIAL_MCP_BASE_URL};
pub use smithery::{SmitherySource, DEFAULT_BASE_URL as SMITHERY_BASE_URL};
pub use source::Source;
pub use types::Package;
