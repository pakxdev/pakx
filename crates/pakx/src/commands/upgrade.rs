//! `pakx upgrade` — check GitHub Releases for a newer version and, when
//! one exists, run the upgrade command that matches how *this* binary
//! was installed.
//!
//! The version check + semver comparison is unchanged from v0.1: GET the
//! latest GitHub release, strip the `v` prefix, compare against the
//! compiled-in `CARGO_PKG_VERSION`.
//!
//! What is new is *channel detection*. We inspect `current_exe()` and
//! classify the install channel (cargo / install-script / brew / scoop /
//! unknown), then run the matching upgrade command:
//!
//! | channel | detected by                          | upgrade command                                  |
//! |---------|--------------------------------------|--------------------------------------------------|
//! | cargo   | path under `$CARGO_HOME/bin`         | `cargo install pakx-cli --force --locked`        |
//! | script  | path under `~/.pakx/bin`             | re-run `install.sh` (unix) / `install.ps1` (win) |
//! | brew    | path under a Homebrew prefix         | `brew upgrade pakx`                               |
//! | scoop   | path contains `scoop/apps/pakx`      | `scoop update pakx`                              |
//! | unknown | anything else                        | (no auto-run — print the full channel menu)      |
//!
//! Safety: every command we may run is **hardcoded**. No part of the
//! command string is derived from network responses or any other
//! untrusted input — the only network value consumed is the release tag,
//! and that flows solely into the printed "X -> Y" line and the semver
//! comparison, never into a spawned command. There is no shell-injection
//! surface.
//!
//! Windows + script caveat: the install script overwrites
//! `~/.pakx/bin/pakx.exe` in place (`Copy-Item -Force`). Windows locks a
//! running executable, so overwriting the *live* binary from within
//! `pakx upgrade` fails. For the `windows + script` combination we
//! therefore do NOT spawn the installer — we print the command for the
//! user to run from a fresh shell, with a one-line note explaining why.
//! cargo / brew / scoop manage their own binary replacement and are run
//! normally on Windows.

use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use pakx_core::http_client;
use serde::Deserialize;

use crate::registry_url::validate_base_url;
use crate::ui;

const CURRENT: &str = env!("CARGO_PKG_VERSION");
const LATEST_URL: &str = "https://api.github.com/repos/pakxdev/pakx/releases/latest";
const USER_AGENT: &str = concat!("pakx/", env!("CARGO_PKG_VERSION"));

/// Hardcoded install-script URLs. These are constants, never derived
/// from input, so re-running them carries no injection risk.
const INSTALL_SH_URL: &str = "https://pakx.dev/install.sh";
const INSTALL_PS1_URL: &str = "https://pakx.dev/install.ps1";

#[derive(Debug, Clone, Args)]
pub struct UpgradeArgs {
    /// Run the detected channel's upgrade command without prompting.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Read-only: do the version check and print the status / channel
    /// menu, but never run an upgrade command. Use this in CI or any
    /// script that relied on `pakx upgrade` being non-mutating.
    #[arg(long)]
    pub check: bool,

    /// Override the GitHub Releases API URL (testing only).
    #[arg(long, hide = true)]
    pub releases_url: Option<String>,

    /// Override the user-agent header sent to GitHub (testing only).
    #[arg(long, hide = true)]
    pub user_agent: Option<String>,

    /// Force the detected install channel instead of probing
    /// `current_exe()` (testing only). Lets the no-TTY / prompt paths be
    /// exercised for a *known* channel from the cargo-test harness, whose
    /// real exe always classifies as `unknown` (lives under `target/`).
    #[arg(long, hide = true, value_enum)]
    pub force_channel: Option<ForceChannel>,
}

/// Testing-only channel override mirroring [`Channel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum ForceChannel {
    Cargo,
    Script,
    Brew,
    Scoop,
    Unknown,
}

impl From<ForceChannel> for Channel {
    fn from(f: ForceChannel) -> Self {
        match f {
            ForceChannel::Cargo => Self::Cargo,
            ForceChannel::Script => Self::Script,
            ForceChannel::Brew => Self::Brew,
            ForceChannel::Scoop => Self::Scoop,
            ForceChannel::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Release {
    /// e.g. `v0.1.1`.
    tag_name: String,
    html_url: String,
}

pub async fn run(args: UpgradeArgs) -> Result<()> {
    let url = args.releases_url.as_deref().unwrap_or(LATEST_URL);
    let ua = args.user_agent.as_deref().unwrap_or(USER_AGENT);

    // Vet any user-supplied `--releases-url` override BEFORE the HTTP
    // call. The default is hardcoded https so it cannot smuggle, but
    // the hidden test override exists, and applying the validator
    // uniformly across every command keeps the contract simple: every
    // user-supplied base URL goes through `validate_base_url`.
    if args.releases_url.is_some() {
        validate_base_url(url)?;
    }

    let release: Release = http_client()
        .get(url)
        .header("user-agent", ua)
        .header("accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let latest = release.tag_name.trim_start_matches('v');
    let cmp = compare_semver(CURRENT, latest);

    match cmp {
        Ordering::Equal => {
            println!("pakx {} is the latest release.", ui::success(CURRENT));
        }
        Ordering::Greater => {
            println!(
                "pakx {CURRENT} is newer than the latest release ({latest}). Running a dev build?"
            );
        }
        Ordering::Less => {
            handle_upgrade_available(
                latest,
                &release.html_url,
                args.yes,
                args.check,
                args.force_channel.map(Channel::from),
            )?;
        }
    }
    Ok(())
}

/// An upgrade is available. Resolve the install channel, then either
/// auto-run the matching command (after confirmation) or — for Unknown
/// and the windows+script special case — print a command for the user.
fn handle_upgrade_available(
    latest: &str,
    html_url: &str,
    yes: bool,
    check: bool,
    forced: Option<Channel>,
) -> Result<()> {
    println!(
        "A newer pakx is available: {} -> {}",
        ui::dim(CURRENT),
        ui::success(latest),
    );
    println!("{} {}", ui::heading("release notes:"), html_url);
    println!();

    // Resolve how this binary was installed. A `--force-channel` test
    // override short-circuits the probe; otherwise inspect `current_exe`.
    // If we cannot even locate our own exe, treat it as Unknown and fall
    // back to the menu.
    let env = ChannelEnv::from_process();
    let channel = if let Some(c) = forced {
        c
    } else {
        match resolve_current_exe() {
            Ok(p) => detect_channel(&p, &env),
            Err(e) => {
                tracing::debug!("could not resolve current_exe for channel detection: {e:#}");
                print_channel_menu();
                return Ok(());
            }
        }
    };
    let plan = UpgradePlan::for_channel(channel, &env);

    match plan {
        // Unknown channel → preserve the original read-only behaviour.
        UpgradePlan::PrintMenu => {
            print_channel_menu();
            Ok(())
        }
        // `--check`: never run, but tell the user exactly what they (or
        // a re-run without `--check`) would execute.
        UpgradePlan::Run { .. } if check => {
            println!(
                "{} detected install channel: {}",
                ui::heading("channel:"),
                channel.label(),
            );
            println!("  would run: {}", plan.command_display());
            println!("  ({} is set — not running)", ui::dim("--check"));
            Ok(())
        }
        // windows + script: the installer overwrites the live exe and
        // Windows locks running binaries, so we never spawn here. Print
        // the command for a fresh shell instead.
        UpgradePlan::PrintForFreshShell { .. } => {
            println!(
                "{} detected install channel: {}",
                ui::heading("channel:"),
                channel.label(),
            );
            println!(
                "  Windows locks the running pakx.exe, so the install script can't replace it \
                 from inside this process."
            );
            println!("  Run this in a fresh terminal to upgrade:");
            println!("    {}", plan.command_display());
            Ok(())
        }
        UpgradePlan::Run { .. } => {
            println!(
                "upgrade pakx {} -> {} via {} (runs: {})",
                ui::dim(CURRENT),
                ui::success(latest),
                channel.label(),
                plan.command_display(),
            );

            // Confirm before running. With `--yes` this returns Ok(true)
            // without prompting. With no TTY and no `--yes` it bails with
            // an actionable hint (does NOT hang on stdin).
            let action = format!("upgrade pakx via {}", channel.label());
            let confirm = ui::confirm_or_bail(yes, &action, || {
                Ok(inquire::Confirm::new("Run the upgrade now?")
                    .with_default(false)
                    .prompt()
                    .unwrap_or(false))
            });
            let Ok(proceed) = confirm else {
                // No TTY + no --yes: print the command and a hint, then
                // exit 0 (don't hang, don't error the CLI).
                println!("  {}", plan.command_display());
                println!(
                    "  Re-run with {} to upgrade non-interactively, or run the command above \
                     yourself.",
                    ui::dim("--yes"),
                );
                return Ok(());
            };

            if !proceed {
                println!("Upgrade cancelled.");
                return Ok(());
            }

            run_command(&plan)
        }
    }
}

/// Spawn the resolved upgrade command, inheriting stdio so the user sees
/// the package manager's / installer's own progress output. Maps a
/// non-zero child exit into an error so the CLI surfaces the failure.
fn run_command(plan: &UpgradePlan) -> Result<()> {
    // Caller only invokes this on a Run plan; anything else is a no-op.
    let UpgradePlan::Run {
        program,
        args: c_args,
    } = plan
    else {
        return Ok(());
    };

    let mut cmd = ProcessCommand::new(program);
    cmd.args(c_args);
    let status = cmd
        .status()
        .with_context(|| format!("failed to launch `{program}` for upgrade"))?;

    if status.success() {
        println!("{} upgrade command finished.", ui::glyph_ok());
        Ok(())
    } else {
        let code = status.code().unwrap_or(-1);
        Err(anyhow!(
            "upgrade command `{program}` exited with status {code}"
        ))
    }
}

fn print_channel_menu() {
    println!("{}", ui::heading("upgrade via your install channel:"));
    println!("  curl|sh / irm|iex   curl -fsSL {INSTALL_SH_URL} | sh");
    println!("                      irm {INSTALL_PS1_URL} | iex");
    println!("  brew                brew upgrade pakx");
    println!("  scoop               scoop update pakx");
    println!("  cargo               cargo install pakx-cli --force --locked");
}

// ---------------------------------------------------------------------------
// Channel detection
// ---------------------------------------------------------------------------

/// Which install channel this binary came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Installed via `cargo install` (path under `$CARGO_HOME/bin`).
    Cargo,
    /// Installed via the `install.sh` / `install.ps1` script (path under
    /// `~/.pakx/bin`).
    Script,
    /// Installed via Homebrew (path under a Homebrew prefix).
    Brew,
    /// Installed via Scoop (path contains `scoop/apps/pakx`).
    Scoop,
    /// Anything we don't recognise — fall back to printing the menu.
    Unknown,
}

impl Channel {
    const fn label(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Script => "install script",
            Self::Brew => "brew",
            Self::Scoop => "scoop",
            Self::Unknown => "unknown",
        }
    }
}

/// Operating system family, abstracted so `detect_channel` is pure and
/// testable without `cfg!` branching inside the function body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Unix,
}

/// Filesystem + OS facts that drive channel detection, injected so tests
/// can supply synthetic paths without touching the real environment or
/// `current_exe()`.
#[derive(Debug, Clone)]
pub struct ChannelEnv {
    /// `$CARGO_HOME` if set, else `~/.cargo`. `None` if neither resolves.
    pub cargo_home: Option<PathBuf>,
    /// User home directory. `None` if it can't be resolved.
    pub home: Option<PathBuf>,
    /// Homebrew prefix candidates (e.g. `/opt/homebrew`, `/usr/local`).
    pub brew_prefixes: Vec<PathBuf>,
    /// The running OS family.
    pub os: Os,
}

impl ChannelEnv {
    /// Build the detection environment from the live process: read
    /// `CARGO_HOME`, the home dir, and the standard Homebrew prefixes.
    fn from_process() -> Self {
        let home = dirs::home_dir();
        let cargo_home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".cargo")));

        // Standard Homebrew prefixes: Apple-silicon (/opt/homebrew),
        // Intel mac + Linuxbrew (/usr/local), and the common Linuxbrew
        // location. We don't shell out to `brew --prefix` — these cover
        // the documented defaults and keep detection cheap + offline.
        let brew_prefixes = vec![
            PathBuf::from("/opt/homebrew"),
            PathBuf::from("/usr/local"),
            PathBuf::from("/home/linuxbrew/.linuxbrew"),
        ];

        let os = if cfg!(windows) { Os::Windows } else { Os::Unix };

        Self {
            cargo_home,
            home,
            brew_prefixes,
            os,
        }
    }
}

/// Resolve + canonicalize the running executable's path.
fn resolve_current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("could not resolve current executable path")?;
    // Canonicalize so symlinks (e.g. brew's `bin/pakx` -> `Cellar/...`)
    // resolve to their real location before we classify. If canonicalize
    // fails (path vanished mid-run), fall back to the raw path.
    Ok(exe.canonicalize().unwrap_or(exe))
}

/// Classify the install channel from the executable path. Pure: every
/// input comes through `exe` + `env`, so this is exhaustively unit-tested
/// without touching the real filesystem.
#[must_use]
pub fn detect_channel(exe_path: &Path, env: &ChannelEnv) -> Channel {
    // Scoop first: its layout (`.../scoop/apps/pakx/...`) is the most
    // specific and can live under the home dir, so check it before the
    // broader prefix checks.
    if path_contains_segments(exe_path, &["scoop", "apps", "pakx"]) {
        return Channel::Scoop;
    }
    if let Some(home) = &env.home {
        let scoop_root = home.join("scoop");
        if starts_with(exe_path, &scoop_root) {
            return Channel::Scoop;
        }
    }

    // cargo: under `$CARGO_HOME/bin`.
    if let Some(cargo_home) = &env.cargo_home {
        if starts_with(exe_path, &cargo_home.join("bin")) {
            return Channel::Cargo;
        }
    }

    // install script: under `~/.pakx/bin`.
    if let Some(home) = &env.home {
        if starts_with(exe_path, &home.join(".pakx").join("bin")) {
            return Channel::Script;
        }
    }

    // brew: under a Homebrew prefix. A `Cellar` segment anywhere in the
    // path is also a strong Homebrew signal (canonicalized symlinks land
    // in `<prefix>/Cellar/...`).
    if path_contains_segments(exe_path, &["Cellar"]) {
        return Channel::Brew;
    }
    for prefix in &env.brew_prefixes {
        if starts_with(exe_path, prefix) {
            return Channel::Brew;
        }
    }

    Channel::Unknown
}

/// The concrete action `pakx upgrade` will take for a resolved channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradePlan {
    /// Spawn `program` with `args`, inheriting stdio.
    Run { program: String, args: Vec<String> },
    /// Print the command for the user to run in a fresh shell (the
    /// windows + script case — we can't overwrite the live exe).
    PrintForFreshShell { display: String },
    /// Unknown channel — print the full channel menu, run nothing.
    PrintMenu,
}

impl UpgradePlan {
    /// Map a channel to its upgrade plan. Kept separate from
    /// `detect_channel` so the command strings are asserted directly in
    /// unit tests, and so the only line not under test is the actual
    /// `ProcessCommand::status()` spawn.
    #[must_use]
    pub fn for_channel(channel: Channel, env: &ChannelEnv) -> Self {
        match channel {
            Channel::Cargo => Self::Run {
                program: "cargo".into(),
                args: vec![
                    "install".into(),
                    "pakx-cli".into(),
                    "--force".into(),
                    "--locked".into(),
                ],
            },
            Channel::Brew => Self::Run {
                program: "brew".into(),
                args: vec!["upgrade".into(), "pakx".into()],
            },
            Channel::Scoop => Self::Run {
                program: "scoop".into(),
                args: vec!["update".into(), "pakx".into()],
            },
            Channel::Script => match env.os {
                // Unix: re-run install.sh through sh.
                Os::Unix => Self::Run {
                    program: "sh".into(),
                    args: vec!["-c".into(), format!("curl -fsSL {INSTALL_SH_URL} | sh")],
                },
                // Windows: the installer overwrites the running exe, which
                // Windows locks. Don't spawn — print for a fresh shell.
                Os::Windows => Self::PrintForFreshShell {
                    display: format!("irm {INSTALL_PS1_URL} | iex"),
                },
            },
            Channel::Unknown => Self::PrintMenu,
        }
    }

    /// Human-readable rendering of what will run (used in the prompt
    /// line, `--check` output, and the no-TTY hint).
    #[must_use]
    pub fn command_display(&self) -> String {
        match self {
            Self::Run { program, args } => {
                if args.len() == 2 && args[0] == "-c" {
                    // `sh -c "<script>"` → show the inner script directly.
                    return args[1].clone();
                }
                let mut s = program.clone();
                for a in args {
                    s.push(' ');
                    s.push_str(a);
                }
                s
            }
            Self::PrintForFreshShell { display } => display.clone(),
            Self::PrintMenu => "(see channel menu above)".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// path helpers
// ---------------------------------------------------------------------------

/// True when `path` is `base` or lives beneath it. Comparison is
/// component-wise so it doesn't trip on trailing separators or `..`.
fn starts_with(path: &Path, base: &Path) -> bool {
    let mut p = path.components();
    for b in base.components() {
        match p.next() {
            Some(pc) if pc == b => {}
            _ => return false,
        }
    }
    true
}

/// True when `needles` appear as consecutive path components anywhere in
/// `path`. Case-insensitive so `Cellar` / `cellar` and `scoop` variants
/// match across platforms.
fn path_contains_segments(path: &Path, needles: &[&str]) -> bool {
    let comps: Vec<String> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_ascii_lowercase))
        .collect();
    let needles_lc: Vec<String> = needles.iter().map(|n| n.to_ascii_lowercase()).collect();
    if needles_lc.is_empty() || needles_lc.len() > comps.len() {
        return false;
    }
    comps
        .windows(needles_lc.len())
        .any(|w| w == needles_lc.as_slice())
}

// ---------------------------------------------------------------------------
// semver compare (intentionally tiny — no `semver` crate dep just for this)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ordering {
    Less,
    Equal,
    Greater,
}

fn compare_semver(a: &str, b: &str) -> Ordering {
    let parse = |s: &str| -> Result<(u64, u64, u64), anyhow::Error> {
        // Strip any pre-release suffix (`-rc.1`, `+build.5`).
        let core = s.split(['-', '+']).next().unwrap_or(s);
        let mut parts = core.splitn(3, '.').map(str::parse::<u64>);
        let major = parts.next().transpose()?.unwrap_or(0);
        let minor = parts.next().transpose()?.unwrap_or(0);
        let patch = parts.next().transpose()?.unwrap_or(0);
        if parts.next().is_some() {
            return Err(anyhow!("too many version segments"));
        }
        Ok((major, minor, patch))
    };
    let av = parse(a).unwrap_or((0, 0, 0));
    let bv = parse(b).unwrap_or((0, 0, 0));
    match av.cmp(&bv) {
        std::cmp::Ordering::Less => Ordering::Less,
        std::cmp::Ordering::Equal => Ordering::Equal,
        std::cmp::Ordering::Greater => Ordering::Greater,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(home: &str, cargo_home: Option<&str>, os: Os) -> ChannelEnv {
        ChannelEnv {
            cargo_home: cargo_home.map(PathBuf::from),
            home: Some(PathBuf::from(home)),
            brew_prefixes: vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")],
            os,
        }
    }

    // -- detect_channel ----------------------------------------------------

    #[test]
    fn detects_cargo_from_cargo_home_bin() {
        let env = env_with("/home/u", Some("/home/u/.cargo"), Os::Unix);
        let exe = PathBuf::from("/home/u/.cargo/bin/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Cargo);
    }

    #[test]
    fn detects_cargo_when_cargo_home_overridden() {
        // Respect an explicit CARGO_HOME that is not under ~/.cargo.
        let env = env_with("/home/u", Some("/opt/cargo"), Os::Unix);
        let exe = PathBuf::from("/opt/cargo/bin/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Cargo);
    }

    #[test]
    fn detects_script_from_pakx_bin() {
        let env = env_with("/home/u", Some("/home/u/.cargo"), Os::Unix);
        let exe = PathBuf::from("/home/u/.pakx/bin/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Script);
    }

    // Windows-only: backslash paths are only parsed into components by
    // `std::path` on Windows. On a Unix host `PathBuf::from(r"C:\...")` is a
    // single opaque component, so this scenario can only be exercised where
    // `\` is the native separator. The Windows CI leg (main-push + release
    // matrix) covers it; the Unix equivalent is `detects_script_from_pakx_bin`.
    #[cfg(windows)]
    #[test]
    fn detects_script_from_pakx_bin_windows() {
        let env = env_with(r"C:\Users\u", Some(r"C:\Users\u\.cargo"), Os::Windows);
        let exe = PathBuf::from(r"C:\Users\u\.pakx\bin\pakx.exe");
        assert_eq!(detect_channel(&exe, &env), Channel::Script);
    }

    #[test]
    fn detects_brew_from_opt_homebrew_prefix() {
        let env = env_with("/Users/u", Some("/Users/u/.cargo"), Os::Unix);
        let exe = PathBuf::from("/opt/homebrew/bin/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Brew);
    }

    #[test]
    fn detects_brew_from_cellar_segment() {
        let env = env_with("/Users/u", Some("/Users/u/.cargo"), Os::Unix);
        // Canonicalized brew symlink lands in Cellar.
        let exe = PathBuf::from("/opt/homebrew/Cellar/pakx/0.1.6/bin/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Brew);
    }

    #[test]
    fn detects_brew_from_usr_local_prefix() {
        let env = env_with("/Users/u", Some("/Users/u/.cargo"), Os::Unix);
        let exe = PathBuf::from("/usr/local/bin/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Brew);
    }

    // Windows-only for the same backslash-component reason as the script
    // test above. The Unix `scoop/apps/pakx` segment match is covered by
    // `detects_scoop_from_apps_path_unix`.
    #[cfg(windows)]
    #[test]
    fn detects_scoop_from_apps_path() {
        let env = env_with(r"C:\Users\u", Some(r"C:\Users\u\.cargo"), Os::Windows);
        let exe = PathBuf::from(r"C:\Users\u\scoop\apps\pakx\current\pakx.exe");
        assert_eq!(detect_channel(&exe, &env), Channel::Scoop);
    }

    // Unix-representable cover for the `scoop/apps/pakx` consecutive-segment
    // match (keeps `path_contains_segments` exercised on the Linux PR leg).
    #[test]
    fn detects_scoop_from_apps_path_unix() {
        let env = env_with("/home/u", Some("/home/u/.cargo"), Os::Unix);
        let exe = PathBuf::from("/home/u/scoop/apps/pakx/current/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Scoop);
    }

    #[test]
    fn detects_scoop_when_under_home_scoop() {
        let env = env_with("/home/u", Some("/home/u/.cargo"), Os::Unix);
        // scoop layout without the apps/pakx tail but under ~/scoop.
        let exe = PathBuf::from("/home/u/scoop/shims/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Scoop);
    }

    #[test]
    fn random_path_is_unknown() {
        let env = env_with("/home/u", Some("/home/u/.cargo"), Os::Unix);
        let exe = PathBuf::from("/some/random/place/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Unknown);
    }

    #[test]
    fn target_debug_path_is_unknown() {
        // The cargo-test harness path (under target/) must NOT classify
        // as a known channel, or `pakx upgrade` integration tests would
        // try to spawn a package manager.
        let env = env_with("/home/u", Some("/home/u/.cargo"), Os::Unix);
        let exe = PathBuf::from("/work/pakx/target/debug/pakx");
        assert_eq!(detect_channel(&exe, &env), Channel::Unknown);
    }

    // -- UpgradePlan command selection -------------------------------------

    #[test]
    fn plan_cargo_uses_crates_io_force_locked() {
        let env = env_with("/home/u", Some("/home/u/.cargo"), Os::Unix);
        let plan = UpgradePlan::for_channel(Channel::Cargo, &env);
        assert_eq!(
            plan,
            UpgradePlan::Run {
                program: "cargo".into(),
                args: vec![
                    "install".into(),
                    "pakx-cli".into(),
                    "--force".into(),
                    "--locked".into(),
                ],
            }
        );
        assert_eq!(
            plan.command_display(),
            "cargo install pakx-cli --force --locked"
        );
    }

    #[test]
    fn plan_brew_upgrades_pakx() {
        let env = env_with("/Users/u", None, Os::Unix);
        let plan = UpgradePlan::for_channel(Channel::Brew, &env);
        assert_eq!(plan.command_display(), "brew upgrade pakx");
    }

    #[test]
    fn plan_scoop_updates_pakx() {
        let env = env_with(r"C:\Users\u", None, Os::Windows);
        let plan = UpgradePlan::for_channel(Channel::Scoop, &env);
        assert_eq!(plan.command_display(), "scoop update pakx");
    }

    #[test]
    fn plan_script_unix_reruns_install_sh_via_sh() {
        let env = env_with("/home/u", None, Os::Unix);
        let plan = UpgradePlan::for_channel(Channel::Script, &env);
        assert_eq!(
            plan,
            UpgradePlan::Run {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    "curl -fsSL https://pakx.dev/install.sh | sh".into(),
                ],
            }
        );
        // command_display unwraps the `sh -c` to show the inner script.
        assert_eq!(
            plan.command_display(),
            "curl -fsSL https://pakx.dev/install.sh | sh"
        );
    }

    #[test]
    fn plan_script_windows_prints_for_fresh_shell_not_spawn() {
        // The windows+script combination must NOT produce a Run plan —
        // Windows locks the live exe, so we print for a fresh shell.
        let env = env_with(r"C:\Users\u", None, Os::Windows);
        let plan = UpgradePlan::for_channel(Channel::Script, &env);
        assert_eq!(
            plan,
            UpgradePlan::PrintForFreshShell {
                display: "irm https://pakx.dev/install.ps1 | iex".into(),
            }
        );
        assert!(matches!(plan, UpgradePlan::PrintForFreshShell { .. }));
    }

    #[test]
    fn plan_unknown_is_print_menu() {
        let env = env_with("/home/u", None, Os::Unix);
        assert_eq!(
            UpgradePlan::for_channel(Channel::Unknown, &env),
            UpgradePlan::PrintMenu
        );
    }

    // -- path helpers ------------------------------------------------------

    #[test]
    fn starts_with_is_component_wise() {
        assert!(starts_with(
            &PathBuf::from("/a/b/c"),
            &PathBuf::from("/a/b")
        ));
        assert!(!starts_with(
            // `/a/bc` must NOT match base `/a/b` (no substring matching).
            &PathBuf::from("/a/bc"),
            &PathBuf::from("/a/b")
        ));
    }

    #[test]
    fn path_contains_segments_matches_consecutive() {
        let p = PathBuf::from("/x/scoop/apps/pakx/current/pakx");
        assert!(path_contains_segments(&p, &["scoop", "apps", "pakx"]));
        assert!(!path_contains_segments(
            &PathBuf::from("/x/scoop/buckets/pakx"),
            &["scoop", "apps", "pakx"]
        ));
    }

    // -- semver compare ----------------------------------------------------

    #[test]
    fn semver_compare_handles_basic_cases() {
        assert_eq!(compare_semver("0.1.0", "0.1.0"), Ordering::Equal);
        assert_eq!(compare_semver("0.1.0", "0.1.1"), Ordering::Less);
        assert_eq!(compare_semver("0.2.0", "0.1.99"), Ordering::Greater);
        assert_eq!(compare_semver("1.0.0", "0.99.99"), Ordering::Greater);
    }

    #[test]
    fn semver_strips_pre_release_and_build_metadata() {
        assert_eq!(compare_semver("0.1.0-rc.1", "0.1.0"), Ordering::Equal);
        assert_eq!(compare_semver("0.1.0+build.5", "0.1.0"), Ordering::Equal);
    }

    #[test]
    fn semver_missing_segments_treated_as_zero() {
        assert_eq!(compare_semver("0.1", "0.1.0"), Ordering::Equal);
        assert_eq!(compare_semver("1", "1.0.0"), Ordering::Equal);
    }
}
