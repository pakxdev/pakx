//! Manifest types and (de)serialization for `agents.yml`.

pub mod parse;
pub mod schema;
pub mod write;

pub use parse::parse_manifest;
pub use schema::{
    AgentId, DepSpec, Dependencies, GitSpec, Manifest, PackageType, RegistrySpec, StringSpec,
    KNOWN_AGENT_IDS, PACKAGE_TYPES,
};
pub use write::write_manifest;
