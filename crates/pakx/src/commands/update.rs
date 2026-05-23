//! `pakx update` — rewrite `agents.yml` pins to a newer version, then
//! reconcile via `pakx install`.
//!
//! Closes the loop opened by `pakx outdated` (round 14):
//!   - `pakx outdated` surfaces deps with a newer registry-side version.
//!   - `pakx update` rewrites the shorthand pin in `agents.yml` and
//!     re-runs the install pipeline so the lockfile + on-disk install
//!     state catch up in one step.
//!
//! Three input shapes:
//!   - `pakx update` (no args) — interactive prompt per outdated dep.
//!     `--yes` / `-y` accepts every prompt without asking.
//!   - `pakx update <id>` — find the matching dep, query the registry
//!     for its latest non-deprecated version, rewrite to that. Acts
//!     as if `--yes` was supplied (explicit invocation = consent).
//!   - `pakx update <id>@<version>` — pin to the supplied version
//!     verbatim (no registry round-trip). Allows downgrades and works
//!     even when the registry is unreachable.
//!
//! Out of scope for v0.1 (and the [`Out of scope`] section of the
//! `pakx update` spec):
//!   - Git or registry-object specs. They surface as a hard error
//!     because the shorthand-string rewriter has no path for them.
//!   - Conflict resolution across dep kinds. The single-section
//!     auto-pick mirrors `pakx remove`'s behaviour: pick when
//!     unambiguous, error otherwise (the user re-runs with `--kind`).
//!   - `--major-only` / `--minor-only` filters. Adding them would
//!     bloat the surface without serving the v0.1 user, who is almost
//!     always running `pakx update` on a known-good drift.
//!
//! Exit codes:
//!   - `0` — successful update (or nothing to do).
//!   - `1` — install reconciliation failed.
//!   - `2` — unable to determine target version (registry unreachable
//!     for all candidates) when the user did not pin a version
//!     explicitly. Matches the spec.

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, ValueEnum};
use inquire::Confirm;
use pakx_core::manifest::{
    read_from, sections_containing_id, split_shorthand, update_shorthand, write_to, DepSpec,
    Manifest, PackageType, UpdateOutcome, PACKAGE_TYPES,
};
use tracing::debug;

use super::outdated::{gather_outdated, Row, Status};
use crate::install::{run as install_run, InstallOpts};
use crate::redact::{project_root_for, redact_path};
use crate::ui;

const MANIFEST_FILENAME: &str = "agents.yml";

/// CLI-facing copy of [`PackageType`] so clap can derive `ValueEnum`
/// without forcing the trait onto the core type. Matches the variant
/// set used by `pakx add --type` / `pakx remove --kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UpdateKind {
    Skills,
    Mcp,
    Subagents,
    Prompts,
    Commands,
    Hooks,
}

impl UpdateKind {
    const fn to_core(self) -> PackageType {
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

#[derive(Debug, Clone, Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flags are independent toggles; a state machine here would obscure the surface
pub struct UpdateArgs {
    /// Either a single dep id (`owner/name`) or a pinned form
    /// (`owner/name@<version>`). Omit to run the interactive
    /// outdated-flow over every dep.
    pub id: Option<String>,

    /// Skip the per-dep confirmation prompt. Required for non-
    /// interactive use (CI / scripted updates).
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Show what would change without rewriting `agents.yml` or
    /// running install.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the post-update `pakx install` reconciliation. The
    /// manifest is rewritten and the user is responsible for running
    /// `pakx install` themselves. Useful in pipelines that batch
    /// multiple changes before reconciling.
    #[arg(long)]
    pub no_install: bool,

    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Override the pakx-registry base URL (testing).
    #[arg(long, hide = true)]
    pub pakx_base_url: Option<String>,

    /// Override the official MCP Registry base URL (testing).
    #[arg(long, hide = true)]
    pub mcp_base_url: Option<String>,

    /// Override the Smithery registry base URL (testing).
    #[arg(long, hide = true)]
    pub smithery_base_url: Option<String>,

    /// Override the Claude Code home directory (testing). Forwarded
    /// verbatim to the post-update install runner.
    #[arg(long, hide = true)]
    pub claude_home: Option<PathBuf>,

    /// Skip the Smithery source for both the outdated check + the
    /// post-update install. Mirrors `pakx install --no-smithery`.
    #[arg(long)]
    pub no_smithery: bool,

    /// Skip the pakx-registry source for the post-update install.
    /// Note that the outdated check still needs pakx-registry to
    /// resolve `pakx`-tagged entries; setting this turns those rows
    /// into `error` and the corresponding update attempt becomes a
    /// no-op (matches `pakx install` semantics).
    #[arg(long)]
    pub no_pakx_registry: bool,

    /// Explicit dep-kind disambiguator. Required when the requested
    /// `<id>` is declared under more than one section in
    /// `agents.yml`; ignored (but accepted) otherwise. Mirrors the
    /// `--kind` flag on `pakx remove`.
    #[arg(short = 'k', long = "kind", value_enum)]
    pub kind: Option<UpdateKind>,
}

/// One concrete rewrite to apply to the manifest. Either supplied
/// directly by the user (`pakx update <id>@<version>`) or derived from
/// a `pakx outdated` row (interactive / bulk flow).
#[derive(Debug, Clone)]
struct Plan {
    /// `<owner>/<name>` — the pre-`@` segment used to match the
    /// shorthand entry in `agents.yml`.
    id_no_version: String,
    /// Previous shorthand text for the log line. Filled in **after**
    /// the manifest mutation since only `update_shorthand` knows the
    /// exact previous value (which may have included `@version` or
    /// not).
    previous: Option<String>,
    /// Target version to pin.
    new_version: String,
    /// Registry tag — surfaced in the log line so the user knows
    /// which source promoted the new version. `None` for the
    /// explicit-version form (no registry query happened).
    registry_tag: Option<&'static str>,
}

#[allow(clippy::too_many_lines)] // linear orchestration: planning, prompt, mutate, write, install
pub async fn run(args: UpdateArgs) -> Result<ExitCode> {
    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => env::current_dir().context("cannot read cwd")?,
    };
    let manifest_path = project_root.join(MANIFEST_FILENAME);
    let mut manifest = read_from(&manifest_path).map_err(|e| {
        anyhow!(e).context(format!(
            "read manifest at {}",
            redact_path(&manifest_path, &project_root)
        ))
    })?;

    let plans = match plan_phase(&args, &project_root).await? {
        PlanPhase::Plans(p) => p,
        PlanPhase::ExitWithCode(code) => return Ok(code),
    };

    if plans.is_empty() {
        eprintln!("{}", ui::dim_err("\u{2192} all dependencies up to date"));
        return Ok(ExitCode::SUCCESS);
    }

    // Walk every plan: prompt (unless `--yes` / explicit id), mutate
    // the in-memory manifest, capture the previous shorthand.
    let mut applied: Vec<Plan> = Vec::with_capacity(plans.len());
    let mut kept = 0usize;
    let explicit_id = args.id.is_some();
    for mut plan in plans {
        // Per-id confirmation. Skip when the user passed an explicit
        // id (the invocation itself is consent), `--yes`, or
        // `--dry-run` (rendering a confirmation prompt during a
        // dry-run is just noise).
        let should_prompt = !args.yes && !args.dry_run && !explicit_id;
        if should_prompt && !confirm_update(&plan)? {
            kept += 1;
            println!(
                "{} kept {} at the existing pin",
                ui::glyph_info(),
                plan.id_no_version
            );
            continue;
        }

        // Find the section. Single-section auto-pick mirrors
        // `pakx remove`. When the id is ambiguous (declared in
        // multiple sections) the user disambiguates with `--kind`.
        let kind = pick_kind(
            &manifest,
            &plan.id_no_version,
            args.kind.map(UpdateKind::to_core),
        )?;
        if args.dry_run {
            println!(
                "{} would update {} -> {} ({})",
                ui::glyph_info(),
                plan.id_no_version,
                plan.new_version,
                kind.as_str(),
            );
            continue;
        }

        match update_shorthand(&mut manifest, kind, &plan.id_no_version, &plan.new_version) {
            UpdateOutcome::Updated { previous } => {
                plan.previous = Some(previous);
                applied.push(plan);
            }
            UpdateOutcome::NotPresent => {
                // `pick_kind` returned a section that holds the id;
                // a NotPresent from update_shorthand on that same
                // section is genuinely surprising. Surface loudly.
                bail!(
                    "internal: section {} unexpectedly missing entry for {}",
                    kind.as_str(),
                    plan.id_no_version
                );
            }
            UpdateOutcome::NonShorthand => {
                bail!(
                    "{} is a git or registry-object spec; `pakx update` only handles shorthand `<id>@<version>` deps today",
                    plan.id_no_version
                );
            }
        }
    }

    if args.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    if applied.is_empty() {
        // Nothing was actually changed in-memory. Don't rewrite or
        // install. The summary still prints so the user sees the
        // outcome.
        print_summary(applied.len(), kept);
        return Ok(ExitCode::SUCCESS);
    }

    let project_root_for_redact = project_root_for(&manifest_path);
    write_to(&manifest_path, &manifest).with_context(|| {
        format!(
            "write {}",
            redact_path(&manifest_path, &project_root_for_redact)
        )
    })?;

    // Per-update success line. Mirrors the spec's example output.
    for plan in &applied {
        let previous = plan.previous.as_deref().unwrap_or("?");
        let registry = plan
            .registry_tag
            .map_or_else(String::new, |t| format!(" [{t}]"));
        println!(
            "{} updated {} to {}  ({} -> {}){}",
            ui::glyph_ok(),
            ui::success(&plan.id_no_version),
            plan.new_version,
            previous,
            plan.new_version,
            registry,
        );
    }

    print_summary(applied.len(), kept);

    if args.no_install {
        // Mirror the `→ next:` hint cadence the other action commands
        // use. The user opted out so we point at the rest of the loop.
        // Propagate `--directory <dir>` into the hint when set so the
        // user can copy-paste the suggested command verbatim — without
        // this, a `pakx update --directory subdir/ --no-install` user
        // saw `→ next: pakx install` and had to remember to re-thread
        // the directory flag themselves.
        let hint = args.directory.as_deref().map_or_else(
            || String::from("\u{2192} next: pakx install"),
            |dir| format!("\u{2192} next: pakx install --directory {}", dir.display()),
        );
        println!("{}", ui::dim(&hint));
        return Ok(ExitCode::SUCCESS);
    }

    // Reconcile via the in-process install runner.
    eprintln!();
    eprintln!("{}", ui::dim_err("\u{2192} running pakx install"));
    let opts = InstallOpts {
        project_root: Some(project_root.clone()),
        mcp_base_url: args.mcp_base_url,
        smithery_base_url: args.smithery_base_url,
        pakx_base_url: args.pakx_base_url,
        no_smithery: args.no_smithery,
        no_pakx_registry: args.no_pakx_registry,
        claude_home: args.claude_home,
        no_lockfile: false,
    };
    let report = install_run(opts).await?;
    if !report.failed.is_empty() {
        eprintln!("{}", ui::heading("failed:"));
        for (id, reason) in &report.failed {
            eprintln!("  {} {id}: {reason}", ui::glyph_fail_err());
        }
        eprintln!(
            "{} {} dep(s) failed to install",
            ui::glyph_fail_err(),
            report.failed.len()
        );
        return Ok(ExitCode::from(1));
    }
    if !report.installed.is_empty() {
        eprintln!(
            "{} installed {} entr{} after update",
            ui::glyph_ok_err(),
            report.installed.len(),
            if report.installed.len() == 1 {
                "y"
            } else {
                "ies"
            },
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Result of the planning phase. Either a list of plans to apply, or
/// a short-circuit exit code (e.g. `2` when a single-id query could
/// not determine a target version).
enum PlanPhase {
    Plans(Vec<Plan>),
    ExitWithCode(ExitCode),
}

/// Decide which plans to apply. Three shapes:
///   - explicit `id@version` -> one plan, no registry query.
///   - bare `id`             -> one plan, registry decides version.
///   - no `id`               -> fan out via `gather_outdated`.
async fn plan_phase(args: &UpdateArgs, project_root: &Path) -> Result<PlanPhase> {
    let Some(arg) = args.id.as_deref() else {
        let rows = gather_outdated(
            project_root,
            args.pakx_base_url.as_deref(),
            args.mcp_base_url.as_deref(),
            args.smithery_base_url.as_deref(),
            None,
        )
        .await?;
        // Error rows print on stderr via `gather_outdated`'s internals;
        // the planning layer only needs `upgrade` / `drift` actionable
        // rows.
        return Ok(PlanPhase::Plans(collect_plans_from_rows(rows.as_slice())));
    };
    if let (id, Some(version)) = split_shorthand(arg) {
        return Ok(PlanPhase::Plans(vec![Plan {
            id_no_version: id.to_owned(),
            previous: None,
            new_version: version.to_owned(),
            registry_tag: None,
        }]));
    }
    // Bare id form.
    let id = split_shorthand(arg).0;
    match build_plan_for_id(
        project_root,
        id,
        args.pakx_base_url.as_deref(),
        args.mcp_base_url.as_deref(),
        args.smithery_base_url.as_deref(),
    )
    .await?
    {
        SingleIdOutcome::Plan(plan) => Ok(PlanPhase::Plans(vec![plan])),
        // Already-at-latest is the success path — emit an empty plan
        // list so the outer flow renders the "all dependencies up to
        // date" hint and exits 0.
        SingleIdOutcome::AlreadyLatest => Ok(PlanPhase::Plans(Vec::new())),
        // Couldn't determine a target version. Per spec, exit 2.
        SingleIdOutcome::NoTarget => Ok(PlanPhase::ExitWithCode(ExitCode::from(2))),
    }
}

/// What `build_plan_for_id` produces for a single requested id.
enum SingleIdOutcome {
    /// Registry has a newer version — proceed with this plan.
    Plan(Plan),
    /// Already at the latest version. Exit `0` with the
    /// "all dependencies up to date" hint.
    AlreadyLatest,
    /// Registry could not produce a target version (unreachable or
    /// unsupported source). Exit `2` per spec.
    NoTarget,
}

/// Build a single plan when the user types `pakx update <id>` (no
/// `@version`). Runs the same federated outdated check the no-arg
/// flow uses, filtered to the requested id.
///
/// `Err(_)` is reserved for "id not in lockfile" — a user-error
/// surface (exit 1 via the anyhow main path). Network failure on the
/// specific id is **not** an error; it maps to `NoTarget` so the
/// caller can produce the spec-mandated exit code `2`.
async fn build_plan_for_id(
    project_root: &Path,
    id: &str,
    pakx_base_url: Option<&str>,
    mcp_base_url: Option<&str>,
    smithery_base_url: Option<&str>,
) -> Result<SingleIdOutcome> {
    let rows = gather_outdated(
        project_root,
        pakx_base_url,
        mcp_base_url,
        smithery_base_url,
        None,
    )
    .await?;
    if rows.is_empty() {
        bail!("no agents.lock found — run `pakx install` first");
    }
    let Some(row) = rows.iter().find(|r| r.id == id) else {
        bail!("{id} is not pinned in agents.lock");
    };
    match row.status {
        Status::Upgrade | Status::Drift => Ok(SingleIdOutcome::Plan(plan_from_row(row))),
        Status::UpToDate => {
            eprintln!(
                "{} {} already at the latest version ({})",
                ui::glyph_ok_err(),
                row.id,
                row.current,
            );
            Ok(SingleIdOutcome::AlreadyLatest)
        }
        Status::Error => {
            // Registry unreachable for this specific id. The exact
            // stderr line that drove the row was already printed by
            // `gather_outdated`. Surface the consolidated `[fail]`
            // line here so the user has a single actionable
            // diagnostic right above the exit-2 status.
            eprintln!(
                "{} could not determine latest version for {} (registry unreachable)",
                ui::glyph_fail_err(),
                row.id
            );
            Ok(SingleIdOutcome::NoTarget)
        }
        Status::Unknown | Status::Skip => {
            eprintln!(
                "{} {} cannot be updated: registry {} does not support outdated checks",
                ui::glyph_warn_err(),
                row.id,
                row.registry.as_tag(),
            );
            Ok(SingleIdOutcome::NoTarget)
        }
    }
}

/// Pull every actionable row from a bulk outdated run into a plan.
/// Errors / unknowns / skips are dropped silently — `gather_outdated`
/// already routed them to stderr and they're not actionable in the
/// update flow.
fn collect_plans_from_rows(rows: &[Row]) -> Vec<Plan> {
    rows.iter()
        .filter(|r| matches!(r.status, Status::Upgrade | Status::Drift))
        .map(plan_from_row)
        .collect()
}

fn plan_from_row(row: &Row) -> Plan {
    // `Status::Upgrade` / `Status::Drift` guarantees `latest` is Some.
    // If it isn't, the row was misclassified — fall back to the
    // current version as a no-op (the mutate layer will see no
    // change and report `NotPresent`).
    let latest = row.latest.clone().unwrap_or_else(|| row.current.clone());
    Plan {
        id_no_version: row.id.clone(),
        previous: None,
        new_version: latest,
        registry_tag: Some(row.registry.as_tag()),
    }
}

/// Decide which `dependencies` section to rewrite for `id_no_version`.
///
/// Resolution order — mirrors `pakx remove`:
///   1. Explicit `--kind` wins. Rejected with a clean diagnostic when
///      no entry of that kind matches the requested id (better to fail
///      loudly than to silently rewrite a sibling section).
///   2. Unambiguous shorthand match (exactly one section) auto-picks.
///   3. Ambiguous (≥2 sections) errors out with the rerun hint that
///      names the candidate kinds.
///   4. No match falls through to either the non-shorthand-spec
///      diagnostic (the spec mandates that exact message) or the
///      `not found in agents.yml` fallback.
fn pick_kind(
    manifest: &Manifest,
    id_no_version: &str,
    explicit: Option<PackageType>,
) -> Result<PackageType> {
    let present = sections_containing_id(manifest, id_no_version);
    if let Some(kind) = explicit {
        if !present.contains(&kind) {
            bail!(
                "no `{}` entry named `{id_no_version}` in agents.yml",
                kind.as_str(),
            );
        }
        return Ok(kind);
    }
    match present.as_slice() {
        [only] => return Ok(*only),
        many if !many.is_empty() => {
            let listed: Vec<&str> = many.iter().map(|k| k.as_str()).collect();
            bail!(
                "{id_no_version} is declared under multiple sections ({}); rerun with `--kind <{}>`",
                listed.join(", "),
                listed.join("|"),
            )
        }
        _ => {}
    }
    // No shorthand match — check whether a git / registry-object spec
    // matches the requested id and surface the spec-mandated error.
    if has_non_shorthand_match(manifest, id_no_version) {
        bail!(
            "{id_no_version} is a git or registry-object spec; `pakx update` only handles shorthand `<id>@<version>` deps today",
        );
    }
    bail!("{id_no_version} not found in agents.yml");
}

/// Returns true when at least one dep in the manifest's
/// `dependencies` is a non-shorthand spec whose identifying string
/// matches `id_no_version`. Used to upgrade the "not found" error
/// into "non-shorthand spec" when the user is asking for an id that
/// IS present, just not in the shorthand form `pakx update`
/// supports.
fn has_non_shorthand_match(manifest: &Manifest, id_no_version: &str) -> bool {
    for kind in PACKAGE_TYPES {
        if let Some(list) = manifest.dependencies.get(kind) {
            for dep in list {
                match dep {
                    DepSpec::Git(g) if g.git == id_no_version => return true,
                    DepSpec::Registry(r) => {
                        let combined = format!("{}/{}", r.registry, r.name);
                        if combined == id_no_version || r.name == id_no_version {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    false
}

fn confirm_update(plan: &Plan) -> Result<bool> {
    let registry = plan
        .registry_tag
        .map_or_else(String::new, |t| format!(" [{t}]"));
    let label = format!(
        "{}  -> {}{}\nupdate?",
        plan.id_no_version, plan.new_version, registry
    );
    Confirm::new(&label)
        .with_default(true)
        .prompt()
        .map_err(|e| anyhow!("prompt failed: {e}"))
}

fn print_summary(updated: usize, kept: usize) {
    eprintln!();
    eprintln!(
        "{}: updated {}, kept {}",
        ui::heading("summary"),
        if updated == 0 {
            "0".to_string()
        } else {
            ui::success_err(&updated.to_string())
        },
        kept,
    );
    debug!(target: "pakx::update", updated, kept, "update summary");
}
