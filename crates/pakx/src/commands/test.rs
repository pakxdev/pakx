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
    CacheDir, OfficialMcpSource, PakxSource, RegistryClient, RegistryError, SmitherySource,
    OFFICIAL_MCP_BASE_URL, PAKX_BASE_URL, SMITHERY_BASE_URL,
};
use reqwest::Client;
use tempfile::TempDir;

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

    /// Override the official MCP Registry base URL (testing). Must be
    /// `https://` or `http://localhost` / `http://127.0.0.1` — any other
    /// `http://` URL is rejected.
    #[arg(long, hide = true)]
    pub mcp_base_url: Option<String>,

    /// Override the Smithery registry base URL (testing). Same scheme
    /// restrictions as `--mcp-base-url`.
    #[arg(long, hide = true)]
    pub smithery_base_url: Option<String>,

    /// Override the pakx-registry base URL (testing). Same scheme
    /// restrictions as `--mcp-base-url`.
    #[arg(long, hide = true)]
    pub pakx_base_url: Option<String>,

    /// Skip Smithery resolution even if a base URL is configured.
    #[arg(long)]
    pub no_smithery: bool,

    /// Skip the pakx-registry source.
    #[arg(long)]
    pub no_pakx_registry: bool,
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

    let mut failures = 0usize;

    if args.offline {
        // Only read the lockfile when running offline. Online validation
        // must not abort on a malformed or absent lockfile — the registry
        // is the source of truth there.
        let lockfile_path = project_root.join(LOCKFILE_FILENAME);
        let lockfile = read_lockfile_from(&lockfile_path)
            .with_context(|| format!("read lockfile {}", lockfile_path.display()))?;
        check_offline(&manifest, lockfile.as_ref(), &mut failures);
    } else {
        let mcp_base_url = match args.mcp_base_url.as_deref() {
            Some(url) => {
                validate_base_url(url)?;
                url
            }
            None => OFFICIAL_MCP_BASE_URL,
        };
        // `_cache_dir` keeps the per-invocation cache directory alive for
        // the duration of the registry calls; it's deleted on drop.
        let (client, _cache_dir) = build_registry_client(
            mcp_base_url,
            args.smithery_base_url.as_deref(),
            args.pakx_base_url.as_deref(),
            args.no_smithery,
            args.no_pakx_registry,
        )?;
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
    let mut skipped = 0usize;
    for (kind, deps) in groups {
        let Some(deps) = deps else { continue };
        for dep in deps {
            // Per-kind git deps are not validated by any source today —
            // mirror the `mcp` rejection so behaviour is uniform across
            // dependency kinds (callers expect `pakx test` not to claim
            // anything about a git URL until a resolver exists for it).
            if let DepSpec::Git(_) = dep {
                println!(
                    "skip  {kind}/{id} (not yet validated: git deps unsupported in this version)",
                    kind = kind.as_str(),
                    id = dep_id(dep),
                );
            } else {
                println!(
                    "skip  {kind}/{id} (not yet validated: resolver not yet wired for this package type)",
                    kind = kind.as_str(),
                    id = dep_id(dep),
                );
            }
            skipped += 1;
        }
    }
    if skipped > 0 {
        eprintln!("note: skipped {skipped} entries (only mcp: validated in this version)");
    }
}

fn dep_id(dep: &DepSpec) -> String {
    match dep {
        DepSpec::String(s) => s.as_str().to_owned(),
        DepSpec::Registry(r) => r.name.clone(),
        DepSpec::Git(g) => g.git.clone(),
    }
}

fn build_registry_client(
    mcp_base_url: &str,
    smithery_base_url: Option<&str>,
    pakx_base_url: Option<&str>,
    no_smithery: bool,
    no_pakx_registry: bool,
) -> Result<(RegistryClient, TempDir)> {
    // Per-invocation cache dir — avoids cross-run / cross-process state.
    // Dropped (and deleted) when the caller drops the returned `TempDir`.
    let cache_dir = TempDir::new().context("create temp cache dir for pakx test")?;
    let cache_root = cache_dir.path();

    let mcp =
        OfficialMcpSource::with_parts(Client::new(), mcp_base_url, CacheDir::with_root(cache_root));
    let mut client = RegistryClient::new().with_source(Box::new(mcp));

    if !no_smithery {
        let url = match smithery_base_url {
            Some(u) => {
                validate_base_url(u)?;
                u
            }
            None => SMITHERY_BASE_URL,
        };
        let sm = SmitherySource::with_parts(Client::new(), url, CacheDir::with_root(cache_root));
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
        let pakx = PakxSource::with_parts(Client::new(), url, CacheDir::with_root(cache_root));
        client = client.with_source(Box::new(pakx));
    }

    Ok((client, cache_dir))
}

/// Allow `https://` everywhere; allow `http://` only when the host is
/// the loopback address (`localhost` / `127.0.0.1` / `[::1]`, optionally
/// with port). Reject anything else, **including any URL that carries a
/// userinfo component** — `http://localhost:8080@evil.com/` was passing
/// the old `starts_with` + `split('/')` parser because the substring
/// before the path looked loopback-like, even though `url::Url` parses
/// the actual host as `evil.com`. Use the real parser here.
fn validate_base_url(url_str: &str) -> Result<()> {
    let parsed = url::Url::parse(url_str).map_err(|e| {
        anyhow::anyhow!("refusing to use registry base URL {url_str:?}: not a valid URL ({e})")
    })?;

    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!(
            "refusing to use registry base URL {url_str:?}: URLs with embedded \
             credentials are rejected to prevent userinfo smuggling"
        );
    }

    let host = parsed.host_str().ok_or_else(|| {
        anyhow::anyhow!("refusing to use registry base URL {url_str:?}: no host component")
    })?;

    // `url::Url::host_str` returns IPv6 literals with surrounding
    // brackets (e.g. `[::1]`), domains and IPv4 literals without. Match
    // on both forms so `http://[::1]:8080/` is accepted.
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") => Ok(()),
        _ => anyhow::bail!(
            "refusing to use registry base URL {url_str:?}: only `https://` or \
             `http://localhost` / `http://127.0.0.1` / `http://[::1]` are allowed"
        ),
    }
}

#[cfg(test)]
mod validate_base_url_tests {
    use super::validate_base_url;

    #[test]
    fn accepts_https() {
        validate_base_url("https://example.com/").unwrap();
        validate_base_url("https://registry.modelcontextprotocol.io/").unwrap();
    }

    #[test]
    fn accepts_http_loopback() {
        validate_base_url("http://localhost:8080/").unwrap();
        validate_base_url("http://127.0.0.1:8080/").unwrap();
        validate_base_url("http://[::1]:8080/").unwrap();
    }

    #[test]
    fn rejects_userinfo_bypass() {
        // The old parser accepted this because the substring before `/`
        // looked loopback-like. `url::Url` parses the host correctly.
        let err = validate_base_url("http://localhost:8080@evil.com/").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("embedded credentials") || msg.contains("only `https://`"),
            "msg={msg}"
        );
    }

    #[test]
    fn rejects_plaintext_remote_host() {
        validate_base_url("http://evil.com/").unwrap_err();
    }

    #[test]
    fn rejects_empty_host_https() {
        validate_base_url("https:///").unwrap_err();
    }

    #[test]
    fn rejects_unknown_scheme() {
        validate_base_url("ftp://example.com/").unwrap_err();
        validate_base_url("file:///etc/passwd").unwrap_err();
    }

    #[test]
    fn rejects_userinfo_with_https() {
        // Even over https, embedded credentials get rejected — the test
        // harness has no use for them and they hide host smuggling.
        validate_base_url("https://user:pass@registry.example.com/").unwrap_err();
    }
}
