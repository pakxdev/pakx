//! `pakx whoami` — print the GitHub login pakx is authenticated as.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use pakx_core::{Credentials, DEFAULT_REGISTRY_URL};
use pakx_registry_client::PakxBackend;

#[derive(Debug, Clone, Args)]
pub struct WhoamiArgs {
    #[arg(short = 'r', long = "registry", default_value = DEFAULT_REGISTRY_URL)]
    pub registry: String,

    /// Skip the network round-trip; just print what's stored locally.
    #[arg(long)]
    pub offline: bool,

    #[arg(long, hide = true)]
    pub credentials_file: Option<PathBuf>,
}

pub async fn run(args: WhoamiArgs) -> Result<()> {
    let path = match args.credentials_file {
        Some(p) => p,
        None => Credentials::default_path().context("resolve credentials path")?,
    };
    let creds = Credentials::read_from(&path).context("read credentials")?;
    let entry = creds
        .get(&args.registry)
        .ok_or_else(|| anyhow!("not logged in to {} — run `pakx login`", args.registry))?;

    if args.offline {
        let login = entry
            .login
            .clone()
            .unwrap_or_else(|| "(unknown)".to_owned());
        println!("{login}");
        return Ok(());
    }

    let backend = PakxBackend::new(&args.registry);
    let me = backend.whoami(&entry.token).await?;
    println!("{}", me.login);
    Ok(())
}
