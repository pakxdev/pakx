//! `pakx install` — resolve dependencies in `agents.yml`, install via
//! detected adapters, and write `agents.lock`.

pub mod bundle;
pub mod mcp_translate;
pub mod progress;
pub mod rollback;
pub mod runner;
pub mod skill;

pub use progress::MultiProgressSink;
pub use runner::{
    run, run_with_progress, InstallOpts, InstallReportEntry, InstallStatus, ADAPTER_WIRED_KINDS,
};
