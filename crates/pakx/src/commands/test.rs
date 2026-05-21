//! `pakx test` — read-only manifest validation for CI / pre-commit use.
//!
//! Parses `agents.yml` and (unless `--offline`) resolves every MCP entry
//! against the configured registries. Does NOT write `agents.lock` and does
//! NOT touch the install dir. Prints a per-entry status line and exits
//! non-zero on the first failure.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use pakx_core::manifest::{DepSpec, PackageType};
use pakx_core::{read_lockfile_from, read_manifest_from, Lockfile, Manifest, RegistrySource};
use pakx_registry_client::{
    CacheDir, OfficialMcpSource, RegistryClient, RegistryError, OFFICIAL_MCP_BASE_URL,
};
use reqwest::Client;

const MANIFEST_FILENAME: &str = "agents.yml";
const LOCKFILE_FILENAME: &str = "agents.lock";

#[derive(Debug, Clone, Args)]
pub struct TestArgs {
    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Override the default `agents.yml` path. Relative paths resolve
    /// against `--directory` (or cwd).
    #[arg(long)]
    pub manifest: Option<PathBuf>,

    /// Skip registry resolution. Only parse the manifest and (if present)
    /// cross-check the lockfile.
    #[arg(long)]
    pub offline: bool,

    /// Override the official MCP Registry base URL (testing).
    #[arg(long, hide = true)]
    pub mcp_base_url: Option<String>,
}

pub async fn run(args: TestArgs) -> Result<()> {
    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };
    let manifest_path = resolve_manifest_path(&project_root, args.manifest.as_deref());

    let manifest = read_manifest_from(&manifest_path)
        .with_context(|| format!("read manifest at {}", manifest_path.display()))?;
    println!(
        "ok    manifest {} parsed (name={}, version={})",
        manifest_path.display(),
        manifest.name,
        manifest.version,
    );

    let lockfile_path = project_root.join(LOCKFILE_FILENAME);
    let lockfile = read_lockfile_from(&lockfile_path)
        .with_context(|| format!("read lockfile {}", lockfile_path.display()))?;

    let mut failures = 0usize;

    if args.offline {
        check_offline(&manifest, lockfile.as_ref(), &mut failures);
    } else {
        let base_url = args
            .mcp_base_url
            .as_deref()
            .unwrap_or(OFFICIAL_MCP_BASE_URL);
        let client = build_registry_client(base_url);
        check_online(&manifest, &client, &mut failures).await;
    }

    report_unhandled(&manifest);

    if failures == 0 {
        println!("\nall entries ok");
        Ok(())
    } else {
        anyhow::bail!("{failures} entry/entries failed validation")
    }
}

fn resolve_manifest_path(
    project_root: &std::path::Path,
    manifest: Option<&std::path::Path>,
) -> PathBuf {
    match manifest {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => project_root.join(p),
        None => project_root.join(MANIFEST_FILENAME),
    }
}

fn check_offline(manifest: &Manifest, lockfile: Option<&Lockfile>, failures: &mut usize) {
    let Some(deps) = &manifest.dependencies.mcp else {
        return;
    };
    for dep in deps {
        let id = dep_id(dep);
        match lockfile {
            Some(lock) if lock.entries.values().any(|e| e.name == id) => {
                println!("ok    mcp/{id}");
            }
            Some(_) => {
                println!("fail: mcp/{id} not in {LOCKFILE_FILENAME} (offline cannot resolve)");
                *failures += 1;
            }
            None => {
                println!("fail: mcp/{id} cannot validate offline — no {LOCKFILE_FILENAME} present");
                *failures += 1;
            }
        }
    }
}

async fn check_online(manifest: &Manifest, client: &RegistryClient, failures: &mut usize) {
    let Some(deps) = &manifest.dependencies.mcp else {
        return;
    };
    for dep in deps {
        let id = dep_id(dep);
        if let DepSpec::Git(_) = dep {
            println!("fail: mcp/{id} git deps not yet supported");
            *failures += 1;
            continue;
        }
        match client.fetch(RegistrySource::OfficialMcp, &id).await {
            Ok(pkg) => println!("ok    mcp/{id} -> {}@{}", pkg.id, pkg.version),
            Err(RegistryError::NotFound { .. }) => {
                println!("fail: mcp/{id} not found in official MCP registry");
                *failures += 1;
            }
            Err(e) => {
                println!("fail: mcp/{id} {e}");
                *failures += 1;
            }
        }
    }
}

fn report_unhandled(manifest: &Manifest) {
    let groups: [(PackageType, Option<&Vec<DepSpec>>); 5] = [
        (PackageType::Skills, manifest.dependencies.skills.as_ref()),
        (
            PackageType::Subagents,
            manifest.dependencies.subagents.as_ref(),
        ),
        (PackageType::Prompts, manifest.dependencies.prompts.as_ref()),
        (
            PackageType::Commands,
            manifest.dependencies.commands.as_ref(),
        ),
        (PackageType::Hooks, manifest.dependencies.hooks.as_ref()),
    ];
    for (kind, deps) in groups {
        let Some(deps) = deps else { continue };
        for dep in deps {
            println!(
                "skip  {kind}/{id} (resolver not yet wired for this package type)",
                kind = kind.as_str(),
                id = dep_id(dep),
            );
        }
    }
}

fn dep_id(dep: &DepSpec) -> String {
    match dep {
        DepSpec::String(s) => s.as_str().to_owned(),
        DepSpec::Registry(r) => r.name.clone(),
        DepSpec::Git(g) => g.git.clone(),
    }
}

fn build_registry_client(base_url: &str) -> RegistryClient {
    let cache_root = std::env::temp_dir().join("pakx-test-cache");
    let cache = CacheDir::with_root(&cache_root);
    let source = OfficialMcpSource::with_parts(Client::new(), base_url, cache);
    RegistryClient::new().with_source(Box::new(source))
}
