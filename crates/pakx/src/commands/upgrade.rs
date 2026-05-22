//! `pakx upgrade` — check GitHub Releases for a newer version.
//!
//! Read-only by design. We do not auto-download or rewrite the
//! currently-installed binary; that path varies per channel (cargo,
//! brew, scoop, install.sh) and trying to be clever leads to ruined
//! installs. Instead, print the channel-appropriate command the user
//! should run.

use anyhow::{anyhow, Result};
use clap::Args;
use reqwest::Client;
use serde::Deserialize;

use crate::ui;

const CURRENT: &str = env!("CARGO_PKG_VERSION");
const LATEST_URL: &str = "https://api.github.com/repos/pakxdev/pakx/releases/latest";
const USER_AGENT: &str = concat!("pakx/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Args)]
pub struct UpgradeArgs {
    /// Override the GitHub Releases API URL (testing only).
    #[arg(long, hide = true)]
    pub releases_url: Option<String>,

    /// Override the user-agent header sent to GitHub (testing only).
    #[arg(long, hide = true)]
    pub user_agent: Option<String>,
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

    let release: Release = Client::new()
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
            println!(
                "A newer pakx is available: {} -> {}",
                ui::dim(CURRENT),
                ui::success(latest),
            );
            println!("{} {}", ui::heading("release notes:"), release.html_url);
            println!();
            println!("{}", ui::heading("upgrade via your install channel:"));
            println!("  curl|sh / irm|iex   curl -fsSL https://pakx.dev/install.sh | sh");
            println!("                      irm https://pakx.dev/install.ps1 | iex");
            println!("  brew                brew upgrade pakx");
            println!("  scoop               scoop update pakx");
            println!(
                "  cargo               cargo install --git https://github.com/pakxdev/pakx --tag v{latest} --locked pakx"
            );
        }
    }
    Ok(())
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
