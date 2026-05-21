//! `pakx completion <shell>` — emit shell completion script.
//!
//! Generated via `clap_complete` from the same `Cli` struct that drives
//! every other subcommand, so completions never drift out of sync
//! with the actual flag surface.

use anyhow::Result;
use clap::{Args, CommandFactory, ValueEnum};
use clap_complete::{generate, Shell};
use std::io;

#[derive(Debug, Clone, Args)]
pub struct CompletionArgs {
    /// Target shell.
    pub shell: CompletionShell,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}

impl From<CompletionShell> for Shell {
    fn from(s: CompletionShell) -> Self {
        match s {
            CompletionShell::Bash => Self::Bash,
            CompletionShell::Zsh => Self::Zsh,
            CompletionShell::Fish => Self::Fish,
            CompletionShell::Powershell => Self::PowerShell,
            CompletionShell::Elvish => Self::Elvish,
        }
    }
}

// Pure stdout emit — no await — but the main `Command` enum's dispatch
// arm awaits every variant, so the signature stays async for symmetry.
#[allow(clippy::unused_async)]
pub async fn run<C: CommandFactory>(args: CompletionArgs) -> Result<()> {
    let mut cmd = C::command();
    let bin_name = cmd.get_name().to_string();
    generate(
        Shell::from(args.shell),
        &mut cmd,
        bin_name,
        &mut io::stdout(),
    );
    Ok(())
}
