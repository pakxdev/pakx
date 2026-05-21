//! Manifest, lockfile, resolver, and installer logic for `pakx`.
//!
//! This crate is the functional core: parsing, validation, and pure logic.
//! Filesystem and network side effects live in `pakx-agents` and
//! `pakx-registry-client`, respectively.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
