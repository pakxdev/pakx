//! `pakx install` core loop.
//!
//! Flow:
//!   1. Read `<project_root>/agents.yml`.
//!   2. For each dep, resolve canonical id via federated registry client.
//!   3. Translate registry hints into the installable payload.
//!   4. Dispatch to detected adapters via `Adapter::install_*`.
//!   5. Aggregate results, write `<project_root>/agents.lock`.
//!
//! Errors are collected per-dep so partial installs still produce a lockfile
//! and a clear summary at the end.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pakx_agents::{Adapter, AdapterError, ClaudeCodeAdapter};
use pakx_core::manifest::{DepSpec, PackageType};
use pakx_core::{
    compute_integrity, read_manifest_from, write_lockfile, AgentId, Integrity, LockEntry, Lockfile,
    Manifest, McpServer, RegistrySource, SkillFile, LOCKFILE_VERSION,
};
use pakx_registry_client::{
    CacheDir, OfficialMcpSource, PakxSource, RegistryClient, SmitherySource, OFFICIAL_MCP_BASE_URL,
    PAKX_BASE_URL, SMITHERY_BASE_URL,
};
use reqwest::Client;
use tracing::{debug, warn};

use super::mcp_translate::{translate, TranslateError};
use super::skill::{install_skill_from_pakx, parse_skill_shorthand, ResolvedSkill};
use crate::redact::redact_path;
use crate::registry_url::validate_base_url;
use crate::resolve::{resolve_federated, Resolved};

const MANIFEST_FILENAME: &str = "agents.yml";
const LOCKFILE_FILENAME: &str = "agents.lock";

#[derive(Debug, Default, Clone)]
pub struct InstallOpts {
    /// Override project root (defaults to cwd).
    pub project_root: Option<PathBuf>,
    /// Override MCP registry base URL (testing).
    pub mcp_base_url: Option<String>,
    /// Override Smithery registry base URL (testing).
    pub smithery_base_url: Option<String>,
    /// Override pakx-registry base URL (testing).
    pub pakx_base_url: Option<String>,
    /// Skip Smithery resolution.
    pub no_smithery: bool,
    /// Skip pakx-registry resolution.
    pub no_pakx_registry: bool,
    /// Override Claude Code home dir (testing).
    pub claude_home: Option<PathBuf>,
    /// Skip writing the lockfile (dry-run-ish).
    pub no_lockfile: bool,
}

#[derive(Debug, Default)]
pub struct InstallReport {
    pub installed: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub lockfile_path: Option<PathBuf>,
}

pub async fn run(opts: InstallOpts) -> Result<InstallReport> {
    let project_root = match opts.project_root.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };
    let manifest_path = project_root.join(MANIFEST_FILENAME);
    let manifest = read_manifest_from(&manifest_path).with_context(|| {
        format!(
            "read manifest at {}",
            redact_path(&manifest_path, &project_root)
        )
    })?;

    let client = build_registry_client(
        opts.mcp_base_url.as_deref(),
        opts.smithery_base_url.as_deref(),
        opts.pakx_base_url.as_deref(),
        opts.no_smithery,
        opts.no_pakx_registry,
    )?;

    // Standalone PakxSource for skill installs. Skills resolve directly
    // through pakx-registry (not the federated MCP fallback dance) so
    // we hold a dedicated handle even when the federated client also
    // owns one. Cheap — both share the inner reqwest connection pool
    // via clone().
    let pakx_source_with_url = if opts.no_pakx_registry {
        None
    } else {
        let url = opts
            .pakx_base_url
            .clone()
            .unwrap_or_else(|| PAKX_BASE_URL.to_owned());
        let cache_root = std::env::temp_dir().join("pakx-install-cache");
        let src = PakxSource::with_parts(Client::new(), &url, CacheDir::with_root(&cache_root));
        Some((src, url))
    };

    let claude = build_claude_adapter(&opts, &project_root);

    let mut report = InstallReport::default();
    let mut entries: BTreeMap<String, LockEntry> = BTreeMap::new();

    if let Some(deps) = &manifest.dependencies.mcp {
        for dep in deps {
            install_mcp_dep(dep, &client, &claude, &mut report, &mut entries).await;
        }
    }

    // Skills — wired through pakx-registry. Each dep gets fetched,
    // sha256-verified, and extracted into the Claude Code skills tree.
    // If `--no-pakx-registry` was passed, we can't resolve at all,
    // so every skill dep is reported as a hard failure (matching the
    // MCP path: opting out of the only source that knows the dep is
    // a contradiction).
    if let Some(deps) = &manifest.dependencies.skills {
        if let Some((source, base_url)) = pakx_source_with_url.as_ref() {
            let http = Client::new();
            for dep in deps {
                install_skill_dep(
                    dep,
                    source,
                    &http,
                    base_url,
                    &claude,
                    &mut report,
                    &mut entries,
                )
                .await;
            }
        } else {
            for dep in deps {
                let label = format!("skills/{}", dep.display_hint());
                report.failed.push((
                    label,
                    "skill installs require pakx-registry; --no-pakx-registry refused".into(),
                ));
            }
        }
    }

    // Subagents / prompts / commands / hooks — not yet wired. Surface
    // as "skipped" so users see they were noticed.
    for (kind, deps) in unhandled_deps(&manifest) {
        if let Some(list) = deps {
            for dep in list {
                let label = format!("{}/{}", kind.as_str(), dep.display_hint());
                report.skipped.push(label);
            }
        }
    }

    if !opts.no_lockfile {
        let lockfile_path = project_root.join(LOCKFILE_FILENAME);
        let lock = Lockfile {
            lockfile_version: LOCKFILE_VERSION,
            manifest_hash: hash_manifest(&manifest),
            entries,
        };
        let body = write_lockfile(&lock);
        std::fs::write(&lockfile_path, body).with_context(|| {
            format!(
                "write lockfile {}",
                redact_path(&lockfile_path, &project_root)
            )
        })?;
        report.lockfile_path = Some(lockfile_path);
    }

    Ok(report)
}

const fn unhandled_deps(manifest: &Manifest) -> [(PackageType, &Option<Vec<DepSpec>>); 4] {
    [
        (PackageType::Subagents, &manifest.dependencies.subagents),
        (PackageType::Prompts, &manifest.dependencies.prompts),
        (PackageType::Commands, &manifest.dependencies.commands),
        (PackageType::Hooks, &manifest.dependencies.hooks),
    ]
}

fn build_registry_client(
    mcp_base_url: Option<&str>,
    smithery_base_url: Option<&str>,
    pakx_base_url: Option<&str>,
    no_smithery: bool,
    no_pakx_registry: bool,
) -> Result<RegistryClient> {
    // Validate every user-supplied override BEFORE any HTTP work.
    // `pakx test` already did this; install must mirror it or the same
    // `http://localhost:8080@evil.com/` userinfo-smuggle bypass that
    // PR #29 closed for `test` is still reachable on `install`. Single
    // source of truth: `crate::registry_url::validate_base_url`.
    let mcp_url = match mcp_base_url {
        Some(u) => {
            validate_base_url(u)?;
            u
        }
        None => OFFICIAL_MCP_BASE_URL,
    };

    let cache_root = std::env::temp_dir().join("pakx-install-cache");
    let mcp =
        OfficialMcpSource::with_parts(Client::new(), mcp_url, CacheDir::with_root(&cache_root));
    let mut client = RegistryClient::new().with_source(Box::new(mcp));

    if !no_smithery {
        let url = match smithery_base_url {
            Some(u) => {
                validate_base_url(u)?;
                u
            }
            None => SMITHERY_BASE_URL,
        };
        let sm = SmitherySource::with_parts(Client::new(), url, CacheDir::with_root(&cache_root));
        client = client.with_source(Box::new(sm));
    }

    if !no_pakx_registry {
        let url = match pakx_base_url {
            Some(u) => {
                validate_base_url(u)?;
                u
            }
            None => PAKX_BASE_URL,
        };
        let pakx = PakxSource::with_parts(Client::new(), url, CacheDir::with_root(&cache_root));
        client = client.with_source(Box::new(pakx));
    }

    Ok(client)
}

fn build_claude_adapter(opts: &InstallOpts, project_root: &Path) -> ClaudeCodeAdapter {
    let home = opts
        .claude_home
        .clone()
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    let adapter = home.map_or_else(
        || ClaudeCodeAdapter::with_config_dir(project_root.join(".claude")),
        ClaudeCodeAdapter::with_config_dir,
    );
    adapter.with_project_root(project_root)
}

async fn install_mcp_dep(
    dep: &DepSpec,
    client: &RegistryClient,
    claude: &ClaudeCodeAdapter,
    report: &mut InstallReport,
    entries: &mut BTreeMap<String, LockEntry>,
) {
    let id = match dep {
        DepSpec::String(s) => s.as_str().to_owned(),
        DepSpec::Registry(r) => r.name.clone(),
        DepSpec::Git(g) => {
            report
                .failed
                .push((g.git.clone(), "git deps not implemented for MCP yet".into()));
            return;
        }
    };
    debug!(target: "pakx::install", %id, "resolving mcp dep");

    let (pkg, source) = match resolve_federated(client, &id).await {
        Ok(Resolved::OfficialMcp(p)) => (p, RegistrySource::OfficialMcp),
        Ok(Resolved::Federated(p)) => {
            let src = p.source;
            debug!(target: "pakx::install", %id, source = ?src, "resolved via federated search");
            (p, src)
        }
        Ok(Resolved::NotFound) => {
            warn!(target: "pakx::install", %id, "not in any federated registry");
            report
                .failed
                .push((id.clone(), "not found in any federated registry".into()));
            return;
        }
        Err(e) => {
            report.failed.push((id.clone(), e.to_string()));
            return;
        }
    };

    let transport = match translate(&pkg) {
        Ok(t) => t,
        Err(TranslateError::NoTransport { .. }) => {
            report
                .failed
                .push((id.clone(), "no installable transport advertised".into()));
            return;
        }
        Err(e) => {
            report.failed.push((id.clone(), e.to_string()));
            return;
        }
    };

    let mcp = McpServer {
        id: pkg.id.clone(),
        version: pkg.version.clone(),
        transport,
    };
    let integrity = mcp.computed_integrity();

    match claude.install_mcp(&mcp).await {
        Ok(_) => {
            report.installed.push(mcp.id.clone());
            entries.insert(mcp.lockfile_key(), lock_entry_for(&mcp, integrity, source));
        }
        Err(AdapterError::AlreadyInstalled { id }) => {
            report.skipped.push(id);
            entries.insert(mcp.lockfile_key(), lock_entry_for(&mcp, integrity, source));
        }
        Err(e) => {
            report.failed.push((mcp.id, e.to_string()));
        }
    }
}

/// Build a lockfile entry, recording **which federated source** the
/// dep was resolved through. Storing the source-of-truth per id in the
/// lockfile lets `pakx doctor` reason about drift without re-running
/// the federated search every time.
fn lock_entry_for(mcp: &McpServer, integrity: Integrity, source: RegistrySource) -> LockEntry {
    LockEntry {
        name: mcp.id.clone(),
        kind: PackageType::Mcp,
        version: mcp.version.clone(),
        resolved_from: format!("{}:{}", source.as_tag(), mcp.id),
        registry: source,
        integrity,
        agents: vec![AgentId::new_unchecked(ClaudeCodeAdapter::ID)],
        dependencies: vec![],
    }
}

/// Install one `skills:` dep: parse the shorthand, fetch metadata
/// through `PakxSource`, verify, extract. Records `Adapter::install_*`
/// results into `report` and writes a lockfile entry on success.
///
/// Other adapters (cursor/codex/copilot/windsurf) don't yet implement
/// skill extraction; for them we add a `skipped: <adapter> does not
/// yet implement skills extraction` entry rather than failing the
/// whole install. The Claude Code path runs whenever a Claude home
/// is configured (override or default), which it always is in the
/// runner's `build_claude_adapter` path.
async fn install_skill_dep(
    dep: &DepSpec,
    source: &PakxSource,
    http: &reqwest::Client,
    base_url: &str,
    claude: &ClaudeCodeAdapter,
    report: &mut InstallReport,
    entries: &mut BTreeMap<String, LockEntry>,
) {
    // Only `String(...)` shorthand is wired at v0.1 — git + registry
    // object specs need their own resolution paths (Phase C+).
    let shorthand = match dep {
        DepSpec::String(s) => s.as_str().to_owned(),
        DepSpec::Git(g) => {
            report.failed.push((
                g.git.clone(),
                "git deps not implemented for skills yet".into(),
            ));
            return;
        }
        DepSpec::Registry(r) => {
            report.failed.push((
                format!("{}/{}", r.registry, r.name),
                "registry-object spec not implemented for skills yet".into(),
            ));
            return;
        }
    };

    let (_, _, requested_version) = match parse_skill_shorthand(&shorthand) {
        Ok(t) => t,
        Err(e) => {
            report.failed.push((shorthand, e.to_string()));
            return;
        }
    };

    debug!(target: "pakx::install", id = %shorthand, "resolving skill dep");

    // Install path: Claude Code only at v0.1. Other adapters will
    // grow their own extract logic as their adapters land.
    let claude_home = claude.config_dir();
    let resolved = match install_skill_from_pakx(
        source,
        http,
        base_url,
        claude_home,
        &shorthand,
        requested_version.as_deref(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            report.failed.push((shorthand, format!("{e:#}")));
            return;
        }
    };

    let lockfile_key = format!(
        "{}/{}@{}",
        PackageType::Skills.as_str(),
        resolved.id,
        resolved.version
    );
    entries.insert(lockfile_key, lock_entry_for_skill(&resolved));
    report.installed.push(resolved.id);
}

/// Build a lockfile entry for a skill resolved through pakx-registry.
/// Mirrors `lock_entry_for` (MCP) but pins the canonical
/// pakx-registry URL — never the signed `tarballUrl`, which is
/// ephemeral.
fn lock_entry_for_skill(resolved: &ResolvedSkill) -> LockEntry {
    LockEntry {
        name: resolved.id.clone(),
        kind: PackageType::Skills,
        version: resolved.version.clone(),
        resolved_from: resolved.canonical_url.clone(),
        registry: RegistrySource::Pakx,
        integrity: resolved.integrity.clone(),
        agents: vec![AgentId::new_unchecked(ClaudeCodeAdapter::ID)],
        dependencies: vec![],
    }
}

/// Sha256 over the serialized manifest body. Stored in the lockfile so
/// `pakx doctor` can detect drift.
fn hash_manifest(manifest: &Manifest) -> Integrity {
    let body = pakx_core::manifest::write_manifest(manifest);
    let file = SkillFile {
        relative_path: MANIFEST_FILENAME.to_owned(),
        contents: body.into_bytes(),
    };
    compute_integrity(&[file])
}
