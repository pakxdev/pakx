//! `pakx` CLI entrypoint.

// `unreachable_pub` is meaningful only for library crates with an external
// API surface. In a binary crate every `pub` item is internal by
// construction, so the workspace lint just produces noise.
#![allow(unreachable_pub)]

mod commands;

use anyhow::Result;
use clap::{Parser, Subcommand};

use commands::add::{self, AddArgs};
use commands::init::{self, InitArgs};

const VERSION: &str = env!("CARGO_PKG_VERSION");

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
    Install,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init(args) => init::run(args).await,
        Command::Add(args) => add::run(args).await,
        Command::Install => {
            eprintln!("pakx v{VERSION} — scaffold only; install not yet implemented");
            Ok(())
        }
    }
}
