//! `pakx login` — obtain and store a `pakx_v1_…` API token.
//!
//! Two flows:
//!
//! - **Device authorization grant** (`--device`, default since 0.1.4):
//!   the CLI calls `POST /api/v1/auth/device`, prints the user-code,
//!   tries to launch the verification URL in a browser, then polls
//!   `POST /api/v1/auth/device/poll` until the registry returns
//!   `success` (token), `denied`, `expired`, or the local 600s timeout
//!   trips. RFC-8628-style `slow_down` responses bump the local poll
//!   interval by at least 5 seconds (RFC 8628 §3.5 floor).
//!
//! - **Token paste** (`--token <pakx_v1_…>` or `PAKX_TOKEN` env): the
//!   legacy v0.1.0 flow. Verified against `GET /api/v1/whoami` and
//!   stored alongside the device-flow tokens. Useful for CI runners
//!   that cannot reach an interactive browser.
//!
//! Default mode flipped from token-prompt to `--device` in 0.1.4 (see
//! `CHANGELOG.md` under Unreleased). If neither `--device` nor
//! `--token` is supplied, the device flow runs.
//!
//! ## Security notes
//!
//! - The poll loop uses [`std::time::Instant`] (monotonic) for the
//!   600-second total timeout. NTP slew on `SystemTime` would otherwise
//!   either prolong or short-circuit the loop unpredictably.
//! - Tokens never reach `stdout`. Status / hint lines render on
//!   `stderr` so callers piping `pakx login` get a clean stdout.
//! - `tracing` lines that touch the token use `tracing::debug!` only
//!   and always print a redacted prefix (`pakx_v1_********`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use pakx_core::{CredentialEntry, Credentials, DEFAULT_REGISTRY_URL};
use pakx_registry_client::{
    BackendError, DeviceAuthClient, DeviceAuthError, InitiateRequest, InitiateResponse,
    PakxBackend, PollStatus,
};

use crate::registry_url::validate_base_url;
use crate::ui;

/// Hard cap on `clientHostname` / `clientOs` so we never exceed the
/// registry's 80-char validation even if a user has an exotically long
/// hostname (some corp domains push 64+ chars before TLD).
const CLIENT_LABEL_MAX_LEN: usize = 80;

/// Fallback interval bump when the server returns `slow_down` without
/// supplying a fresh `interval` value. RFC 8628 §3.5 floor.
const SLOW_DOWN_BUMP_SECS: u64 = 5;

/// Total device-flow window. Matches `expires_in` in the contract.
/// Overridable via `--timeout-secs` for tests.
const DEFAULT_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, Args)]
pub struct LoginArgs {
    /// Registry to log in to. Defaults to <https://registry.pakx.dev>.
    #[arg(short = 'r', long = "registry", default_value = DEFAULT_REGISTRY_URL)]
    pub registry: String,

    /// Use the GitHub device authorization grant flow (default since
    /// 0.1.4). The CLI prints a user-code and verification URL, opens
    /// the URL in a browser when possible, and polls until the user
    /// approves on the dashboard. Mutually exclusive with `--token`.
    #[arg(long, conflicts_with = "token")]
    pub device: bool,

    /// Paste a `pakx_v1_…` token directly (legacy v0.1.0 flow). Also
    /// reads from the `PAKX_TOKEN` environment variable. Mutually
    /// exclusive with `--device`.
    #[arg(long, env = "PAKX_TOKEN")]
    pub token: Option<String>,

    /// Skip launching the verification URL in a browser. Useful on
    /// headless boxes and required for the integration tests so the
    /// suite never spawns a real browser. Device flow only.
    #[arg(long, hide = true)]
    pub no_open: bool,

    /// Override the poll interval (seconds) returned by the registry.
    /// Test affordance only — production code respects the server-
    /// supplied interval and the `slow_down` bumps that follow.
    #[arg(long, hide = true)]
    pub poll_interval_secs: Option<u64>,

    /// Override the total device-flow window. Defaults to 600. Test
    /// affordance — keep production callers off this flag so the CLI
    /// stays aligned with `expires_in`.
    #[arg(long, hide = true)]
    pub timeout_secs: Option<u64>,

    /// Override the credentials file path (testing).
    #[arg(long, hide = true)]
    pub credentials_file: Option<PathBuf>,
}

pub async fn run(args: LoginArgs) -> Result<()> {
    // Validate the registry override the same way every other network-
    // touching subcommand does. The base URL flows into both the device-
    // auth client and the whoami probe; a userinfo-smuggled URL would
    // exfiltrate the user-code on the way in.
    if args.registry != DEFAULT_REGISTRY_URL {
        validate_base_url(&args.registry)?;
    }

    // Direct paste / env-var path: legacy token flow. Skip device when
    // the user (or `PAKX_TOKEN`) provided a token explicitly. clap's
    // `conflicts_with` already rules out `--device --token`.
    if let Some(token) = args.token.clone() {
        return run_token_flow(&args, token).await;
    }

    // Default + explicit `--device`: device authorization grant.
    run_device_flow(&args).await
}

async fn run_token_flow(args: &LoginArgs, token: String) -> Result<()> {
    let token = token.trim().to_owned();
    if !token.starts_with("pakx_v1_") {
        anyhow::bail!("token does not look like a pakx_v1_ key");
    }

    let backend = PakxBackend::new(&args.registry);
    let pb = ui::spinner(format!("verifying token against {}", args.registry));
    let me = backend.whoami(&token).await.map_err(|e| match e {
        BackendError::Unauthorized => {
            anyhow::anyhow!("registry rejected the token (401) — generate a fresh one")
        }
        other => anyhow::anyhow!(other),
    });
    pb.finish_and_clear();
    let me = me?;

    persist_credentials(args, &token, &me.login)?;

    eprintln!(
        "{} logged in to {} as {}",
        ui::glyph_ok_err(),
        args.registry,
        ui::success_err(&me.login),
    );
    print_credentials_hint(args)?;
    Ok(())
}

async fn run_device_flow(args: &LoginArgs) -> Result<()> {
    let client = DeviceAuthClient::new(&args.registry);

    let pb = ui::spinner(format!("requesting device code from {}", args.registry));
    let init = client
        .initiate(&build_initiate_request())
        .await
        .map_err(map_device_err);
    pb.finish_and_clear();
    let init = init?;

    print_user_instructions(&init);

    // Best-effort browser launch. Soft-fail — a closed-source corp
    // box might not even have a default browser, and the user can
    // still paste the URL manually.
    if !args.no_open {
        let _ = try_open_browser(&init.verification_uri_complete);
    }

    let total_timeout = Duration::from_secs(args.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let mut interval = Duration::from_secs(args.poll_interval_secs.unwrap_or(init.interval).max(1));

    let token = poll_until_done(&client, &init.device_code, &mut interval, total_timeout).await?;

    // Verify the freshly-minted token resolves to a real login.
    // Belt-and-braces — the registry already confirmed user identity
    // on the approval page, but the same whoami probe is part of the
    // legacy token flow and keeps both paths writing the same payload
    // shape.
    let backend = PakxBackend::new(&args.registry);
    let me = backend.whoami(&token).await.map_err(|e| match e {
        BackendError::Unauthorized => {
            anyhow::anyhow!("registry returned a token the whoami endpoint refuses (401)")
        }
        other => anyhow::anyhow!(other),
    })?;

    persist_credentials(args, &token, &me.login)?;

    eprintln!(
        "{} signed in to {} as {}",
        ui::glyph_ok_err(),
        args.registry,
        ui::success_err(&me.login),
    );
    print_credentials_hint(args)?;
    Ok(())
}

/// Poll loop body. Pulled out as a function so the timeout / interval
/// invariants stay localised and the surrounding orchestration in
/// `run_device_flow` reads top-to-bottom.
///
/// Returns the `pakx_v1_…` token on `Success`. On `Denied` / `Expired`
/// / total-timeout, returns an `anyhow::Error` whose `Display` message
/// matches the contract documented in `LoginArgs`.
///
/// Security note: timing uses [`Instant`] (monotonic) so a wall-clock
/// jump from NTP cannot prematurely expire the loop or extend it past
/// the registry's own `expires_in` window.
async fn poll_until_done(
    client: &DeviceAuthClient,
    device_code: &str,
    interval: &mut Duration,
    total_timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + total_timeout;

    loop {
        if Instant::now() >= deadline {
            anyhow::bail!("sign-in window expired, run `pakx login --device` again");
        }
        // Sleep BEFORE the first poll — RFC 8628 §3.5 says the device
        // SHOULD wait `interval` seconds between polls; the first poll
        // wait is bundled in so we don't slam the registry the instant
        // the initiate response lands.
        tokio::time::sleep(*interval).await;

        let poll = client.poll(device_code).await.map_err(map_device_err)?;
        let status = &poll.status;
        tracing::debug!(target: "pakx::login", ?status, "poll response");

        match poll.status {
            PollStatus::Pending => {}
            PollStatus::SlowDown => {
                // RFC 8628 §3.5: bump by at least 5 seconds. If the
                // server hands us a fresh interval, take the max of
                // (server_interval, current + 5) so an aggressive
                // server can wind us down further but cannot speed us
                // up below the 5-second floor.
                let bumped = *interval + Duration::from_secs(SLOW_DOWN_BUMP_SECS);
                let server_hint = poll
                    .interval
                    .map_or(bumped, |s| Duration::from_secs(s).max(bumped));
                *interval = server_hint;
                tracing::debug!(
                    target: "pakx::login",
                    new_interval_secs = interval.as_secs(),
                    "slow_down: bumped poll interval",
                );
                eprintln!(
                    "{}",
                    ui::dim_err(&format!(
                        "\u{2192} polling too fast — backing off to {}s",
                        interval.as_secs()
                    )),
                );
            }
            PollStatus::Denied => {
                anyhow::bail!("sign-in denied");
            }
            PollStatus::Expired => {
                anyhow::bail!("sign-in window expired, run `pakx login --device` again");
            }
            PollStatus::Success => {
                let token = poll.token.ok_or_else(|| {
                    anyhow::anyhow!("registry returned status=success without a token field")
                })?;
                return Ok(token);
            }
        }
    }
}

fn map_device_err(e: DeviceAuthError) -> anyhow::Error {
    anyhow::anyhow!(e)
}

/// Build the initiate request body. Hostname pulled from
/// [`gethostname`]; OS / arch from [`std::env::consts`]. Each field is
/// truncated to 80 chars to match the registry's max validation.
/// Unknown / empty values are omitted (the field is optional on the
/// wire — `skip_serializing_if = Option::is_none`).
fn build_initiate_request() -> InitiateRequest {
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let hostname = trim_label(&hostname);
    let os_string = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    let os_string = trim_label(&os_string);
    InitiateRequest {
        client_hostname: hostname,
        client_os: os_string,
    }
}

/// Truncate a label to the registry's 80-char limit, returning `None`
/// when the input is empty so the field is omitted on the wire entirely
/// (instead of sending `""`).
fn trim_label(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= CLIENT_LABEL_MAX_LEN {
        return Some(trimmed.to_owned());
    }
    // char-boundary safe truncate: take chars up to the byte limit.
    let mut out = String::with_capacity(CLIENT_LABEL_MAX_LEN);
    for ch in trimmed.chars() {
        if out.len() + ch.len_utf8() > CLIENT_LABEL_MAX_LEN {
            break;
        }
        out.push(ch);
    }
    Some(out)
}

fn print_user_instructions(init: &InitiateResponse) {
    // All instruction lines go to stderr so a caller that pipes the
    // command (e.g. for the JSON-emitting flow we'll add later) still
    // gets a clean stdout. We never print the token here — that's
    // strictly the `success` poll branch.
    eprintln!("To sign in, open this URL in your browser:");
    eprintln!();
    eprintln!("  {}", ui::success_err(&init.verification_uri_complete));
    eprintln!();
    eprintln!("Or visit {} and enter:", init.verification_uri);
    eprintln!();
    eprintln!("  {}", ui::success_err(&init.user_code));
    eprintln!();
    eprintln!("Waiting for confirmation...");
}

/// Cross-platform "open this URL" using the platform's native handler.
/// Soft-fails — every error path is intentionally swallowed because the
/// printed instructions above already cover the manual case.
fn try_open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command as StdCommand;
        // `start` is a cmd builtin, not a standalone executable.
        // Spawning `cmd /C start "" "<url>"` is the documented
        // incantation; the empty string is the window title.
        StdCommand::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
            .context("failed to launch browser via cmd start")?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command as StdCommand;
        StdCommand::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .context("failed to launch browser via macOS open")?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::process::Command as StdCommand;
        // xdg-open is the freedesktop default; fall back to nothing —
        // a headless / container box without xdg-utils just gets the
        // manual instructions we already printed.
        StdCommand::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .context("failed to launch browser via xdg-open")?;
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = url; // silence unused on exotic targets
        Ok(())
    }
}

fn persist_credentials(args: &LoginArgs, token: &str, login: &str) -> Result<()> {
    let path = match args.credentials_file.clone() {
        Some(p) => p,
        None => Credentials::default_path().context("resolve credentials path")?,
    };
    let mut creds = Credentials::read_from(&path).context("read credentials")?;
    creds.set(
        &args.registry,
        CredentialEntry {
            token: token.to_owned(),
            login: Some(login.to_owned()),
            created_at: Some(now_iso()),
        },
    );
    creds.write_to(&path).context("write credentials")?;
    Ok(())
}

fn print_credentials_hint(args: &LoginArgs) -> Result<()> {
    let path = match args.credentials_file.clone() {
        Some(p) => p,
        None => Credentials::default_path().context("resolve credentials path")?,
    };
    // Single dimmed hint pointing at the on-disk credentials path,
    // surfacing the unix mode so users can `ls -l` to verify the
    // 0600 contract (no value on Windows but the path is still
    // useful for the manual `attrib` / GPO audit).
    let mode_note = if cfg!(unix) { " (mode 0600)" } else { "" };
    eprintln!(
        "{}",
        ui::dim_err(&format!(
            "\u{2192} credentials: {}{}",
            path.display(),
            mode_note,
        ))
    );
    Ok(())
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!("epoch:{now}")
}

#[cfg(test)]
mod tests {
    use super::{trim_label, CLIENT_LABEL_MAX_LEN};

    #[test]
    fn trim_label_returns_none_for_empty() {
        assert!(trim_label("").is_none());
        assert!(trim_label("   ").is_none());
    }

    #[test]
    fn trim_label_returns_input_when_under_limit() {
        assert_eq!(trim_label("host.local"), Some("host.local".to_owned()));
    }

    #[test]
    fn trim_label_truncates_at_limit() {
        let long = "a".repeat(200);
        let out = trim_label(&long).unwrap();
        assert_eq!(out.len(), CLIENT_LABEL_MAX_LEN);
    }

    #[test]
    fn trim_label_truncates_on_char_boundary() {
        // Multibyte char that would straddle the byte limit if we
        // truncated at byte 80 directly. `é` is 2 bytes in UTF-8.
        let s: String = "é".repeat(50); // 100 bytes
        let out = trim_label(&s).unwrap();
        assert!(out.len() <= CLIENT_LABEL_MAX_LEN);
        // Round-trips as valid UTF-8 (the `chars()` iteration in
        // `trim_label` guarantees this — the assertion is the safety
        // net for any future re-write).
        assert!(out.chars().all(|c| c == 'é'));
    }
}
