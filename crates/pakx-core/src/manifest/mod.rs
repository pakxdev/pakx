//! Manifest types and (de)serialization for `agents.yml`.

pub mod io;
pub mod mutate;
pub mod parse;
pub mod path;
pub mod schema;
pub mod sponsors;
pub mod write;

pub use io::{read_from, write_to};
pub use mutate::{
    add_dep, add_shorthand, remove_shorthand, sections_containing, sections_containing_id,
    split_shorthand, update_shorthand, AddOutcome, RemoveOutcome, UpdateOutcome,
};
pub use parse::parse_manifest;
pub use path::{
    delete_value, get_value, get_value_json, parse_path, set_value, DeleteOutcome, PathError,
    PathSeg,
};
pub use schema::{
    AgentId, DepSpec, Dependencies, GitSpec, Manifest, PackageType, RegistrySpec, Sponsor,
    SponsorKind, StringSpec, KNOWN_AGENT_IDS, PACKAGE_TYPES,
};
pub use sponsors::{validate_sponsors, SponsorError, MAX_SPONSORS};
pub use write::write_manifest;
