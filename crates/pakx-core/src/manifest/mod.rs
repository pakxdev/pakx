//! Manifest types and (de)serialization for `agents.yml`.

pub mod io;
pub mod mutate;
pub mod parse;
pub mod schema;
pub mod write;

pub use io::{read_from, write_to};
pub use mutate::{add_dep, add_shorthand, AddOutcome};
pub use parse::parse_manifest;
pub use schema::{
    AgentId, DepSpec, Dependencies, GitSpec, Manifest, PackageType, RegistrySpec, StringSpec,
    KNOWN_AGENT_IDS, PACKAGE_TYPES,
};
pub use write::write_manifest;
