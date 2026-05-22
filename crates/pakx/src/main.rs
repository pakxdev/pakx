//! `pakx` CLI entrypoint.

// `unreachable_pub` is meaningful only for library crates with an external
// API surface. In a binary crate every `pub` item is internal by
// construction, so the workspace lint just produces noise.
#![allow(unreachable_pub)]

mod commands;
mod install;
mod pack;
mod registry_url;
mod resolve;

use anyhow::Result;
use clap::{Parser, Subcommand};

use commands::add::{self, AddArgs};
use commands::completion::{self as completion_cmd, CompletionArgs};
use commands::config::{self as config_cmd, ConfigArgs};
use commands::doctor::{self, DoctorArgs};
use commands::info::{self as info_cmd, InfoArgs};
use commands::init::{self, InitArgs};
use commands::install::{self as install_cmd, InstallArgs};
use commands::list::{self as list_cmd, ListArgs};
use commands::login::{self as login_cmd, LoginArgs};
use commands::pack::{self as pack_cmd, PackArgs};
use commands::publish::{self as publish_cmd, PublishArgs};
use commands::remove::{self as remove_cmd, RemoveArgs};
use commands::search::{self, SearchArgs};
use commands::test::{self as test_cmd, TestArgs};
use commands::unpublish::{self as unpublish_cmd, UnpublishArgs};
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
    /// Check GitHub Releases for a newer pakx version.
    #[command(alias = "update")]
    Upgrade(UpgradeArgs),
    /// Emit shell completion script for bash / zsh / fish / powershell / elvish.
    Completion(CompletionArgs),
    /// Print resolved CLI configuration (paths + registry URLs).
    Config(ConfigArgs),
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init(args) => init::run(args).await,
        Command::Add(args) => add::run(args).await,
        Command::Remove(args) => remove_cmd::run(args).await,
        Command::Install(args) => install_cmd::run_cmd(args).await,
        Command::List(args) => list_cmd::run(args).await,
        Command::Doctor(args) => doctor::run(args).await,
        Command::Search(args) => search::run(args).await,
        Command::Test(args) => test_cmd::run(args).await,
        Command::Info(args) => info_cmd::run(args).await,
        Command::Login(args) => login_cmd::run(args).await,
        Command::Whoami(args) => whoami_cmd::run(args).await,
        Command::Pack(args) => pack_cmd::run(args).await,
        Command::Publish(args) => publish_cmd::run(args).await,
        Command::Unpublish(args) => unpublish_cmd::run(args).await,
        Command::Upgrade(args) => upgrade_cmd::run(args).await,
        Command::Completion(args) => completion_cmd::run::<Cli>(args).await,
        Command::Config(args) => config_cmd::run(args).await,
    }
}
