//! `pakx` CLI entrypoint.

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    /// Create an agents.yml manifest in the current directory.
    Init,
    /// Install everything in agents.yml to detected agents.
    Install,
}

// Subcommand stubs are infallible today, but the real `install` / `add`
// flows return Result, so the signature is pre-committed.
#[allow(clippy::unnecessary_wraps)]
fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init => {
            eprintln!("pakx v{VERSION} — scaffold only; init not yet implemented");
        }
        Command::Install => {
            eprintln!("pakx v{VERSION} — scaffold only; install not yet implemented");
        }
    }
    Ok(())
}
