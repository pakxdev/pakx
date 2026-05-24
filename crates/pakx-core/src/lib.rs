//! Manifest, lockfile, resolver, and installer logic for `pakx`.
//!
//! This crate is the functional core: parsing, validation, and pure logic.
//! Filesystem and network side effects live in `pakx-agents` and
//! `pakx-registry-client`, respectively.

pub mod atomic_write;
pub mod credentials;
pub mod errors;
pub mod http_client;
pub mod install;
pub mod lockfile;
pub mod manifest;
pub mod validation;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use atomic_write::atomic_write;
pub use credentials::{
    Credentials, CredentialsError, Entry as CredentialEntry, DEFAULT_REGISTRY_URL,
};
pub use errors::{LockfileError, ManifestError};
pub use http_client::{
    http_client, http_client_with_timeout, DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT,
    UPLOAD_REQUEST_TIMEOUT,
};
pub use install::{
    compute_integrity, Command, Hook, McpServer, McpTransport, Prompt, Skill, SkillFile, Subagent,
};
pub use lockfile::{
    parse_lockfile, read_from as read_lockfile_from, write_lockfile, write_to as write_lockfile_to,
    Integrity, LockEntry, Lockfile, RegistrySource, LOCKFILE_VERSION, REGISTRY_SOURCES,
};
pub use manifest::{
    add_dep, add_shorthand, delete_value as manifest_delete_value, get_value as manifest_get_value,
    get_value_json as manifest_get_value_json, parse_manifest, parse_path as manifest_parse_path,
    read_from as read_manifest_from, remove_shorthand, sections_containing, sections_containing_id,
    set_value as manifest_set_value, split_shorthand, update_shorthand, validate_sponsors,
    write_manifest, write_to as write_manifest_to, AddOutcome, AgentId, DeleteOutcome, DepSpec,
    Dependencies, GitSpec, Manifest, PackageType, PathError as ManifestPathError,
    PathSeg as ManifestPathSeg, RegistrySpec, RemoveOutcome, Sponsor, SponsorError, SponsorKind,
    StringSpec, UpdateOutcome, KNOWN_AGENT_IDS, MAX_SPONSORS, PACKAGE_TYPES,
};
pub use validation::{validate_package_name, validate_version, ValidationError, MAX_VERSION_LEN};
