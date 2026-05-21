//! `pakx login` — exchange a `pakx_v1_…` API key for stored credentials.
//!
//! v0 flow: the user opens the pakx-registry dashboard in a browser,
//! creates a token there, and pastes it back into this command. A
//! future Phase C v2 will add a GitHub device flow that mediates the
//! whole thing.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use pakx_core::{CredentialEntry, Credentials, DEFAULT_REGISTRY_URL};
use pakx_registry_client::{BackendError, PakxBackend};

#[derive(Debug, Clone, Args)]
pub struct LoginArgs {
    /// Registry to log in to. Defaults to <https://registry.pakx.dev>.
    #[arg(short = 'r', long = "registry", default_value = DEFAULT_REGISTRY_URL)]
    pub registry: String,

    /// Paste the `pakx_v1_…` token here directly. If omitted we prompt.
    #[arg(long, env = "PAKX_TOKEN")]
    pub token: Option<String>,

    /// Override the credentials file path (testing).
    #[arg(long, hide = true)]
    pub credentials_file: Option<PathBuf>,
}

pub async fn run(args: LoginArgs) -> Result<()> {
    let token = match args.token {
        Some(t) => t.trim().to_owned(),
        None => prompt_for_token(&args.registry)?,
    };
    if !token.starts_with("pakx_v1_") {
        anyhow::bail!("token does not look like a pakx_v1_ key");
    }

    let backend = PakxBackend::new(&args.registry);
    let me = backend.whoami(&token).await.map_err(|e| match e {
        BackendError::Unauthorized => {
            anyhow::anyhow!("registry rejected the token (401) — generate a fresh one")
        }
        other => anyhow::anyhow!(other),
    })?;

    let path = match args.credentials_file {
        Some(p) => p,
        None => Credentials::default_path().context("resolve credentials path")?,
    };
    let mut creds = Credentials::read_from(&path).context("read credentials")?;
    creds.set(
        &args.registry,
        CredentialEntry {
            token,
            login: Some(me.login.clone()),
            created_at: Some(now_iso()),
        },
    );
    creds.write_to(&path).context("write credentials")?;

    eprintln!(
        "logged in to {} as {} (creds saved to {})",
        args.registry,
        me.login,
        path.display()
    );
    Ok(())
}

fn prompt_for_token(registry: &str) -> Result<String> {
    use inquire::Password;
    let token = Password::new(&format!("paste pakx_v1_ token for {registry}:"))
        .without_confirmation()
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .prompt()
        .context("token prompt failed")?;
    Ok(token.trim().to_owned())
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!("epoch:{now}")
}
