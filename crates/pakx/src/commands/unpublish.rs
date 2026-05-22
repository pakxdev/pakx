//! `pakx unpublish <owner>/<name>@<version>` — soft-delete a pinned version.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use pakx_core::{Credentials, DEFAULT_REGISTRY_URL};
use pakx_registry_client::PakxBackend;

use crate::ui;

#[derive(Debug, Clone, Args)]
pub struct UnpublishArgs {
    /// Spec `<owner>/<name>@<version>`.
    pub spec: String,

    /// Override the pakx-registry base URL. Defaults to <https://registry.pakx.dev>.
    #[arg(short = 'r', long = "registry", default_value = DEFAULT_REGISTRY_URL)]
    pub registry: String,

    #[arg(long, hide = true)]
    pub credentials_file: Option<PathBuf>,
}

pub async fn run(args: UnpublishArgs) -> Result<()> {
    let (owner, name, version) = parse_spec(&args.spec)?;
    let creds_path = match args.credentials_file {
        Some(p) => p,
        None => Credentials::default_path().context("resolve credentials path")?,
    };
    let creds = Credentials::read_from(&creds_path).context("read credentials")?;
    let entry = creds
        .get(&args.registry)
        .ok_or_else(|| anyhow!("not logged in to {} — run `pakx login`", args.registry))?;

    let backend = PakxBackend::new(&args.registry);
    backend
        .unpublish(&entry.token, owner, name, version)
        .await?;
    // Keep the legacy `unpublished <owner>/<name>@<version>` substring
    // so existing test asserts still match, but follow it with a
    // dimmed `deprecated` hint that surfaces the real backend
    // semantics — 30-day soft-delete grace, not a hard removal.
    eprintln!(
        "{} {}",
        ui::glyph_ok_err(),
        ui::success_err(&format!("unpublished {owner}/{name}@{version}"))
    );
    eprintln!(
        "{}",
        ui::dim_err(&format!(
            "\u{2192} deprecated {owner}/{name}@{version}: 30-day soft-delete grace; resolves to 404 after the window closes"
        ))
    );
    Ok(())
}

fn parse_spec(spec: &str) -> Result<(&str, &str, &str)> {
    let (lhs, version) = spec
        .rsplit_once('@')
        .ok_or_else(|| anyhow!("spec {spec:?} must be <owner>/<name>@<version>"))?;
    let (owner, name) = lhs
        .split_once('/')
        .ok_or_else(|| anyhow!("spec {spec:?} must be <owner>/<name>@<version>"))?;
    if owner.is_empty() || name.is_empty() || version.is_empty() {
        anyhow::bail!("spec {spec:?} has an empty owner/name/version segment");
    }
    Ok((owner, name, version))
}
