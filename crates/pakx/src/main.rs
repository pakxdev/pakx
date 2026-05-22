//! `pakx` CLI entrypoint.

// `unreachable_pub` is meaningful only for library crates with an external
// API surface. In a binary crate every `pub` item is internal by
// construction, so the workspace lint just produces noise.
#![allow(unreachable_pub)]

mod commands;
mod install;
mod pack;
mod redact;
mod registry_url;
mod resolve;
mod ui;

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::ui::ColorMode;
use commands::add::{self, AddArgs};
use commands::completion::{self as completion_cmd, CompletionArgs};
use commands::config::{self as config_cmd, ConfigArgs};
use commands::doctor::{self, DoctorArgs};
use commands::info::{self as info_cmd, InfoArgs};
use commands::init::{self, InitArgs};
use commands::install::{self as install_cmd, InstallArgs};
use commands::list::{self as list_cmd, ListArgs};
use commands::login::{self as login_cmd, LoginArgs};
use commands::outdated::{self as outdated_cmd, OutdatedArgs};
use commands::pack::{self as pack_cmd, PackArgs};
use commands::publish::{self as publish_cmd, PublishArgs};
use commands::remove::{self as remove_cmd, RemoveArgs};
use commands::search::{self, SearchArgs};
use commands::test::{self as test_cmd, TestArgs};
use commands::unpublish::{self as unpublish_cmd, UnpublishArgs};
use commands::update::{self as update_cmd, UpdateArgs};
use commands::upgrade::{self as upgrade_cmd, UpgradeArgs};
use commands::whoami::{self as whoami_cmd, WhoamiArgs};

#[derive(Debug, Parser)]
#[command(
    name = "pakx",
    version,
    about = "Universal package manager for AI agent context",
    propagate_version = true
)]
struct Cli {
    /// When to emit ANSI color codes.
    ///
    /// `auto` (default) — color when stdout/stderr is a TTY and
    /// `NO_COLOR` is unset. `always` — force-enable regardless of the
    /// stream (e.g. for `pakx list --color always | less -R`). `never`
    /// — force-disable regardless (CI logs, scripted tests).
    #[arg(long, value_name = "MODE", value_enum, default_value_t = ColorMode::Auto, global = true)]
    color: ColorMode,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create an `agents.yml` manifest in the current directory.
    Init(InitArgs),
    /// Add a dependency to `agents.yml`.
    Add(AddArgs),
    /// Remove a dependency from `agents.yml`.
    Remove(RemoveArgs),
    /// Install everything in `agents.yml` to detected agents.
    Install(InstallArgs),
    /// List pinned dependencies (reads `agents.lock`).
    List(ListArgs),
    /// Show lockfile entries whose source registry has a newer version.
    Outdated(OutdatedArgs),
    /// Health-check the project + agent install state.
    Doctor(DoctorArgs),
    /// Search the federated registry for packages.
    Search(SearchArgs),
    /// Validate `agents.yml` without installing anything (CI / pre-commit).
    Test(TestArgs),
    /// Print registry metadata + version list for a published package.
    Info(InfoArgs),
    /// Log in to a pakx-registry deployment.
    Login(LoginArgs),
    /// Print the GitHub login pakx is authenticated as.
    Whoami(WhoamiArgs),
    /// Build a gzipped tarball from a local skill bundle.
    Pack(PackArgs),
    /// Pack + upload a skill bundle to the pakx-registry.
    Publish(PublishArgs),
    /// Soft-delete a published version.
    Unpublish(UnpublishArgs),
    /// Rewrite `agents.yml` pins to a newer version, then reinstall.
    ///
    /// Note: this is **package** update — for upgrading the `pakx`
    /// CLI binary itself, see `pakx upgrade`.
    #[command(alias = "up")]
    Update(UpdateArgs),
    /// Check GitHub Releases for a newer pakx version. (Upgrades the
    /// CLI binary itself, not packages — see `pakx update` for that.)
    Upgrade(UpgradeArgs),
    /// Emit shell completion script for bash / zsh / fish / powershell / elvish.
    Completion(CompletionArgs),
    /// Print resolved CLI configuration (paths + registry URLs).
    Config(ConfigArgs),
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // Resolve the process-global color mode from `--color` before any
    // paint helper memoises a stream's color decision. `OnceLock` makes
    // a second call inert, so a re-entry from tests is harmless.
    ui::set_color_mode(cli.color);
    match dispatch(cli.command).await {
        Ok(code) => code,
        Err(e) => {
            // Match the previous `anyhow::main` shape: print the full
            // error chain (`{:#}` shows `with_context` wrapping) then
            // exit 1. anyhow's default Debug impl would also print a
            // backtrace, which we do NOT want on a user-facing CLI.
            eprintln!("Error: {e:#}");
            ExitCode::from(1)
        }
    }
}

/// Subcommand dispatcher. Pulled out of `main` so error rendering +
/// exit-code mapping live in one place. Most commands return `()` on
/// success — `outdated` and `update` are the exceptions (the former
/// propagates a non-zero exit code when any dep has drifted; the
/// latter maps install failure to `1` and "could not determine
/// target version" to `2`, both CI-friendly).
async fn dispatch(cmd: Command) -> Result<ExitCode> {
    match cmd {
        Command::Init(args) => init::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Add(args) => add::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Remove(args) => remove_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Install(args) => install_cmd::run_cmd(args).await.map(|()| ExitCode::SUCCESS),
        Command::List(args) => list_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Outdated(args) => outdated_cmd::run(args).await,
        Command::Doctor(args) => doctor::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Search(args) => search::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Test(args) => test_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Info(args) => info_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Login(args) => login_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Whoami(args) => whoami_cmd::run(args).await,
        Command::Pack(args) => pack_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Publish(args) => publish_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Unpublish(args) => unpublish_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Update(args) => update_cmd::run(args).await,
        Command::Upgrade(args) => upgrade_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
        Command::Completion(args) => completion_cmd::run::<Cli>(args)
            .await
            .map(|()| ExitCode::SUCCESS),
        Command::Config(args) => config_cmd::run(args).await.map(|()| ExitCode::SUCCESS),
    }
}
