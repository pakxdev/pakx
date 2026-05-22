//! `pakx info <owner>/<name>` — print registry-side metadata + version
//! list for a published package without installing it. Read-only.

use anyhow::{anyhow, Result};
use clap::Args;
use comfy_table::{Cell, CellAlignment};
use pakx_core::Sponsor;
use reqwest::Client;
use serde::Deserialize;

use crate::ui;

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
    sponsors: Vec<Sponsor>,
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
    // Wrap the network call in `with_context` so a connection refused
    // / DNS failure shows the registry URL the user gave us, not the
    // full reqwest error chain (which leaks the userinfo-stripped URL
    // and a 3-level cause stack that confuses adopters).
    let response = Client::new()
        .get(&url)
        .header("user-agent", USER_AGENT)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| anyhow!("could not reach {}: {e}", args.registry))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!("{}/{} not found on {}", owner, name, args.registry));
    }
    let response = response
        .error_for_status()
        .map_err(|e| anyhow!("registry returned an error: {e}"))?;
    let detail: PackageDetail = response
        .json()
        .await
        .map_err(|e| anyhow!("registry response was not valid JSON: {e}"))?;

    if args.json {
        // `sponsors` is a **stable** field on the `--json` contract per
        // spec §2 — always emit the array (empty when none) so callers
        // can rely on `.sponsors | length` without null-checking.
        let raw = serde_json::to_string_pretty(&serde_json::json!({
            "id": detail.id,
            "kind": detail.kind,
            "description": detail.description,
            "createdAt": detail.created_at,
            "sponsors": detail.sponsors.iter().map(|s| serde_json::json!({
                "kind": s.kind.as_str(),
                "url": s.url,
            })).collect::<Vec<_>>(),
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

    println!("{}", ui::heading(&detail.id));
    if let Some(k) = &detail.kind {
        println!("  {} {}", ui::dim("kind:       "), k);
    }
    if let Some(d) = &detail.description {
        println!("  {} {}", ui::dim("description:"), d);
    }
    if let Some(c) = &detail.created_at {
        println!("  {} {}", ui::dim("created:    "), c);
    }
    println!("  {} {}", ui::dim("registry:   "), args.registry);
    // Sponsors render between the description block and the versions
    // table per spec §7 open-question #7. Heading-less when empty so a
    // sponsor-less package looks the same as before this feature.
    if !detail.sponsors.is_empty() {
        println!();
        println!("{}", ui::heading("sponsors:"));
        for s in &detail.sponsors {
            // Pad the kind to a fixed width so the URLs line up
            // visually across rows. Matches the dim/label cadence used
            // by the `kind:` / `description:` / `created:` lines above.
            println!(
                "  {} {}",
                ui::dim(&format!("{:<8}", s.kind.as_str())),
                s.url
            );
        }
    }
    println!();
    if detail.versions.is_empty() {
        println!("  {}", ui::dim("no versions published yet."));
    } else {
        println!("{}", ui::heading("versions:"));
        let mut table = ui::table();
        table.set_header(vec![
            Cell::new("version").set_alignment(CellAlignment::Right),
            Cell::new("size").set_alignment(CellAlignment::Right),
            Cell::new("published"),
            Cell::new("status"),
        ]);
        for v in &detail.versions {
            let size = v.size_bytes.map_or_else(|| "-".to_string(), human_bytes);
            let published = v.published_at.as_deref().unwrap_or("-");
            let status = if v.deprecated_at.is_some() {
                "deprecated"
            } else {
                "active"
            };
            table.add_row(vec![
                Cell::new(v.version.as_str()).set_alignment(CellAlignment::Right),
                Cell::new(size).set_alignment(CellAlignment::Right),
                Cell::new(published),
                Cell::new(status),
            ]);
        }
        println!("{table}");
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
