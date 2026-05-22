//! Manifest, lockfile, resolver, and installer logic for `pakx`.
//!
//! This crate is the functional core: parsing, validation, and pure logic.
//! Filesystem and network side effects live in `pakx-agents` and
//! `pakx-registry-client`, respectively.

pub mod atomic_write;
pub mod credentials;
pub mod errors;
pub mod install;
pub mod lockfile;
pub mod manifest;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use atomic_write::atomic_write;
pub use credentials::{
    Credentials, CredentialsError, Entry as CredentialEntry, DEFAULT_REGISTRY_URL,
};
pub use errors::{LockfileError, ManifestError};
pub use install::{
    compute_integrity, Command, Hook, McpServer, McpTransport, Prompt, Skill, SkillFile, Subagent,
};
pub use lockfile::{
    parse_lockfile, read_from as read_lockfile_from, write_lockfile, write_to as write_lockfile_to,
    Integrity, LockEntry, Lockfile, RegistrySource, LOCKFILE_VERSION, REGISTRY_SOURCES,
};
pub use manifest::{
    add_dep, add_shorthand, parse_manifest, read_from as read_manifest_from, remove_shorthand,
    sections_containing, sections_containing_id, split_shorthand, update_shorthand,
    validate_sponsors, write_manifest, write_to as write_manifest_to, AddOutcome, AgentId, DepSpec,
    Dependencies, GitSpec, Manifest, PackageType, RegistrySpec, RemoveOutcome, Sponsor,
    SponsorError, SponsorKind, StringSpec, UpdateOutcome, KNOWN_AGENT_IDS, MAX_SPONSORS,
    PACKAGE_TYPES,
};
