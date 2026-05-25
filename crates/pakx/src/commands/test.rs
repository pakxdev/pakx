//! `pakx test` — read-only manifest validation for CI / pre-commit use.
//!
//! Parses `agents.yml` and (unless `--offline`) resolves every MCP entry
//! against the configured registries. Does NOT write `agents.lock` and does
//! NOT touch the install dir. Prints a per-entry status line and exits
//! non-zero on the first failure.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use futures::stream::{self, StreamExt};
use pakx_core::manifest::{DepSpec, PackageType};
use pakx_core::{http_client, read_lockfile_from, read_manifest_from, Lockfile, Manifest};
use pakx_registry_client::{
    OfficialMcpSource, PakxSource, RegistryClient, SmitherySource, OFFICIAL_MCP_BASE_URL,
    PAKX_BASE_URL, SMITHERY_BASE_URL,
};
use tempfile::TempDir;

use crate::commands::cache_tempdir::{cache_dir_at, make_cache_tempdir};
use crate::redact::redact_path;
use crate::registry_url::validate_base_url;
use crate::resolve::{resolve_federated, Resolved};
use crate::ui;

const MANIFEST_FILENAME: &str = "agents.yml";
const LOCKFILE_FILENAME: &str = "agents.lock";

/// Concurrency cap on the federated resolver fan-out. 10 is the
/// sweet-spot from the 2026-05 perf pass: 3 deps × ~400ms RTT
/// sequential ≈ 1066ms p50, collapsing to ~400ms (max single dep) when
/// fanned out. Larger fan-outs hammer upstream registries
/// (`OfficialMcp` / Smithery / pakx-registry) without buying
/// additional wall-clock gain past ~10 concurrent calls — past that
/// the registry's own rate-limit / connection-pool ceiling becomes
/// the bottleneck. Same constant is intentionally not shared with the
/// install runner: that path also does per-dep adapter writes which
/// are FS-bound, not network-bound, so its concurrency model is
/// different (currently still sequential per-kind).
const TEST_RESOLVE_CONCURRENCY: usize = 10;

#[derive(Debug, Clone, Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flags are independent toggles; a state machine here would obscure the surface
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
    ///
    /// Mutually exclusive with `--no-smithery`: opting out of a source
    /// while supplying a base URL for it is a contradiction. Clap
    /// errors on the contradiction so the user sees the mistake
    /// instead of silently dropping the override. Mirrors the same
    /// guard on `pakx install`.
    #[arg(long, hide = true, conflicts_with = "no_smithery")]
    pub smithery_base_url: Option<String>,

    /// Override the pakx-registry base URL (testing). Same scheme
    /// restrictions as `--mcp-base-url`.
    ///
    /// Mutually exclusive with `--no-pakx-registry` for the same
    /// reason as `--smithery-base-url` / `--no-smithery`.
    #[arg(long, hide = true, conflicts_with = "no_pakx_registry")]
    pub pakx_base_url: Option<String>,

    /// Skip Smithery resolution even if a base URL is configured.
    #[arg(long)]
    pub no_smithery: bool,

    /// Skip the pakx-registry source.
    #[arg(long)]
    pub no_pakx_registry: bool,

    /// Bypass the federated-source cache for this invocation. Drops
    /// the per-call cache TTL to zero so a cached resolution is
    /// ignored and the upstream registry is re-queried. Useful in CI
    /// right after a publish, when the on-disk cache may still hold
    /// the pre-publish `not found` answer.
    #[arg(long)]
    pub no_cache: bool,
}

pub async fn run(args: TestArgs) -> Result<()> {
    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };
    let manifest_path = resolve_manifest_path(&project_root, args.manifest.as_deref());

    let manifest = read_manifest_from(&manifest_path).with_context(|| {
        format!(
            "read manifest at {}",
            redact_path(&manifest_path, &project_root)
        )
    })?;
    let manifest_label = display_manifest_path(&project_root, &manifest_path);
    println!(
        "{} manifest {} parsed (name={}, version={})",
        ui::glyph_ok(),
        manifest_label,
        manifest.name,
        manifest.version,
    );

    let mut failures = 0usize;

    if args.offline {
        // Only read the lockfile when running offline. Online validation
        // must not abort on a malformed or absent lockfile — the registry
        // is the source of truth there.
        let lockfile_path = project_root.join(LOCKFILE_FILENAME);
        let lockfile = read_lockfile_from(&lockfile_path).with_context(|| {
            format!(
                "read lockfile {}",
                redact_path(&lockfile_path, &project_root)
            )
        })?;
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
            args.no_cache,
        )?;
        check_online(&manifest, &client, &mut failures).await;
    }

    let skipped = report_unhandled(&manifest);

    if failures == 0 {
        // Honest footer. Only `mcp:` deps are actually resolved today;
        // skills / subagents / prompts / commands / hooks entries are
        // reported "not yet validated" per-row but were previously
        // followed by an unconditional "all entries ok / manifest
        // validated" — a false all-clear for a manifest made entirely
        // of installable skills. Qualify the footer so the exit-0 never
        // overclaims: it now states exactly how many entries of other
        // kinds were skipped rather than validated.
        if skipped == 0 {
            println!("\n{}", ui::heading("all entries ok"));
            println!("{}", ui::dim("\u{2192} manifest validated"));
        } else {
            println!("\n{}", ui::heading("mcp entries ok"));
            println!(
                "{}",
                ui::dim(&format!(
                    "\u{2192} only mcp: resolved; {skipped} entr{} of other kinds skipped (not validated)",
                    if skipped == 1 { "y" } else { "ies" },
                ))
            );
        }
        Ok(())
    } else {
        anyhow::bail!("{failures} entry/entries failed validation")
    }
}

/// Render `manifest_path` for human output. When the path lives under
/// `project_root` we strip the prefix so absolute temp paths (which leak
/// host info and make output noisy) don't show up in CI logs. When the
/// caller pointed `--manifest` at a path outside the project root we
/// keep the absolute form so the user can still trace which file was
/// parsed.
fn display_manifest_path(
    project_root: &std::path::Path,
    manifest_path: &std::path::Path,
) -> String {
    manifest_path.strip_prefix(project_root).map_or_else(
        |_| manifest_path.display().to_string(),
        |p| p.display().to_string(),
    )
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
                println!("{} mcp/{id}", ui::glyph_ok());
            }
            Some(_) => {
                println!(
                    "{} mcp/{id} not in {LOCKFILE_FILENAME} (offline cannot resolve)",
                    ui::glyph_fail()
                );
                *failures += 1;
            }
            None => {
                println!(
                    "{} mcp/{id} cannot validate offline — no {LOCKFILE_FILENAME} present",
                    ui::glyph_fail()
                );
                *failures += 1;
            }
        }
    }
}

/// Fan resolution out across every `mcp:` dep in parallel, capped at
/// [`TEST_RESOLVE_CONCURRENCY`]. Pre-fix this loop awaited each
/// `resolve_federated` call serially — 3 deps × ~400ms RTT = ~1066ms
/// p50 wall clock. With `buffer_unordered` the wall-clock collapses to
/// `max(per-dep RTT)` (~400ms on the same 3-dep manifest, ≈ 58% drop).
///
/// Output ordering matches the manifest deps order even though the
/// futures complete out-of-order: we tag each future with its dep
/// index, collect the (index, render) pairs, then sort by index before
/// printing. That keeps the user-visible row order stable across runs
/// (which a flaky-output CI parser would otherwise hate) without
/// sacrificing the parallel-network win.
///
/// Git deps short-circuit synchronously (no network) and are counted
/// straight into `failures` before the fan-out — they don't take a
/// slot in the concurrency budget.
async fn check_online(manifest: &Manifest, client: &RegistryClient, failures: &mut usize) {
    let Some(deps) = &manifest.dependencies.mcp else {
        return;
    };

    // Split sync-failed (git deps) from network-resolved entries up
    // front so the fan-out only carries work that actually awaits.
    let mut rendered: Vec<(usize, String, bool)> = Vec::with_capacity(deps.len());
    let mut to_resolve: Vec<(usize, String)> = Vec::with_capacity(deps.len());
    for (idx, dep) in deps.iter().enumerate() {
        let id = dep_id(dep);
        if let DepSpec::Git(_) = dep {
            rendered.push((
                idx,
                format!("{} mcp/{id} git deps not yet supported", ui::glyph_fail()),
                true,
            ));
        } else {
            to_resolve.push((idx, id));
        }
    }

    // Bounded-concurrency fan-out. `buffer_unordered` keeps up to
    // `TEST_RESOLVE_CONCURRENCY` resolutions in flight; results
    // surface as they complete (we sort by `idx` after the collect to
    // restore manifest order).
    let resolved: Vec<(usize, String, bool)> = stream::iter(to_resolve)
        .map(|(idx, id)| async move {
            // Federated resolution: OfficialMcp.fetch first, then
            // search every other registered source for an exact-name
            // match. README + CHANGELOG sell this as a federated
            // check; without the search fallback the `--no-smithery` /
            // `--no-pakx-registry` toggles are dead flags.
            let outcome = resolve_federated(client, &id).await;
            let (line, failed) = match outcome {
                Ok(Resolved::OfficialMcp(pkg) | Resolved::Federated(pkg)) => (
                    format!(
                        "{} mcp/{id} -> {source}:{pid}@{ver}",
                        ui::glyph_ok(),
                        source = pkg.source.as_tag(),
                        pid = pkg.id,
                        ver = pkg.version,
                    ),
                    false,
                ),
                Ok(Resolved::NotFound) => (
                    format!(
                        "{} mcp/{id} not found in any federated registry",
                        ui::glyph_fail()
                    ),
                    true,
                ),
                Err(e) => (format!("{} mcp/{id} {e}", ui::glyph_fail()), true),
            };
            (idx, line, failed)
        })
        .buffer_unordered(TEST_RESOLVE_CONCURRENCY)
        .collect()
        .await;

    rendered.extend(resolved);
    rendered.sort_by_key(|(idx, _, _)| *idx);
    for (_, line, failed) in rendered {
        println!("{line}");
        if failed {
            *failures += 1;
        }
    }
}

/// Print a per-entry "not yet validated" row for every dep of a kind
/// the online/offline resolver doesn't cover yet (everything but
/// `mcp:`), and return how many such entries were skipped so the caller
/// can qualify the success footer instead of claiming a full all-clear.
fn report_unhandled(manifest: &Manifest) -> usize {
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
                    "{} {kind}/{id} (not yet validated: git deps unsupported in this version)",
                    ui::glyph_info(),
                    kind = kind.as_str(),
                    id = dep_id(dep),
                );
            } else {
                println!(
                    "{} {kind}/{id} (not yet validated: resolver not yet wired for this package type)",
                    ui::glyph_info(),
                    kind = kind.as_str(),
                    id = dep_id(dep),
                );
            }
            skipped += 1;
        }
    }
    skipped
}

fn dep_id(dep: &DepSpec) -> String {
    match dep {
        DepSpec::String(s) => s.as_str().to_owned(),
        DepSpec::Registry(r) => r.name.clone(),
        DepSpec::Git(g) => g.git.clone(),
    }
}

#[allow(clippy::fn_params_excessive_bools)] // each bool maps 1:1 to a documented CLI flag; folding into an enum would obscure the surface
fn build_registry_client(
    mcp_base_url: &str,
    smithery_base_url: Option<&str>,
    pakx_base_url: Option<&str>,
    no_smithery: bool,
    no_pakx_registry: bool,
    no_cache: bool,
) -> Result<(RegistryClient, TempDir)> {
    // Per-invocation cache dir — avoids cross-run / cross-process state.
    // Dropped (and deleted) when the caller drops the returned `TempDir`.
    //
    // Uses [`make_cache_tempdir`] (pid + nanos prefix) rather than a
    // bare `TempDir::new()` so parallel integration tests can't share
    // cache entries when their wiremock mock servers happen to land
    // on the same loopback port — same regression class round 30
    // fixed in `outdated::build_clients` for `pakx outdated` /
    // `pakx add` / `pakx search`. The pakx-test surface had been
    // missed by that pass; this aligns it with the rest.
    let cache_dir =
        make_cache_tempdir("pakx-test-cache").context("create temp cache dir for pakx test")?;
    let cache_root = cache_dir.path();

    let mcp = OfficialMcpSource::with_parts(
        http_client(),
        mcp_base_url,
        cache_dir_at(cache_root, no_cache),
    );
    let mut client = RegistryClient::new().with_source(Box::new(mcp));

    if !no_smithery {
        let url = match smithery_base_url {
            Some(u) => {
                validate_base_url(u)?;
                u
            }
            None => SMITHERY_BASE_URL,
        };
        let sm = SmitherySource::with_parts(http_client(), url, cache_dir_at(cache_root, no_cache));
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
        let pakx = PakxSource::with_parts(http_client(), url, cache_dir_at(cache_root, no_cache));
        client = client.with_source(Box::new(pakx));
    }

    Ok((client, cache_dir))
}
