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
    compute_integrity, http_client, read_manifest_from, write_lockfile_to, AgentId, Integrity,
    LockEntry, Lockfile, Manifest, McpServer, RegistrySource, SkillFile, LOCKFILE_VERSION,
};
use pakx_registry_client::{
    OfficialMcpSource, PakxSource, RegistryClient, SmitherySource, OFFICIAL_MCP_BASE_URL,
    PAKX_BASE_URL, SMITHERY_BASE_URL,
};
use tracing::{debug, warn};

use super::bundle::{install_bundle_from_pakx, ResolvedBundle};
use super::mcp_translate::{translate, TranslateError};
use super::progress::{NoopSink, Phase, ProgressSink};
use super::rollback::Snapshot;
use super::skill::{install_skill_from_pakx, parse_skill_shorthand, ResolvedSkill};
use crate::commands::cache_tempdir::cache_dir_at;
use crate::redact::redact_path;
use crate::registry_url::validate_base_url;
use crate::resolve::{resolve_federated, Resolved};

const MANIFEST_FILENAME: &str = "agents.yml";
const LOCKFILE_FILENAME: &str = "agents.lock";

/// Kinds whose install adapter is fully wired through the runner.
///
/// Used by `pakx tree` + `pakx why` to tag each lockfile entry as
/// `wired` vs `skipped` without duplicating the dispatch list across
/// modules. **Single source of truth** — the runner's `match` below
/// MUST cover every kind in this constant (and only those). A clippy
/// lint can't enforce the round-trip, so the runner's match arm
/// reaches `unreachable!()` for anything outside this set and the
/// associated unit test (`adapter_wired_kinds_matches_dispatch`)
/// asserts the membership.
pub const ADAPTER_WIRED_KINDS: &[PackageType] = &[
    PackageType::Skills,
    PackageType::Mcp,
    PackageType::Subagents,
    PackageType::Prompts,
    PackageType::Commands,
    PackageType::Hooks,
];

#[derive(Debug, Default, Clone)]
#[allow(clippy::struct_excessive_bools)] // each field maps 1:1 to a documented CLI flag; folding into an enum would obscure the surface
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
    /// Bypass the federated-source cache for this invocation. When
    /// `true`, the per-call `CacheDir` is built with a 0-second TTL so
    /// any prior cached metadata is ignored and the registry is
    /// re-queried. Mirrors `pakx search --no-cache` etc. — the
    /// installer's cache reads happen via `RegistryClient` /
    /// `PakxSource::fetch`, both of which honour the `CacheDir` TTL.
    pub no_cache: bool,
    /// Restore the local filesystem to its pre-run state when any dep
    /// fails. When `true`, the runner snapshots every target dir the run
    /// will touch *before* the first adapter write, then — if the run
    /// ends with at least one failure — restores each target (deleting
    /// dirs the run created, moving prior contents back for dirs that
    /// pre-existed). Default `false`: today's partial-install behaviour
    /// is preserved unless the caller opts in. Default-on is reserved
    /// for a future major bump.
    pub rollback_on_error: bool,
}

#[derive(Debug, Default)]
pub struct InstallReport {
    pub installed: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub lockfile_path: Option<PathBuf>,
    /// Set `true` the first time an `mcp:` dep's adapter actually WRITES
    /// `.mcp.json` (a fresh insert or a changed entry). Left `false` when
    /// every mcp dep was skip / already-installed (the adapter returns
    /// `AlreadyInstalled` *before* touching the file) or failed before
    /// the write. Consumed by the rollback gate so a run that merely
    /// *declared* an mcp dep without writing the merge file never reverts
    /// `.mcp.json` and clobbers servers the user already had.
    pub mcp_json_written: bool,
    /// Per-entry structured record. Populated alongside the legacy
    /// `installed`/`skipped`/`failed` vecs so existing consumers
    /// (`pakx install`'s human render, `pakx update`'s post-update
    /// log) keep working unchanged while `pakx install --json` can
    /// emit kind + version metadata per entry. Order is preserved as
    /// the dispatch order (`mcp` → `skills` → `subagents` → `prompts`
    /// → `commands` → `hooks`).
    pub entries: Vec<InstallReportEntry>,
}

/// One row in [`InstallReport::entries`]. `status` is one of `ok`,
/// `skipped`, or `failed` — the same trichotomy the human render
/// surfaces — and `error` is populated only on the failed path.
/// `kind` carries the [`PackageType`] discriminator so a JSON consumer
/// can pivot per-kind without re-grepping the id.
#[derive(Debug, Clone)]
pub struct InstallReportEntry {
    pub id: String,
    pub status: InstallStatus,
    pub kind: PackageType,
    pub version: Option<String>,
    pub error: Option<String>,
}

/// Per-entry result discriminator. Three states match the v0
/// human surface: `Ok` (newly installed), `Skipped` (idempotent
/// re-install, no work done), `Failed` (error captured in `error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStatus {
    Ok,
    Skipped,
    Failed,
}

impl InstallStatus {
    /// Stable wire tag used by `pakx install --json`. Only additive
    /// changes (new variants) are backwards-compatible.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// Run an install with no progress reporting. Thin wrapper over
/// [`run_with_progress`] using the [`NoopSink`]. This is the stable
/// entry point for callers that don't render per-dep progress —
/// `pakx update`'s in-process reconcile and every test — so threading
/// the sink stays opt-in and the behaviour is byte-for-byte identical.
///
/// # Errors
///
/// See [`run_with_progress`].
pub async fn run(opts: InstallOpts) -> Result<InstallReport> {
    run_with_progress(opts, &NoopSink).await
}

/// Run an install, reporting per-dependency lifecycle events to `sink`.
///
/// The sink is **presentation-only**: it observes each dep's
/// begin → resolve → install → finish transitions but cannot influence
/// control flow, the install outcome, the lockfile write, or the
/// `tracing` trail. Handing in a [`NoopSink`] is exactly equivalent to
/// the historical no-progress behaviour.
///
/// # Errors
///
/// Surfaces manifest-read, registry-client-build, snapshot-capture,
/// rollback-restore, and lockfile-write failures. Per-dep install
/// errors are collected into [`InstallReport::failed`] rather than
/// short-circuiting, so a partial run still returns `Ok(report)`.
pub async fn run_with_progress(
    opts: InstallOpts,
    sink: &dyn ProgressSink,
) -> Result<InstallReport> {
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
        opts.no_cache,
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
        let src = PakxSource::with_parts(
            http_client(),
            &url,
            cache_dir_at(&cache_root, opts.no_cache),
        );
        Some((src, url))
    };

    let claude = build_claude_adapter(&opts, &project_root);

    // Rollback snapshot: when `--rollback-on-error` is set, record the
    // prior on-disk state of every target this run will touch BEFORE the
    // first adapter write. On a failed run we restore from this snapshot
    // so the filesystem looks as if the run never happened; on a clean
    // run we commit (discard the snapshot). Captured here — after the
    // adapter is built so we know the resolved Claude home, but before
    // any install loop mutates disk.
    let snapshot = maybe_capture_snapshot(&opts, &manifest, &claude)?;

    let mut report = InstallReport::default();
    let mut entries: BTreeMap<String, LockEntry> = BTreeMap::new();

    if let Some(deps) = &manifest.dependencies.mcp {
        for dep in deps {
            install_mcp_dep(dep, &client, &claude, &mut report, &mut entries, sink).await;
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
            let http = http_client();
            for dep in deps {
                install_skill_dep(
                    dep,
                    source,
                    &http,
                    base_url,
                    &claude,
                    &mut report,
                    &mut entries,
                    sink,
                )
                .await;
            }
        } else {
            for dep in deps {
                let label = format!("skills/{}", dep.display_hint());
                sink.begin(&label);
                let reason =
                    "skill installs require pakx-registry; --no-pakx-registry refused".to_owned();
                sink.finish_failed(&label, &reason);
                push_failed(&mut report, label, PackageType::Skills, None, reason);
            }
        }
    }

    // Subagents / prompts / commands / hooks — wired through
    // pakx-registry via the generic bundle installer. Each kind
    // extracts under a kind-specific subdirectory of the Claude Code
    // tree (`agents/`, `prompts/`, `commands/`, `hooks/`) but is
    // otherwise identical to the skill path.
    install_all_bundle_deps(
        &manifest,
        pakx_source_with_url.as_ref(),
        &claude,
        &mut report,
        &mut entries,
        sink,
    )
    .await;

    // Rollback gate (only when `--rollback-on-error` set, i.e.
    // `snapshot.is_some()`): a failed run restores the pre-run
    // filesystem state and reports zero installs; a clean run commits
    // (drops the snapshot, keeping what landed). When the flag is absent
    // this whole block is a no-op and partial installs survive exactly
    // as before.
    apply_rollback_gate(snapshot, &mut report)?;

    // Lockfile write gate: skip if any dep failed.
    //
    // The previous flow wrote `agents.lock` unconditionally even when
    // `report.failed` was non-empty. That left a half-pinned lockfile
    // on disk alongside a non-zero exit code — downstream tools
    // (`pakx test`, `pakx list`, `pakx doctor`) then saw an incomplete
    // state that conflicted with the manifest's declared deps, and the
    // user had to manually `rm agents.lock` to retry from a clean
    // slate. Gating on `report.failed.is_empty()` means a failed
    // install leaves the prior `agents.lock` intact (or absent on a
    // first install). The summary line still prints
    // `installed N, skipped M, failed K` so the user sees what
    // happened. `--no-lockfile` continues to skip the write
    // regardless, mirroring v0.1.
    if !opts.no_lockfile && report.failed.is_empty() {
        let lockfile_path = project_root.join(LOCKFILE_FILENAME);
        let lock = Lockfile {
            lockfile_version: LOCKFILE_VERSION,
            manifest_hash: hash_manifest(&manifest),
            entries,
        };
        write_lockfile_to(&lockfile_path, &lock).with_context(|| {
            format!(
                "write lockfile {}",
                redact_path(&lockfile_path, &project_root)
            )
        })?;
        report.lockfile_path = Some(lockfile_path);
    }

    Ok(report)
}

/// Bundle deps grouped by kind. Order matches the dispatch order used
/// by `runner::run` — `pakx install` reports the kinds in this exact
/// order, so flipping it would change the user-visible log.
const fn bundle_deps(manifest: &Manifest) -> [(PackageType, &Option<Vec<DepSpec>>); 4] {
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
    no_cache: bool,
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
        OfficialMcpSource::with_parts(http_client(), mcp_url, cache_dir_at(&cache_root, no_cache));
    let mut client = RegistryClient::new().with_source(Box::new(mcp));

    if !no_smithery {
        let url = match smithery_base_url {
            Some(u) => {
                validate_base_url(u)?;
                u
            }
            None => SMITHERY_BASE_URL,
        };
        let sm =
            SmitherySource::with_parts(http_client(), url, cache_dir_at(&cache_root, no_cache));
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
        let pakx = PakxSource::with_parts(http_client(), url, cache_dir_at(&cache_root, no_cache));
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

#[allow(clippy::too_many_lines)] // linear per-dep lifecycle (resolve → translate → install) with a sink call at each boundary; splitting would scatter the error-path parity
async fn install_mcp_dep(
    dep: &DepSpec,
    client: &RegistryClient,
    claude: &ClaudeCodeAdapter,
    report: &mut InstallReport,
    entries: &mut BTreeMap<String, LockEntry>,
    sink: &dyn ProgressSink,
) {
    let id = match dep {
        DepSpec::String(s) => s.as_str().to_owned(),
        DepSpec::Registry(r) => r.name.clone(),
        DepSpec::Git(g) => {
            // "Not implemented yet" is a SKIP, not a failure: routing it
            // through `skipped` (heading reads "unchanged or not yet
            // supported") keeps an otherwise-clean install at exit 0
            // instead of letting an unsupported dep shape kill the run.
            sink.begin(&g.git);
            sink.finish_skipped(&g.git);
            push_skipped(report, g.git.clone(), PackageType::Mcp, None);
            return;
        }
    };
    // Label the bar by the *manifest* id for the whole lifecycle so the
    // begin / phase / finish calls all address the same bar even though
    // `mcp.id` (the resolved canonical id) may differ post-resolution.
    sink.begin(&id);
    sink.phase(&id, Phase::Resolve);
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
            let reason = "not found in any federated registry \
                          (checked official MCP, Smithery, pakx-registry — verify the id; \
                          if it's a skill, try `pakx add skills <id>`)";
            sink.finish_failed(&id, reason);
            push_failed(report, id.clone(), PackageType::Mcp, None, reason.into());
            return;
        }
        Err(e) => {
            sink.finish_failed(&id, &e.to_string());
            push_failed(report, id.clone(), PackageType::Mcp, None, e.to_string());
            return;
        }
    };

    let transport = match translate(&pkg) {
        Ok(t) => t,
        Err(TranslateError::NoTransport { .. }) => {
            let reason = "advertises no npm / pypi / docker / http transport pakx can install \
                          — check the upstream registry entry or report it to the publisher";
            sink.finish_failed(&id, reason);
            push_failed(
                report,
                id.clone(),
                PackageType::Mcp,
                Some(pkg.version.clone()),
                reason.into(),
            );
            return;
        }
        Err(e) => {
            sink.finish_failed(&id, &e.to_string());
            push_failed(
                report,
                id.clone(),
                PackageType::Mcp,
                Some(pkg.version.clone()),
                e.to_string(),
            );
            return;
        }
    };

    sink.phase(&id, Phase::Install);
    let mcp = McpServer {
        id: pkg.id.clone(),
        version: pkg.version.clone(),
        transport,
    };
    let integrity = mcp.computed_integrity();

    match claude.install_mcp(&mcp).await {
        Ok(_) => {
            // A successful `install_mcp` is the ONLY path that mutates
            // `.mcp.json` (the adapter merges + writes on Ok; it returns
            // `AlreadyInstalled` *before* writing when the entry is
            // unchanged). Record the write so the rollback gate knows it
            // may legitimately revert the merge file — see
            // `apply_rollback_gate`.
            report.mcp_json_written = true;
            sink.finish_ok(&id);
            push_ok(
                report,
                mcp.id.clone(),
                PackageType::Mcp,
                Some(mcp.version.clone()),
            );
            entries.insert(mcp.lockfile_key(), lock_entry_for(&mcp, integrity, source));
        }
        Err(AdapterError::AlreadyInstalled { id: installed_id }) => {
            sink.finish_skipped(&id);
            push_skipped(
                report,
                installed_id,
                PackageType::Mcp,
                Some(mcp.version.clone()),
            );
            entries.insert(mcp.lockfile_key(), lock_entry_for(&mcp, integrity, source));
        }
        Err(e) => {
            sink.finish_failed(&id, &e.to_string());
            push_failed(
                report,
                mcp.id,
                PackageType::Mcp,
                Some(mcp.version.clone()),
                e.to_string(),
            );
        }
    }
}

/// Capture a rollback [`Snapshot`] when `--rollback-on-error` is set,
/// else `None`. Pulled out of `run` so the top-level loop stays under
/// the line cap; the snapshot must be taken after the Claude adapter is
/// built (so the resolved home + project root are known) but before any
/// install loop mutates disk.
///
/// # Errors
///
/// Surfaces a snapshot-capture failure (backup dir creation or moving a
/// pre-existing target aside).
fn maybe_capture_snapshot(
    opts: &InstallOpts,
    manifest: &Manifest,
    claude: &ClaudeCodeAdapter,
) -> Result<Option<Snapshot>> {
    if !opts.rollback_on_error {
        return Ok(None);
    }
    let snap = Snapshot::capture(manifest, claude.config_dir(), claude.project_root())?;
    Ok(Some(snap))
}

/// Apply the `--rollback-on-error` gate after the install loops finish.
///
/// `snapshot` is `Some` only when the flag was set. On a clean run we
/// commit (drop the snapshot, keeping what landed); on a failed run we
/// restore the pre-run filesystem state and re-cast the report so it no
/// longer claims installs that were reverted. When `snapshot` is `None`
/// this is a no-op and partial installs survive exactly as before.
///
/// # Errors
///
/// Surfaces a restore failure (some targets may be left half-restored;
/// the backup dir is retained on disk for manual recovery).
fn apply_rollback_gate(snapshot: Option<Snapshot>, report: &mut InstallReport) -> Result<()> {
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    if report.failed.is_empty() {
        snapshot.commit();
        return Ok(());
    }
    warn!(
        target: "pakx::install",
        failed = report.failed.len(),
        "install failed with --rollback-on-error; restoring pre-run state"
    );
    snapshot
        .restore(report.mcp_json_written)
        .context("rollback failed; some targets may be left half-restored")?;
    // After a successful rollback nothing the run installed remains on
    // disk. Re-cast every `installed`/`skipped` row as a
    // failure-rolled-back outcome so the human summary + the `--json`
    // payload don't claim installs that were reverted.
    mark_rolled_back(report);
    Ok(())
}

/// After a successful rollback, re-cast the report so it no longer
/// claims installs that were reverted off disk.
///
/// The previously-`Ok` / `Skipped` rows describe work that was undone by
/// the restore, so leaving them in `installed` / `skipped` would lie to
/// the user (and to `--json` consumers). We:
///   * clear the legacy `installed` / `skipped` vecs (nothing remains
///     installed), and
///   * rewrite each structured entry that was `Ok` / `Skipped` to
///     `Failed` with a `rolled back` note, leaving the genuinely-failed
///     rows (which carry the real error) untouched.
///
/// The `failed` vec is left as-is — it already lists the dep(s) whose
/// failure triggered the rollback, which is exactly what the non-zero
/// exit code and summary should reflect.
fn mark_rolled_back(report: &mut InstallReport) {
    report.installed.clear();
    report.skipped.clear();
    for entry in &mut report.entries {
        if matches!(entry.status, InstallStatus::Ok | InstallStatus::Skipped) {
            entry.status = InstallStatus::Failed;
            entry.error = Some("rolled back: install run had failures".to_owned());
        }
    }
}

/// Append a successful install to both the legacy `installed` vec
/// and the structured `entries` list. Keeps the two surfaces in sync.
fn push_ok(report: &mut InstallReport, id: String, kind: PackageType, version: Option<String>) {
    report.installed.push(id.clone());
    report.entries.push(InstallReportEntry {
        id,
        status: InstallStatus::Ok,
        kind,
        version,
        error: None,
    });
}

/// Append a skipped install (already up to date, idempotent reinstall).
fn push_skipped(
    report: &mut InstallReport,
    id: String,
    kind: PackageType,
    version: Option<String>,
) {
    report.skipped.push(id.clone());
    report.entries.push(InstallReportEntry {
        id,
        status: InstallStatus::Skipped,
        kind,
        version,
        error: None,
    });
}

/// Append a failed install with its rendered reason.
fn push_failed(
    report: &mut InstallReport,
    id: String,
    kind: PackageType,
    version: Option<String>,
    reason: String,
) {
    report.failed.push((id.clone(), reason.clone()));
    report.entries.push(InstallReportEntry {
        id,
        status: InstallStatus::Failed,
        kind,
        version,
        error: Some(reason),
    });
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
#[allow(clippy::too_many_arguments)] // matches `install_bundle_dep`; the sink is presentation-only
async fn install_skill_dep(
    dep: &DepSpec,
    source: &PakxSource,
    http: &reqwest::Client,
    base_url: &str,
    claude: &ClaudeCodeAdapter,
    report: &mut InstallReport,
    entries: &mut BTreeMap<String, LockEntry>,
    sink: &dyn ProgressSink,
) {
    // Only `String(...)` shorthand is wired at v0.1 — git + registry
    // object specs need their own resolution paths (Phase C+).
    let shorthand = match dep {
        DepSpec::String(s) => s.as_str().to_owned(),
        DepSpec::Git(g) => {
            // Not-yet-supported → skip (not fail) so it can't trip the
            // run's exit code. See the matching MCP path above.
            sink.begin(&g.git);
            sink.finish_skipped(&g.git);
            push_skipped(report, g.git.clone(), PackageType::Skills, None);
            return;
        }
        DepSpec::Registry(r) => {
            let label = format!("{}/{}", r.registry, r.name);
            sink.begin(&label);
            sink.finish_skipped(&label);
            push_skipped(report, label, PackageType::Skills, None);
            return;
        }
    };

    sink.begin(&shorthand);
    sink.phase(&shorthand, Phase::Resolve);

    let (_, _, requested_version) = match parse_skill_shorthand(&shorthand) {
        Ok(t) => t,
        Err(e) => {
            sink.finish_failed(&shorthand, &e.to_string());
            push_failed(report, shorthand, PackageType::Skills, None, e.to_string());
            return;
        }
    };

    debug!(target: "pakx::install", id = %shorthand, "resolving skill dep");

    // Install path: Claude Code only at v0.1. Other adapters will
    // grow their own extract logic as their adapters land.
    sink.phase(&shorthand, Phase::Install);
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
            sink.finish_failed(&shorthand, &format!("{e:#}"));
            push_failed(
                report,
                shorthand,
                PackageType::Skills,
                requested_version.clone(),
                format!("{e:#}"),
            );
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
    sink.finish_ok(&shorthand);
    push_ok(
        report,
        resolved.id,
        PackageType::Skills,
        Some(resolved.version),
    );
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

/// Drive every `subagents` / `prompts` / `commands` / `hooks` entry
/// through the bundle installer.
///
/// Extracted out of `run` so the top-level loop stays readable
/// (clippy's `too_many_lines` cap on a single function). When
/// `--no-pakx-registry` is set we fail each dep loudly — the same
/// policy the skill path uses — because there is no other source that
/// can satisfy the request.
#[allow(clippy::too_many_arguments)] // linear fan-out; the sink is presentation-only
async fn install_all_bundle_deps(
    manifest: &Manifest,
    pakx_source_with_url: Option<&(PakxSource, String)>,
    claude: &ClaudeCodeAdapter,
    report: &mut InstallReport,
    entries: &mut BTreeMap<String, LockEntry>,
    sink: &dyn ProgressSink,
) {
    for (kind, deps) in bundle_deps(manifest) {
        let Some(deps) = deps else { continue };
        if let Some((source, base_url)) = pakx_source_with_url {
            let http = http_client();
            for dep in deps {
                install_bundle_dep(
                    kind, dep, source, &http, base_url, claude, report, entries, sink,
                )
                .await;
            }
        } else {
            for dep in deps {
                let label = format!("{}/{}", kind.as_str(), dep.display_hint());
                sink.begin(&label);
                let reason = format!(
                    "{} installs require pakx-registry; --no-pakx-registry refused",
                    kind.as_str()
                );
                sink.finish_failed(&label, &reason);
                push_failed(report, label, kind, None, reason);
            }
        }
    }
}

/// Install one bundle dep (commands / subagents / prompts / hooks).
///
/// Mirrors [`install_skill_dep`] step-for-step; the only delta is
/// the call into the kind-parameterised
/// [`super::bundle::install_bundle_from_pakx`].
#[allow(clippy::too_many_arguments)] // matches `install_skill_dep`; refactoring would obscure the parity
async fn install_bundle_dep(
    kind: PackageType,
    dep: &DepSpec,
    source: &PakxSource,
    http: &reqwest::Client,
    base_url: &str,
    claude: &ClaudeCodeAdapter,
    report: &mut InstallReport,
    entries: &mut BTreeMap<String, LockEntry>,
    sink: &dyn ProgressSink,
) {
    // Only the shorthand string form is wired at v0 — git + registry
    // object specs need their own resolution paths (Phase C+), same
    // policy as the skill installer.
    let shorthand = match dep {
        DepSpec::String(s) => s.as_str().to_owned(),
        DepSpec::Git(g) => {
            // Not-yet-supported → skip (not fail); see the MCP path.
            sink.begin(&g.git);
            sink.finish_skipped(&g.git);
            push_skipped(report, g.git.clone(), kind, None);
            return;
        }
        DepSpec::Registry(r) => {
            let label = format!("{}/{}", r.registry, r.name);
            sink.begin(&label);
            sink.finish_skipped(&label);
            push_skipped(report, label, kind, None);
            return;
        }
    };

    sink.begin(&shorthand);
    sink.phase(&shorthand, Phase::Resolve);

    let (_, _, requested_version) = match parse_skill_shorthand(&shorthand) {
        Ok(t) => t,
        Err(e) => {
            sink.finish_failed(&shorthand, &e.to_string());
            push_failed(report, shorthand, kind, None, e.to_string());
            return;
        }
    };

    debug!(target: "pakx::install", kind = kind.as_str(), id = %shorthand, "resolving bundle dep");

    sink.phase(&shorthand, Phase::Install);
    let claude_home = claude.config_dir();
    let resolved = match install_bundle_from_pakx(
        source,
        http,
        base_url,
        claude_home,
        kind,
        &shorthand,
        requested_version.as_deref(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            sink.finish_failed(&shorthand, &format!("{e:#}"));
            push_failed(
                report,
                shorthand,
                kind,
                requested_version.clone(),
                format!("{e:#}"),
            );
            return;
        }
    };

    let lockfile_key = format!("{}/{}@{}", kind.as_str(), resolved.id, resolved.version);
    entries.insert(lockfile_key, lock_entry_for_bundle(&resolved));
    sink.finish_ok(&shorthand);
    push_ok(report, resolved.id, kind, Some(resolved.version));
}

/// Build a lockfile entry for a bundle resolved through pakx-registry.
/// Same shape as [`lock_entry_for_skill`] but pinned to the bundle's
/// kind so downstream readers see the right discriminator.
fn lock_entry_for_bundle(resolved: &ResolvedBundle) -> LockEntry {
    LockEntry {
        name: resolved.id.clone(),
        kind: resolved.kind,
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

#[cfg(test)]
mod tests {
    use super::ADAPTER_WIRED_KINDS;
    use pakx_core::PACKAGE_TYPES;

    /// Invariant: every `PackageType` variant must appear in
    /// [`ADAPTER_WIRED_KINDS`]. After this PR all six kinds have an
    /// install dispatch arm; if a future PR adds a new kind it must
    /// either wire it here too or explicitly remove it from the
    /// constant (which would force `tree` / `why` to render
    /// `skipped`). Either way the test forces the author to think
    /// about the contract.
    #[test]
    fn adapter_wired_kinds_covers_every_package_type() {
        for kind in PACKAGE_TYPES {
            assert!(
                ADAPTER_WIRED_KINDS.contains(&kind),
                "{} missing from ADAPTER_WIRED_KINDS — wire the install dispatch \
                 in runner::run before adding it to the constant",
                kind.as_str(),
            );
        }
    }
}
