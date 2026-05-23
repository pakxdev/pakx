//! `pakx whoami` — print the GitHub login pakx is authenticated as.
//!
//! Human render: a single coloured line ("alice" / "not logged in").
//! With `--json`, emits a single-line JSON object (newline-terminated)
//! that matches the `--json` style of `pakx list` / `pakx info` so
//! pipelines can `jq` the result. Field names are a stable contract:
//!
//! ```text
//!   { "login": <string|null>,
//!     "id": <string|null>,
//!     "email": <string|null>,
//!     "registry": <string>,
//!     "source": "online" | "cached" | "none" }
//! ```
//!
//! Exit code is `0` when logged in (online or cached) and `1` when
//! there is no stored entry for the targeted registry, so a script
//! can branch on the exit code without parsing JSON.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use pakx_core::{CredentialEntry, Credentials, DEFAULT_REGISTRY_URL};
use pakx_registry_client::PakxBackend;
use serde::Serialize;

use crate::registry_url::validate_base_url;
use crate::ui;

#[derive(Debug, Clone, Args)]
pub struct WhoamiArgs {
    /// Registry to interrogate. Defaults to <https://registry.pakx.dev>.
    #[arg(short = 'r', long = "registry", default_value = DEFAULT_REGISTRY_URL)]
    pub registry: String,

    /// Skip the network round-trip; just print what's stored locally.
    #[arg(long)]
    pub offline: bool,

    /// Emit machine-readable JSON on stdout (single line, newline-terminated).
    /// Field names are a stable contract for downstream pipelines.
    #[arg(long)]
    pub json: bool,

    #[arg(long, hide = true)]
    pub credentials_file: Option<PathBuf>,
}

/// Wire-format payload emitted by `--json`. Field names are a stable
/// contract — only additive changes (new optional fields) are
/// backwards-compatible. `source` is one of `"online"`, `"cached"`,
/// `"none"`; `login` / `id` / `email` are `null` when unavailable
/// (e.g. no stored entry, or a backend that does not surface an
/// `email`).
#[derive(Debug, Serialize)]
struct JsonPayload<'a> {
    login: Option<&'a str>,
    id: Option<&'a str>,
    email: Option<&'a str>,
    registry: &'a str,
    source: &'static str,
}

pub async fn run(args: WhoamiArgs) -> Result<ExitCode> {
    // Vet any user-supplied `--registry` BEFORE any HTTP work. The
    // bearer token would be sent to whatever host the URL resolves to;
    // a userinfo-smuggled override would exfiltrate it. Mirrors `pakx
    // login` / `pakx install` discipline.
    if args.registry != DEFAULT_REGISTRY_URL {
        validate_base_url(&args.registry)?;
    }
    let path = match args.credentials_file.clone() {
        Some(p) => p,
        None => Credentials::default_path().context("resolve credentials path")?,
    };
    let creds = Credentials::read_from(&path).context("read credentials")?;
    let entry = creds.get(&args.registry);

    // No stored entry — emit a "none" payload (JSON) or the human
    // error line. Both paths exit 1 so scripts can branch on the
    // process exit code without parsing JSON.
    let Some(entry) = entry else {
        if args.json {
            emit_json(&JsonPayload {
                login: None,
                id: None,
                email: None,
                registry: &args.registry,
                source: "none",
            })?;
            return Ok(ExitCode::from(1));
        }
        return Err(anyhow!(
            "not logged in to {} — run `pakx login`",
            args.registry
        ));
    };

    if args.offline {
        return Ok(emit_cached(&args, entry));
    }

    let backend = PakxBackend::new(&args.registry);
    match backend.whoami(&entry.token).await {
        Ok(me) => {
            if args.json {
                emit_json(&JsonPayload {
                    login: Some(me.login.as_str()),
                    id: Some(me.id.as_str()),
                    email: me.email.as_deref(),
                    registry: &args.registry,
                    source: "online",
                })?;
            } else {
                println!("{}", ui::success(&me.login));
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            // In `--json` mode a transient network failure silently
            // degrades to the cached entry so pipelines do not break
            // on offline / DNS-blocked hosts. The cached output is
            // distinguishable from the online output via the
            // `"source": "cached"` discriminator, so callers can
            // detect the degradation if they care. The human path
            // still surfaces the error verbatim — interactive users
            // expect to see what went wrong.
            if args.json {
                return Ok(emit_cached(&args, entry));
            }
            Err(e.into())
        }
    }
}

/// Emit the cached payload (offline or network-failure fallback) and
/// return the matching exit code. Factored out so the `--offline`
/// branch and the network-failure branch share one rendering path.
fn emit_cached(args: &WhoamiArgs, entry: &CredentialEntry) -> ExitCode {
    if args.json {
        let payload = JsonPayload {
            login: entry.login.as_deref(),
            id: None,
            email: None,
            registry: &args.registry,
            source: "cached",
        };
        if let Err(e) = emit_json(&payload) {
            eprintln!("Error: {e:#}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    let login = entry
        .login
        .clone()
        .unwrap_or_else(|| "(unknown)".to_owned());
    println!("{}", ui::success(&login));
    ExitCode::SUCCESS
}

/// Serialise + print a single-line JSON line (newline-terminated). The
/// shape matches the `pakx list --json` / `pakx info --json` style.
fn emit_json(payload: &JsonPayload<'_>) -> Result<()> {
    let line = serde_json::to_string(payload).context("serialize whoami as json")?;
    println!("{line}");
    Ok(())
}
