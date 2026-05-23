//! `pakx manifest get/set/delete <path>` — scripting surface for
//! field-level access to `agents.yml`.
//!
//! Modelled on `npm pkg get/set/delete`. Path syntax is dot-separated
//! mapping keys plus `[N]` index segments for arrays:
//!
//! - `description` — top-level scalar
//! - `dependencies.skills[0]` — first entry of the skills section
//! - `dependencies.mcp[1].agents` — keys + indices interleave freely
//!
//! Output:
//! - `get` prints the value to stdout (string scalars unquoted; arrays
//!   / mappings as YAML). `--json` reshapes to JSON.
//! - `set` writes back via `pakx_core::atomic_write` so a crash
//!   mid-write can't corrupt `agents.yml`.
//! - `delete` is idempotent — removing a missing path exits 0 with a
//!   warning on stderr.
//!
//! Locked in until v1:
//! - YAML comment preservation is **not** supported. The
//!   `serde_yaml_ng` loader drops comments at parse time so any
//!   round-trip via `pakx manifest set` will strip them. The
//!   sub-subcommand help text surfaces this so future contributors
//!   don't promise it inadvertently.
//! - `set` is a pure-text mutator. Schema validation happens at
//!   `pakx pack` / `pakx test` time, not here.
//!
//! The path-parser + walker live in `pakx_core::manifest::path`
//! (re-exported via the crate root as `manifest_parse_path` /
//! `manifest_get_value` / `manifest_set_value` /
//! `manifest_delete_value`) — single source of truth so this CLI
//! surface stays in lockstep with anything else that wants to address
//! manifest fields by path.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use pakx_core::atomic_write;
use pakx_core::manifest::path::{
    delete_value, get_value, parse_path, set_value, DeleteOutcome, PathSeg,
};
use serde_yaml_ng::Value;

use crate::redact::{project_root_for, redact_path};
use crate::ui;

const MANIFEST_FILENAME: &str = "agents.yml";

#[derive(Debug, Clone, Args)]
pub struct ManifestArgs {
    #[command(subcommand)]
    pub command: ManifestCmd,

    /// Operate on a manifest at a path other than `./agents.yml`.
    /// Used by the integration tests to point at a tempdir without
    /// changing the process cwd.
    #[arg(long, global = true, hide = true)]
    pub manifest: Option<PathBuf>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ManifestCmd {
    /// Read a value out of `agents.yml` by dot-path.
    ///
    /// Path syntax mirrors `npm pkg get`: dot-separated keys + `[N]`
    /// for array indices. Example: `pakx manifest get
    /// dependencies.skills[0]`.
    ///
    /// Exit code is `1` when the path doesn't resolve; under `--json`
    /// the missing case prints `null` to stdout and exits `1` so
    /// scripts can distinguish "field absent" from "field present but
    /// null".
    Get(GetArgs),

    /// Write a value into `agents.yml` by dot-path.
    ///
    /// The value is treated as a string by default — sufficient for
    /// the common case (`pakx manifest set description "new desc"`).
    /// Pass `--json` to accept a JSON-encoded value for non-string
    /// types: `pakx manifest set --json agents '["claude-code"]'`.
    ///
    /// Atomicity: the file is written via the
    /// `pakx_core::atomic_write` helper (the same temp-then-rename
    /// flow `agents.lock` uses) so a crash mid-write leaves the prior
    /// `agents.yml` body intact.
    ///
    /// Note: comments in the existing `agents.yml` are NOT preserved
    /// — the YAML loader drops them at parse time.
    Set(SetArgs),

    /// Remove a key or array element from `agents.yml` by dot-path.
    ///
    /// Idempotent: deleting a missing path exits `0` with a warning
    /// on stderr.
    Delete(DeleteArgs),
}

#[derive(Debug, Clone, Args)]
pub struct GetArgs {
    /// Dot-path to read. See command docs for syntax.
    pub path: String,

    /// Emit the value as JSON instead of YAML / unquoted string.
    /// Missing path under `--json` prints `null` to stdout and exits
    /// `1`.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SetArgs {
    /// Dot-path to write. Creates intermediate keys as needed if the
    /// parent type allows.
    pub path: String,

    /// Replacement value. Interpreted as a string unless `--json`
    /// is set.
    pub value: String,

    /// Treat `<value>` as a JSON-encoded scalar / array / object.
    /// Required for setting non-string types (e.g. arrays, numbers,
    /// booleans). Example: `pakx manifest set --json
    /// dependencies.skills '["alice/bob@0.1.2"]'`.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct DeleteArgs {
    /// Dot-path to remove. Missing paths are a soft no-op (exit 0
    /// with a warning).
    pub path: String,
}

#[allow(clippy::unused_async)] // matches every other `commands::*::run` signature
pub async fn run(args: ManifestArgs) -> Result<ExitCode> {
    let manifest_path = match args.manifest {
        Some(p) => p,
        None => env::current_dir()
            .context("cannot read cwd")?
            .join(MANIFEST_FILENAME),
    };
    // Only `manifest get` reads `--json`; `set` uses it as the value
    // type discriminator (not output mode) and `delete` doesn't expose
    // one. Force stdout to no-color only when the read path is in
    // JSON mode so a `--color always --json | jq` pipeline stays
    // byte-clean.
    if let ManifestCmd::Get(g) = &args.command {
        if g.json {
            crate::ui::force_stdout_no_color();
        }
    }
    match args.command {
        ManifestCmd::Get(g) => run_get(&manifest_path, &g),
        ManifestCmd::Set(s) => run_set(&manifest_path, &s),
        ManifestCmd::Delete(d) => run_delete(&manifest_path, &d),
    }
}

fn run_get(manifest_path: &std::path::Path, args: &GetArgs) -> Result<ExitCode> {
    let root = load_yaml(manifest_path)?;
    let path = parse_path(&args.path).map_err(|e| anyhow!("invalid path: {e}"))?;
    let Some(value) = get_value(&root, &path) else {
        // Missing path. `--json` callers want stable `null` output on
        // stdout (so `jq` doesn't choke) plus the diagnostic on
        // stderr; the human render gets only the diagnostic.
        if args.json {
            println!("null");
        }
        eprintln!(
            "{} path not found in {}: {}",
            ui::glyph_fail_err(),
            redact_path(manifest_path, &project_root_for(manifest_path)),
            args.path,
        );
        return Ok(ExitCode::from(1));
    };

    if args.json {
        // `serde_yaml_ng::Value` → `serde_json::Value` round-trip via
        // its `Serialize` impl. Strings come out unquoted on the YAML
        // side but **quoted** here — that's the whole point of
        // `--json`.
        let json: serde_json::Value =
            serde_json::to_value(value).map_err(|e| anyhow!("could not convert to JSON: {e}"))?;
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(ExitCode::SUCCESS);
    }

    // Human render. Scalar strings come out unquoted (so
    // `pakx manifest get description` is a friendly one-liner);
    // anything else round-trips through `serde_yaml_ng::to_string`
    // which already renders sequences + mappings as block-style YAML.
    match value {
        Value::String(s) => println!("{s}"),
        Value::Bool(b) => println!("{b}"),
        Value::Number(n) => println!("{n}"),
        Value::Null => println!("null"),
        other => {
            let rendered = serde_yaml_ng::to_string(other)
                .map_err(|e| anyhow!("could not render value as YAML: {e}"))?;
            // `to_string` always tacks on a trailing newline; print!
            // (no extra newline) keeps the output one line per value
            // for scripts piping `pakx manifest get foo | wc -l`.
            print!("{rendered}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_set(manifest_path: &std::path::Path, args: &SetArgs) -> Result<ExitCode> {
    let mut root = load_yaml(manifest_path)?;
    let path = parse_path(&args.path).map_err(|e| anyhow!("invalid path: {e}"))?;

    let value = if args.json {
        // Accept a JSON literal; round-trip through serde_yaml_ng so
        // the in-tree value uses the same Value variant the rest of
        // the manifest uses (avoids a JSON-tagged Number where a YAML
        // Number would otherwise live).
        let json: serde_json::Value = serde_json::from_str(&args.value)
            .map_err(|e| anyhow!("--json value is not valid JSON: {e}"))?;
        // `serde_json::Value` → `serde_yaml_ng::Value` via Serialize
        // / Deserialize is the only stable bridge between the two
        // Value types.
        serde_yaml_ng::to_value(&json).map_err(|e| anyhow!("could not convert JSON value: {e}"))?
    } else {
        Value::String(args.value.clone())
    };

    set_value(&mut root, &path, value).map_err(|e| anyhow!("could not set path: {e}"))?;

    // Serialise + atomic-write.
    let body = serde_yaml_ng::to_string(&root)
        .map_err(|e| anyhow!("could not serialise manifest: {e}"))?;
    let bytes = if body.ends_with('\n') {
        body.into_bytes()
    } else {
        let mut b = body.into_bytes();
        b.push(b'\n');
        b
    };
    atomic_write(manifest_path, &bytes).with_context(|| {
        format!(
            "write {}",
            redact_path(manifest_path, &project_root_for(manifest_path))
        )
    })?;

    println!(
        "{} set {} in {}",
        ui::glyph_ok(),
        ui::success(&args.path),
        redact_path(manifest_path, &project_root_for(manifest_path)),
    );
    Ok(ExitCode::SUCCESS)
}

fn run_delete(manifest_path: &std::path::Path, args: &DeleteArgs) -> Result<ExitCode> {
    let mut root = load_yaml(manifest_path)?;
    let path = parse_path(&args.path).map_err(|e| anyhow!("invalid path: {e}"))?;

    let outcome = delete_value(&mut root, &path).map_err(|e| anyhow!("could not delete: {e}"))?;

    match outcome {
        DeleteOutcome::Removed => {
            // Serialise + atomic-write only when there's a real change
            // — a missing-path no-op shouldn't touch the file at all
            // (so mtime stays stable and build systems don't see a
            // spurious re-trigger).
            let body = serde_yaml_ng::to_string(&root)
                .map_err(|e| anyhow!("could not serialise manifest: {e}"))?;
            let bytes = if body.ends_with('\n') {
                body.into_bytes()
            } else {
                let mut b = body.into_bytes();
                b.push(b'\n');
                b
            };
            atomic_write(manifest_path, &bytes).with_context(|| {
                format!(
                    "write {}",
                    redact_path(manifest_path, &project_root_for(manifest_path))
                )
            })?;
            println!(
                "{} removed {} from {}",
                ui::glyph_ok(),
                ui::success(&args.path),
                redact_path(manifest_path, &project_root_for(manifest_path)),
            );
        }
        DeleteOutcome::NotPresent => {
            // Idempotent — warn but succeed. The warning goes to
            // stderr so scripts can pipe stdout through `jq` without
            // it bleeding into the data stream.
            eprintln!(
                "{} {} not present in {}",
                ui::glyph_warn_err(),
                args.path,
                redact_path(manifest_path, &project_root_for(manifest_path)),
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Read + parse the manifest as a raw `serde_yaml_ng::Value`. We do
/// NOT route through the typed `parse_manifest` path because that
/// would reject unknown keys (the schema is `deny_unknown_fields`)
/// and crash on the very forward-compat / sponsor fields `pakx
/// manifest set` exists to let users edit ahead of a schema bump.
fn load_yaml(path: &std::path::Path) -> Result<Value> {
    let body = std::fs::read_to_string(path).with_context(|| {
        format!(
            "read manifest at {}",
            redact_path(path, &project_root_for(path))
        )
    })?;
    serde_yaml_ng::from_str(&body).map_err(|e| {
        anyhow!(
            "could not parse {}: {e}",
            redact_path(path, &project_root_for(path))
        )
    })
}

// `PathSeg` is re-exported for downstream tooling that wants to
// inspect a parsed path without depending on `pakx-core` directly. The
// CLI itself doesn't construct paths by hand.
#[allow(dead_code)]
type _Reexport = PathSeg;
