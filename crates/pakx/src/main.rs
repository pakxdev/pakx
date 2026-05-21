//! `pakx` CLI entrypoint.

// `unreachable_pub` is meaningful only for library crates with an external
// API surface. In a binary crate every `pub` item is internal by
// construction, so the workspace lint just produces noise.
#![allow(unreachable_pub)]

mod commands;
mod install;

use anyhow::Result;
use clap::{Parser, Subcommand};

use commands::add::{self, AddArgs};
use commands::doctor::{self, DoctorArgs};
use commands::init::{self, InitArgs};
use commands::install::{self as install_cmd, InstallArgs};
use commands::list::{self as list_cmd, ListArgs};
use commands::search::{self, SearchArgs};

#[derive(Debug, Parser)]
#[command(
    name = "pakx",
    version,
    about = "Universal package manager for AI agent context",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create an `agents.yml` manifest in the current directory.
    Init(InitArgs),
    /// Add a dependency to `agents.yml`.
    Add(AddArgs),
    /// Install everything in `agents.yml` to detected agents.
    Install(InstallArgs),
    /// List pinned dependencies (reads `agents.lock`).
    List(ListArgs),
    /// Health-check the project + agent install state.
    Doctor(DoctorArgs),
    /// Search the federated registry for packages.
    Search(SearchArgs),
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init(args) => init::run(args).await,
        Command::Add(args) => add::run(args).await,
        Command::Install(args) => install_cmd::run_cmd(args).await,
        Command::List(args) => list_cmd::run(args).await,
        Command::Doctor(args) => doctor::run(args).await,
        Command::Search(args) => search::run(args).await,
    }
}
