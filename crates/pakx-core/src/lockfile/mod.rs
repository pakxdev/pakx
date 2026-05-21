//! Lockfile types and (de)serialization for `agents.lock`.

pub mod parse;
pub mod schema;
pub mod write;

pub use parse::parse_lockfile;
pub use schema::{
    Integrity, LockEntry, Lockfile, RegistrySource, LOCKFILE_VERSION, REGISTRY_SOURCES,
};
pub use write::write_lockfile;
