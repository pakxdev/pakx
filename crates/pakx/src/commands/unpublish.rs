//! `pakx unpublish <owner>/<name>@<version>` — soft-delete a pinned version.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use inquire::Confirm;
use pakx_core::{Credentials, DEFAULT_REGISTRY_URL};
use pakx_registry_client::PakxBackend;

use crate::registry_url::validate_base_url;
use crate::ui;

#[derive(Debug, Clone, Args)]
pub struct UnpublishArgs {
    /// Spec `<owner>/<name>@<version>`.
    pub spec: String,

    /// Override the pakx-registry base URL. Defaults to <https://registry.pakx.dev>.
    #[arg(short = 'r', long = "registry", default_value = DEFAULT_REGISTRY_URL)]
    pub registry: String,

    /// Skip the confirmation prompt. Required when stdin is not a TTY
    /// (CI / piped shell) — otherwise the command bails rather than
    /// silently unpublishing.
    #[arg(short = 'y', long)]
    pub yes: bool,

    #[arg(long, hide = true)]
    pub credentials_file: Option<PathBuf>,
}

pub async fn run(args: UnpublishArgs) -> Result<()> {
    // Vet any user-supplied `--registry` BEFORE the credentials lookup
    // or any HTTP work. `unpublish` sends a bearer-authed DELETE; a
    // userinfo-smuggled override would exfiltrate the token. Mirrors
    // `pakx publish` / `pakx login` discipline.
    if args.registry != DEFAULT_REGISTRY_URL {
        validate_base_url(&args.registry)?;
    }
    let (owner, name, version) = parse_spec(&args.spec)?;
    let creds_path = match args.credentials_file {
        Some(p) => p,
        None => Credentials::default_path().context("resolve credentials path")?,
    };
    let creds = Credentials::read_from(&creds_path).context("read credentials")?;
    let entry = creds
        .get(&args.registry)
        .ok_or_else(|| anyhow!("not logged in to {} — run `pakx login`", args.registry))?;

    // Destructive: confirm before the soft-delete DELETE. A typo'd id
    // (`pakx unpublish alice/widgett@0.1.0`) must not silently mutate
    // the registry. With `--yes` the caller already consented; on a
    // non-TTY without `--yes` this bails with a hint rather than hanging
    // on a prompt that can never be answered (mirrors `pakx remove`).
    let action = format!("unpublish {owner}/{name}@{version}");
    if !ui::confirm_or_bail(args.yes, &action, || {
        Confirm::new(&format!(
            "Unpublish {owner}/{name}@{version}? (deprecates the version on the registry)"
        ))
        .with_default(false)
        .prompt()
        .map_err(|e| anyhow!("prompt failed: {e}"))
    })? {
        eprintln!("aborted; registry unchanged");
        return Ok(());
    }

    let backend = PakxBackend::new(&args.registry);
    backend
        .unpublish(&entry.token, owner, name, version)
        .await?;
    // Keep the legacy `unpublished <owner>/<name>@<version>` substring
    // so existing test asserts still match, but follow it with a
    // dimmed `deprecated` hint that surfaces the **real** backend
    // semantics. The earlier "30-day soft-delete grace; resolves to
    // 404 after the window closes" copy was aspirational — no
    // hard-delete cron exists on `pakx-registry`, so a deprecated
    // version stays resolvable forever (existing pins keep working);
    // it's simply hidden from list endpoints. See the CHANGELOG note
    // for the 2026-05-23 correction.
    eprintln!(
        "{} {}",
        ui::glyph_ok_err(),
        ui::success_err(&format!("unpublished {owner}/{name}@{version}"))
    );
    eprintln!(
        "{}",
        ui::dim_err(&format!(
            "\u{2192} deprecated {owner}/{name}@{version}: still resolvable for existing pins but hidden from list endpoints"
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
