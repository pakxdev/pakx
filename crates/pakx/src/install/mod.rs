//! `pakx install` — resolve dependencies in `agents.yml`, install via
//! detected adapters, and write `agents.lock`.

pub mod mcp_translate;
pub mod runner;

pub use runner::{run, InstallOpts};
