//! Manifest, lockfile, resolver, and installer logic for `pakx`.
//!
//! This crate is the functional core: parsing, validation, and pure logic.
//! Filesystem and network side effects live in `pakx-agents` and
//! `pakx-registry-client`, respectively.

pub mod errors;
pub mod lockfile;
pub mod manifest;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use errors::{LockfileError, ManifestError};
pub use lockfile::{
    parse_lockfile, write_lockfile, Integrity, LockEntry, Lockfile, RegistrySource,
    LOCKFILE_VERSION, REGISTRY_SOURCES,
};
pub use manifest::{
    parse_manifest, write_manifest, AgentId, DepSpec, Dependencies, GitSpec, Manifest, PackageType,
    RegistrySpec, StringSpec, KNOWN_AGENT_IDS, PACKAGE_TYPES,
};
