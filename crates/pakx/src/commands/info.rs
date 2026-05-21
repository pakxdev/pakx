//! `pakx info <owner>/<name>` — print registry-side metadata + version
//! list for a published package without installing it. Read-only.

use anyhow::{anyhow, Result};
use clap::Args;
use reqwest::Client;
use serde::Deserialize;

const DEFAULT_REGISTRY: &str = "https://registry.pakx.dev";
const USER_AGENT: &str = concat!("pakx/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Args)]
pub struct InfoArgs {
    /// Canonical `<owner>/<name>` of the package.
    pub id: String,

    /// Override the pakx-registry base URL.
    #[arg(long, default_value = DEFAULT_REGISTRY)]
    pub registry: String,

    /// Print JSON instead of the human-friendly table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Deserialize)]
struct PackageDetail {
    id: String,
    kind: Option<String>,
    description: Option<String>,
    created_at: Option<String>,
    #[serde(default)]
    versions: Vec<VersionEntry>,
}

#[derive(Debug, Deserialize)]
struct VersionEntry {
    version: String,
    sha256: Option<String>,
    size_bytes: Option<u64>,
    published_at: Option<String>,
    #[serde(default)]
    deprecated_at: Option<String>,
}

pub async fn run(args: InfoArgs) -> Result<()> {
    let (owner, name) = split_id(&args.id)?;
    let url = format!(
        "{}/api/v1/packages/{}/{}",
        args.registry.trim_end_matches('/'),
        owner,
        name,
    );
    let response = Client::new()
        .get(&url)
        .header("user-agent", USER_AGENT)
        .header("accept", "application/json")
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!("{}/{} not found on {}", owner, name, args.registry));
    }
    let response = response.error_for_status()?;
    let detail: PackageDetail = response.json().await?;

    if args.json {
        let raw = serde_json::to_string_pretty(&serde_json::json!({
            "id": detail.id,
            "kind": detail.kind,
            "description": detail.description,
            "createdAt": detail.created_at,
            "versions": detail.versions.iter().map(|v| serde_json::json!({
                "version": v.version,
                "sha256": v.sha256,
                "sizeBytes": v.size_bytes,
                "publishedAt": v.published_at,
                "deprecatedAt": v.deprecated_at,
            })).collect::<Vec<_>>(),
        }))?;
        println!("{raw}");
        return Ok(());
    }

    println!("{}", detail.id);
    if let Some(k) = &detail.kind {
        println!("  kind:        {k}");
    }
    if let Some(d) = &detail.description {
        println!("  description: {d}");
    }
    if let Some(c) = &detail.created_at {
        println!("  created:     {c}");
    }
    println!("  registry:    {}", args.registry);
    println!();
    if detail.versions.is_empty() {
        println!("  no versions published yet.");
    } else {
        println!(
            "  {:<24} {:<12} {:<14} status",
            "version", "size", "published",
        );
        for v in &detail.versions {
            let size = v.size_bytes.map_or_else(|| "-".to_string(), human_bytes);
            let published = v.published_at.as_deref().unwrap_or("-");
            let status = if v.deprecated_at.is_some() {
                "deprecated"
            } else {
                "active"
            };
            println!(
                "  {:<24} {:<12} {:<14} {}",
                v.version, size, published, status
            );
        }
    }
    Ok(())
}

fn split_id(id: &str) -> Result<(String, String)> {
    let Some((owner, name)) = id.split_once('/') else {
        return Err(anyhow!(
            "expected `<owner>/<name>` (e.g. acme/cool-skill), got {id:?}"
        ));
    };
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(anyhow!(
            "expected `<owner>/<name>` (e.g. acme/cool-skill), got {id:?}"
        ));
    }
    Ok((owner.to_string(), name.to_string()))
}

#[allow(clippy::cast_precision_loss)]
fn human_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if n >= MB {
        format!("{:.1} MiB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KiB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_id_accepts_canonical_form() {
        assert_eq!(
            split_id("acme/cool").unwrap(),
            ("acme".to_owned(), "cool".to_owned()),
        );
    }

    #[test]
    fn split_id_rejects_missing_slash() {
        assert!(split_id("no-slash").is_err());
    }

    #[test]
    fn split_id_rejects_empty_halves() {
        assert!(split_id("/right").is_err());
        assert!(split_id("left/").is_err());
    }

    #[test]
    fn split_id_rejects_too_many_slashes() {
        assert!(split_id("a/b/c").is_err());
    }

    #[test]
    fn human_bytes_picks_right_unit() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
