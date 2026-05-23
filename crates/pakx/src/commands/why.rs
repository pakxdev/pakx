//! `pakx why <id>` — reverse-lookup of a dependency id.
//!
//! Tells the user where a given id is declared (`agents.yml`),
//! whether it's pinned in the lockfile (`agents.lock`), which
//! registry resolved it, and whether the install adapter is wired
//! for its kind. Closes parity with `pnpm why` / `npm explain`.
//!
//! Behaviour:
//!
//! - Accepts `owner/name` or `owner/name@version`. When a version is
//!   supplied it's only used to refine the manifest-side lookup; the
//!   lockfile match is by id (a lockfile can only hold one row per
//!   `<kind>/<id>@<version>` triple).
//! - When the id appears in multiple `dependencies.<kind>` sections
//!   (e.g. listed under both `skills:` and `mcp:`), every match is
//!   rendered. `--kind <type>` filters to one.
//! - Lockfile-only entries (id appears in `agents.lock` but not in
//!   `agents.yml`) are surfaced too — useful when chasing where a
//!   transitive pin came from.
//!
//! Exit codes:
//!   - human mode: `0` on at least one match, `1` on no match.
//!   - `--json` mode: always `0` (empty array `[]` for no match).
//!     `outdated` uses the same discipline — `jq` pipelines never
//!     break on an empty result.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use pakx_core::{
    read_lockfile_from, read_manifest_from, split_shorthand, LockEntry, Manifest, PackageType,
    RegistrySource, PACKAGE_TYPES,
};
use pakx_registry_client::PAKX_BASE_URL;
use serde::Serialize;

use crate::install::ADAPTER_WIRED_KINDS;
use crate::ui;

const MANIFEST_FILENAME: &str = "agents.yml";
const LOCKFILE_FILENAME: &str = "agents.lock";

#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum KindFilter {
    Skills,
    Mcp,
    Subagents,
    Prompts,
    Commands,
    Hooks,
}

impl KindFilter {
    const fn as_package_type(self) -> PackageType {
        match self {
            Self::Skills => PackageType::Skills,
            Self::Mcp => PackageType::Mcp,
            Self::Subagents => PackageType::Subagents,
            Self::Prompts => PackageType::Prompts,
            Self::Commands => PackageType::Commands,
            Self::Hooks => PackageType::Hooks,
        }
    }
}

/// Adapter wiring status. Backed by the single-source-of-truth
/// constant [`crate::install::ADAPTER_WIRED_KINDS`] so this command
/// and `pakx tree` can never drift out of sync.
fn adapter_status(kind: PackageType) -> &'static str {
    if ADAPTER_WIRED_KINDS.contains(&kind) {
        "wired"
    } else {
        "skipped"
    }
}

#[derive(Debug, Clone, Args)]
pub struct WhyArgs {
    /// Package id to explain. Accepts `owner/name` or
    /// `owner/name@version` — the `@version` segment is matched
    /// against the manifest shorthand when present.
    pub id: String,

    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Restrict the lookup to a single `dependencies` section.
    #[arg(long, value_name = "TYPE")]
    pub kind: Option<KindFilter>,

    /// Emit machine-readable JSON on stdout (single line,
    /// newline-terminated). Field names are a stable contract for
    /// downstream pipelines.
    #[arg(long)]
    pub json: bool,
}

/// Wire-format row emitted by `--json`. Field names are a stable
/// contract — only additive changes (new optional fields) are
/// backwards-compatible.
#[derive(Debug, Serialize)]
struct JsonRow<'a> {
    id: &'a str,
    kind: &'static str,
    #[serde(rename = "manifestSource")]
    manifest_source: Option<&'static str>,
    #[serde(rename = "lockedVersion")]
    locked_version: Option<&'a str>,
    registry: Option<&'static str>,
    #[serde(rename = "registryUrl")]
    registry_url: Option<String>,
    adapter: &'static str,
}

#[derive(Debug)]
struct Match<'a> {
    kind: PackageType,
    in_manifest: bool,
    lock_entry: Option<&'a LockEntry>,
}

#[allow(clippy::unused_async)] // matches every other `commands::*::run` signature
pub async fn run(args: WhyArgs) -> Result<ExitCode> {
    if args.json {
        // `--json | jq` discipline: keep stdout byte-clean. Stderr
        // remains color-able (the "not found" hint, etc.).
        crate::ui::force_stdout_no_color();
    }
    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };
    let manifest_path = project_root.join(MANIFEST_FILENAME);
    let lockfile_path = project_root.join(LOCKFILE_FILENAME);

    // Strip any `@version` suffix the user typed — manifest lookups
    // work on the id-without-version, lockfile lookups work on id
    // (each version lives at a distinct entry key but the LockEntry
    // `.name` field carries only the id).
    let (id_no_version, _requested_version) = split_shorthand(args.id.as_str());

    // Manifest is optional — a user can run `pakx why` in a project
    // that has a lockfile but no checked-in `agents.yml` (e.g. CI
    // archived only the lock). All manifest-read errors are
    // swallowed here on purpose; this is an informational command
    // and we'd rather render the lockfile half than bail.
    let manifest_opt: Option<Manifest> = read_manifest_from(&manifest_path).ok();

    let lock_opt = read_lockfile_from(&lockfile_path)
        .with_context(|| format!("read lockfile {}", lockfile_path.display()))?;

    let kind_filter = args.kind.map(KindFilter::as_package_type);

    // Collect every `dependencies.<kind>` that holds a shorthand
    // matching `id_no_version`. Mirrors the federated lookup used by
    // `pakx update` so behaviour stays consistent.
    let mut matches: Vec<Match<'_>> = Vec::new();
    for kind in PACKAGE_TYPES {
        if let Some(filter) = kind_filter {
            if kind != filter {
                continue;
            }
        }
        let in_manifest = manifest_opt
            .as_ref()
            .is_some_and(|m| section_contains(m, kind, id_no_version));
        let lock_entry = lock_opt.as_ref().and_then(|l| {
            l.entries
                .values()
                .find(|e| e.kind == kind && e.name == id_no_version)
        });
        if in_manifest || lock_entry.is_some() {
            matches.push(Match {
                kind,
                in_manifest,
                lock_entry,
            });
        }
    }

    if args.json {
        render_json(id_no_version, &matches);
        // JSON mode always exits 0 — empty arrays are jq-friendly.
        return Ok(ExitCode::SUCCESS);
    }

    if matches.is_empty() {
        eprintln!("{id_no_version} not found in {MANIFEST_FILENAME} or {LOCKFILE_FILENAME}");
        return Ok(ExitCode::from(1));
    }

    render_human(id_no_version, &matches);
    Ok(ExitCode::SUCCESS)
}

fn section_contains(manifest: &Manifest, kind: PackageType, id_no_version: &str) -> bool {
    let Some(deps) = manifest.dependencies.get(kind) else {
        return false;
    };
    deps.iter().any(|dep| match dep {
        pakx_core::DepSpec::String(s) => split_shorthand(s.as_str()).0 == id_no_version,
        // git / registry-object specs don't carry a comparable id
        // shorthand — `pakx why owner/name` deliberately skips them.
        pakx_core::DepSpec::Git(_) | pakx_core::DepSpec::Registry(_) => false,
    })
}

fn registry_url_for(registry: RegistrySource, id: &str) -> Option<String> {
    match registry {
        // Only the pakx registry has a stable per-package canonical
        // URL the user can paste into a browser. The federated MCP /
        // Smithery sources have one too but with different shapes;
        // surfacing only the pakx one keeps the contract simple and
        // matches the spec ("registry: pakx (https://...)").
        RegistrySource::Pakx => Some(format!("{PAKX_BASE_URL}/api/v1/packages/{id}")),
        RegistrySource::OfficialMcp
        | RegistrySource::Smithery
        | RegistrySource::Glama
        | RegistrySource::Github
        | RegistrySource::Git => None,
    }
}

fn render_json(id_no_version: &str, matches: &[Match<'_>]) {
    let rows: Vec<JsonRow<'_>> = matches
        .iter()
        .map(|m| {
            let locked_version = m.lock_entry.map(|e| e.version.as_str());
            let registry = m.lock_entry.map(|e| e.registry.as_tag());
            let registry_url = m
                .lock_entry
                .and_then(|e| registry_url_for(e.registry, e.name.as_str()));
            JsonRow {
                id: id_no_version,
                kind: m.kind.as_str(),
                manifest_source: if m.in_manifest {
                    Some(MANIFEST_FILENAME)
                } else {
                    None
                },
                locked_version,
                registry,
                registry_url,
                adapter: adapter_status(m.kind),
            }
        })
        .collect();
    let line = serde_json::to_string(&rows).expect("serialize why rows");
    println!("{line}");
}

fn render_human(id_no_version: &str, matches: &[Match<'_>]) {
    println!("{}", ui::heading(id_no_version));
    for m in matches {
        if m.in_manifest {
            println!(
                "  found in {} under `{}:`",
                MANIFEST_FILENAME,
                m.kind.as_str()
            );
        } else {
            println!(
                "  {}",
                ui::dim(&format!("not declared in {MANIFEST_FILENAME}"))
            );
        }
        if let Some(entry) = m.lock_entry {
            println!("  pinned in {} at {}", LOCKFILE_FILENAME, entry.version);
            let registry_tag = entry.registry.as_tag();
            if let Some(url) = registry_url_for(entry.registry, entry.name.as_str()) {
                println!("  registry: {registry_tag} ({url})");
            } else {
                println!("  registry: {registry_tag}");
            }
        } else {
            println!("  {}", ui::dim(&format!("no pin in {LOCKFILE_FILENAME}")));
        }
        let adapter = adapter_status(m.kind);
        if adapter == "wired" {
            println!("  adapter: wired ({})", m.kind.as_str());
        } else {
            println!(
                "  adapter: skipped ({} not yet implemented)",
                m.kind.as_str()
            );
        }
    }
}
