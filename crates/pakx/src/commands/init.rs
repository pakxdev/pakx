//! `pakx init` — create an `agents.yml` manifest in the current directory.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::Args;
use inquire::{Confirm, MultiSelect, Text};
use pakx_core::manifest::{AgentId, KNOWN_AGENT_IDS};
use pakx_core::{atomic_write, write_manifest, Dependencies, Manifest};

use crate::redact::{project_root_for, redact_path};
use crate::ui;

/// Default file name produced by `init`.
pub const MANIFEST_FILENAME: &str = "agents.yml";

#[derive(Debug, Clone, Args)]
pub struct InitArgs {
    /// Project name. Defaults to the current directory name.
    #[arg(long)]
    pub name: Option<String>,

    /// Project version (semver). Defaults to `1.0.0`.
    #[arg(long)]
    pub manifest_version: Option<String>,

    /// Agents to target. Repeatable: `--agent claude-code --agent cursor`.
    /// Omit entirely to install to every detected agent.
    #[arg(long = "agent", value_name = "AGENT_ID")]
    pub agents: Vec<String>,

    /// Skip all interactive prompts and take defaults / supplied flags.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Overwrite an existing `agents.yml` without prompting.
    #[arg(short = 'f', long)]
    pub force: bool,

    /// Write the manifest somewhere other than `./agents.yml`. Tests use
    /// this to redirect output; rarely useful at the command line.
    #[arg(long, hide = true)]
    pub output: Option<PathBuf>,
}

pub async fn run(args: InitArgs) -> Result<()> {
    let cwd = env::current_dir().context("cannot read current working directory")?;
    let target = args
        .output
        .clone()
        .unwrap_or_else(|| cwd.join(MANIFEST_FILENAME));

    // `init` is an interactive wizard: without `--yes` it prompts for
    // name / version / agents (and may prompt to overwrite an existing
    // file). Fail fast if there is no TTY to answer those prompts rather
    // than blocking forever on a closed/redirected stdin.
    ui::ensure_interactive(args.yes, "scaffold agents.yml")?;

    handle_existing_file(&target, args.force, args.yes)?;

    let default_name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project")
        .to_owned();

    let name = pick_name(args.name.clone(), &default_name, args.yes)?;
    let version = pick_version(args.manifest_version.clone(), args.yes)?;
    let agents = pick_agents(&args.agents, args.yes)?;

    let manifest = Manifest {
        name,
        version,
        agents,
        dependencies: Dependencies::default(),
    };

    let serialized = write_manifest(&manifest);

    if !args.yes && !args.force {
        println!("\n--- {MANIFEST_FILENAME} preview ---\n{serialized}---");
        let proceed = Confirm::new(&format!("Write to {}?", target.display()))
            .with_default(true)
            .prompt()
            .map_err(|e| anyhow!("prompt failed: {e}"))?;
        if !proceed {
            eprintln!("aborted; nothing written");
            return Ok(());
        }
    }

    let project_root = project_root_for(&target);
    // Route the fresh `agents.yml` write through `atomic_write` so a
    // crash mid-flush cannot leave a half-written manifest that the
    // very next `pakx install` would then refuse to parse. The helper
    // is sync, so move the write to `spawn_blocking` to keep the
    // async caller non-blocking.
    let target_for_write = target.clone();
    let bytes = serialized.into_bytes();
    tokio::task::spawn_blocking(move || atomic_write(&target_for_write, &bytes))
        .await
        .map_err(|e| anyhow!("init writer join failed: {e}"))?
        .with_context(|| format!("write {}", redact_path(&target, &project_root)))?;
    eprintln!(
        "{} wrote {}",
        ui::glyph_ok_err(),
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(MANIFEST_FILENAME)
    );
    Ok(())
}

fn handle_existing_file(target: &Path, force: bool, yes: bool) -> Result<()> {
    if !target.exists() {
        return Ok(());
    }
    if force {
        return Ok(());
    }
    if yes {
        // `--yes` without `--force` is the CI-safe default: never silently
        // overwrite. Force explicit consent for destructive paths. Use
        // the file name (not the full path) so CI logs / pasted output
        // don't leak the host's temp / project directory.
        let label = target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(MANIFEST_FILENAME);
        return Err(anyhow!("{label} already exists; pass --force to overwrite"));
    }
    let proceed = Confirm::new(&format!("{} already exists. Overwrite?", target.display()))
        .with_default(false)
        .prompt()
        .map_err(|e| anyhow!("prompt failed: {e}"))?;
    if proceed {
        Ok(())
    } else {
        Err(anyhow!("aborted; existing file kept"))
    }
}

fn pick_name(supplied: Option<String>, default: &str, yes: bool) -> Result<String> {
    if let Some(v) = supplied {
        return Ok(v);
    }
    if yes {
        return Ok(default.to_owned());
    }
    Text::new("Project name?")
        .with_default(default)
        .prompt()
        .map_err(|e| anyhow!("prompt failed: {e}"))
}

fn pick_version(supplied: Option<String>, yes: bool) -> Result<String> {
    if let Some(v) = supplied {
        return Ok(v);
    }
    if yes {
        return Ok("1.0.0".to_owned());
    }
    Text::new("Project version?")
        .with_default("1.0.0")
        .prompt()
        .map_err(|e| anyhow!("prompt failed: {e}"))
}

/// Returns `Some(non-empty list)` when targeting specific agents, or
/// `None` to mean "install to every detected agent".
fn pick_agents(supplied: &[String], yes: bool) -> Result<Option<Vec<AgentId>>> {
    if !supplied.is_empty() {
        let parsed: Result<Vec<_>, _> = supplied
            .iter()
            .map(|s| AgentId::parse(s.as_str()))
            .collect();
        let parsed = parsed.map_err(|e| anyhow!(e))?;
        return Ok(Some(parsed));
    }
    if yes {
        // Default behavior: target every detected agent. Manifest omits
        // the `agents:` key to signal this.
        return Ok(None);
    }
    let options: Vec<&str> = KNOWN_AGENT_IDS.to_vec();
    let chosen = MultiSelect::new(
        "Target which agents? (Enter to confirm; empty = every detected agent)",
        options,
    )
    .prompt()
    .map_err(|e| anyhow!("prompt failed: {e}"))?;
    if chosen.is_empty() {
        Ok(None)
    } else {
        let parsed: Result<Vec<_>, _> = chosen.into_iter().map(AgentId::parse).collect();
        Ok(Some(parsed.map_err(|e| anyhow!(e))?))
    }
}
