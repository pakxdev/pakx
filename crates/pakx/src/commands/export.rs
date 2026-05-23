//! `pakx export <id>` — copy an installed package's on-disk tree into a
//! portable folder under the cwd (or `--output <DIR>`).
//!
//! Inverse of `pakx install` from the *consumer* side: install lands a
//! package under `<claude_home>/<subdir>/<owner>-<name>/`, and
//! `pakx export` copies that tree out so the user can ship the folder
//! anywhere (git, archive, USB stick) without re-resolving through the
//! registry. The lockfile entry is the source of truth for *which*
//! version landed on disk — we never hit the network here.
//!
//! Refuses to overwrite an existing output directory unless `--force`
//! (matches `cp -i` / `tar -x` discipline; surprise overwrites of an
//! adjacent project tree would be bad).
//!
//! MCP entries can't be exported via this command because MCP servers
//! aren't extracted into a per-package tree — they live as JSON entries
//! in `.mcp.json`. Asking to export an MCP id is a user-error surface,
//! not a runtime fault, so we exit with a precise error message.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::Args;
use pakx_core::manifest::PackageType;
use pakx_core::{read_lockfile_from, LockEntry};

use crate::redact::redact_path;
use crate::ui;

const LOCKFILE_FILENAME: &str = "agents.lock";

#[derive(Debug, Clone, Args)]
pub struct ExportArgs {
    /// Canonical `<owner>/<name>` of the installed package.
    pub id: String,

    /// Where to write the exported folder. Defaults to
    /// `<cwd>/<name-after-slash>` (e.g. `arwenizEr/hello-world` →
    /// `./hello-world/`).
    #[arg(short = 'o', long = "output", value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Allow the export to overwrite the destination if it already
    /// exists. Without this flag, an existing destination directory is
    /// a hard error — surprise overwrites would clobber a user's
    /// adjacent project.
    #[arg(long)]
    pub force: bool,

    /// Emit a single machine-readable JSON object on stdout describing
    /// the export (`{from, to, files}`). Human progress + warnings
    /// still go to stderr.
    #[arg(long)]
    pub json: bool,

    /// Operate on a project at a path other than the cwd.
    #[arg(short = 'C', long = "directory")]
    pub directory: Option<PathBuf>,

    /// Disambiguator when the requested id is declared under more than
    /// one section of the lockfile (e.g. an id that ships both a skill
    /// and a command). Mirrors the `--kind` flag on `pakx remove` /
    /// `pakx update`.
    #[arg(short = 'k', long = "kind", value_name = "KIND")]
    pub kind: Option<String>,

    /// Override the Claude Code home directory (testing).
    #[arg(long, hide = true)]
    pub claude_home: Option<PathBuf>,
}

/// Wire-format payload emitted by `--json`. Field names are a stable
/// contract — only additive changes are backwards-compatible.
#[derive(Debug, serde::Serialize)]
struct JsonPayload<'a> {
    from: &'a str,
    to: &'a str,
    files: usize,
}

#[allow(clippy::unused_async)] // matches the other commands::*::run signatures
pub async fn run(args: ExportArgs) -> Result<()> {
    if args.json {
        // Force stdout to no-color BEFORE any paint helper memoises a
        // stream decision. Keeps `pakx export --color always --json | jq`
        // byte-clean.
        ui::force_stdout_no_color();
    }

    let project_root = match args.directory.clone() {
        Some(p) => p,
        None => std::env::current_dir().context("cannot read cwd")?,
    };

    let lockfile_path = project_root.join(LOCKFILE_FILENAME);
    let lock = read_lockfile_from(&lockfile_path)
        .with_context(|| format!("read lockfile {}", lockfile_path.display()))?
        .ok_or_else(|| {
            anyhow!("no {LOCKFILE_FILENAME} found — run `pakx install` before exporting")
        })?;

    let entry = pick_entry(&lock.entries, &args.id, args.kind.as_deref())?;

    let claude_home = resolve_claude_home(args.claude_home.as_deref(), &project_root);
    let from = install_path_for(&claude_home, entry)?;
    if !from.exists() {
        bail!(
            "lockfile pins {id}@{version} but its install tree is missing at {path} — \
             run `pakx install` to restore it",
            id = entry.name,
            version = entry.version,
            path = from.display(),
        );
    }

    let to = args
        .output
        .unwrap_or_else(|| project_root.join(default_output_name(&entry.name)));

    if to.exists() {
        if !args.force {
            bail!(
                "destination {} already exists; pass --force to overwrite",
                to.display(),
            );
        }
        // `--force` semantics: wipe the prior tree so we land a clean
        // copy. Matches the install-side `extract_tarball` discipline.
        // We refuse to delete a *file* of the same name — that would
        // be a destructive surprise even with `--force`.
        if to.is_file() {
            bail!(
                "destination {} is a file, not a directory; refusing to delete it even with --force",
                to.display(),
            );
        }
        std::fs::remove_dir_all(&to).with_context(|| format!("clear existing {}", to.display()))?;
    }

    let files = copy_tree(&from, &to)?;

    if args.json {
        let payload = JsonPayload {
            from: &from.display().to_string(),
            to: &to.display().to_string(),
            files,
        };
        let line = serde_json::to_string(&payload).context("serialize export payload")?;
        println!("{line}");
        return Ok(());
    }

    eprintln!(
        "{} exported {} ({} file{}) to {}",
        ui::glyph_ok_err(),
        ui::success_err(&format!("{}@{}", entry.name, entry.version)),
        files,
        if files == 1 { "" } else { "s" },
        // Render relative to the project root when possible so CI logs
        // don't embed the host's absolute path.
        redact_path(&to, &project_root),
    );
    Ok(())
}

/// Resolve the requested id into a single lockfile entry. Mirrors the
/// kind-disambiguation discipline used by `pakx remove` / `pakx update`:
///
/// - Explicit `--kind` wins; we reject the combination if no entry with
///   the requested id-and-kind is present.
/// - Unambiguous shorthand (exactly one match across kinds) auto-picks.
/// - Ambiguous (≥2 matches across kinds) errors out with the rerun
///   hint that names the candidate kinds.
fn pick_entry<'a>(
    entries: &'a std::collections::BTreeMap<String, LockEntry>,
    id: &str,
    explicit_kind: Option<&str>,
) -> Result<&'a LockEntry> {
    let matches: Vec<&LockEntry> = entries.values().filter(|e| e.name == id).collect();
    if matches.is_empty() {
        bail!("{id} is not pinned in {LOCKFILE_FILENAME}");
    }
    if let Some(want) = explicit_kind {
        let want = parse_kind(want)?;
        return matches
            .into_iter()
            .find(|e| e.kind == want)
            .ok_or_else(|| anyhow!("no `{}` entry named `{id}` in lockfile", want.as_str()));
    }
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    let listed: Vec<&str> = matches.iter().map(|e| e.kind.as_str()).collect();
    bail!(
        "{id} is pinned under multiple kinds ({}); rerun with `--kind <{}>`",
        listed.join(", "),
        listed.join("|"),
    )
}

/// CLI-side `--kind` token parser. Mirrors the same six kinds the rest
/// of the CLI recognises so the user gets a clean rejection for
/// typos rather than a falsely-empty match list.
fn parse_kind(s: &str) -> Result<PackageType> {
    match s {
        "skills" => Ok(PackageType::Skills),
        "mcp" => Ok(PackageType::Mcp),
        "subagents" => Ok(PackageType::Subagents),
        "prompts" => Ok(PackageType::Prompts),
        "commands" => Ok(PackageType::Commands),
        "hooks" => Ok(PackageType::Hooks),
        other => bail!(
            "{other:?} is not a valid kind; expected one of \
             skills|mcp|subagents|prompts|commands|hooks"
        ),
    }
}

/// Map a lockfile entry to the on-disk install path used by
/// [`crate::install::skill`] / [`crate::install::bundle`]. The leaf is
/// always `<owner>-<name>` (dashed) and the parent subdir depends on
/// the kind:
///
/// | kind        | subdir       |
/// |-------------|--------------|
/// | `skills`    | `skills/`    |
/// | `subagents` | `agents/`    |
/// | `prompts`   | `prompts/`   |
/// | `commands`  | `commands/`  |
/// | `hooks`     | `hooks/`     |
///
/// MCP entries are intentionally rejected — they live in `.mcp.json`,
/// not in a per-package tree.
fn install_path_for(claude_home: &Path, entry: &LockEntry) -> Result<PathBuf> {
    let subdir = match entry.kind {
        PackageType::Skills => "skills",
        PackageType::Subagents => "agents",
        PackageType::Prompts => "prompts",
        PackageType::Commands => "commands",
        PackageType::Hooks => "hooks",
        PackageType::Mcp => {
            bail!(
                "{id} is an MCP server — MCP entries live in .mcp.json, not in a per-package tree, so they cannot be exported",
                id = entry.name,
            )
        }
    };
    let (owner, name) = entry.name.split_once('/').ok_or_else(|| {
        anyhow!(
            "lockfile entry {id} is not a canonical <owner>/<name>; cannot derive install path",
            id = entry.name,
        )
    })?;
    Ok(claude_home.join(subdir).join(format!("{owner}-{name}")))
}

/// Default output directory: the part of the id after the slash.
/// `arwenizEr/hello-world` → `hello-world`. Matches the spec example.
fn default_output_name(id: &str) -> String {
    id.rsplit_once('/').map_or(id, |(_, name)| name).to_owned()
}

/// Resolve the Claude Code home directory. CLI override wins; otherwise
/// fall back to `dirs::home_dir()/.claude` (production default) or — as
/// a last resort — `<project_root>/.claude` so tests on machines
/// without a home dir still produce a deterministic path. Mirrors
/// `runner::build_claude_adapter`.
fn resolve_claude_home(override_path: Option<&Path>, project_root: &Path) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    dirs::home_dir().map_or_else(|| project_root.join(".claude"), |h| h.join(".claude"))
}

/// Recursively copy `from` → `to`, returning the count of regular files
/// written. Mirrors `cp -R` semantics: directories are created in
/// `to`, file contents are copied byte-for-byte.
///
/// We refuse symlinks (defensive: a malicious install-time write could
/// have planted one). `extract_tarball` already rejects symlink
/// entries, so a live install will never produce one, but a sufficiently
/// motivated attacker who tampered with the install tree post-install
/// could. Failing loud is the right UX.
fn copy_tree(from: &Path, to: &Path) -> Result<usize> {
    std::fs::create_dir_all(to).with_context(|| format!("create {}", to.display()))?;
    let mut count = 0usize;
    let mut stack = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src_dir, dst_dir)) = stack.pop() {
        for entry in std::fs::read_dir(&src_dir)
            .with_context(|| format!("read_dir {}", src_dir.display()))?
        {
            let entry = entry?;
            let ft = entry.file_type()?;
            let src = entry.path();
            let dst = dst_dir.join(entry.file_name());
            if ft.is_symlink() {
                bail!(
                    "refusing to export symlink at {} — install trees should never contain symlinks",
                    src.display(),
                );
            }
            if ft.is_dir() {
                std::fs::create_dir_all(&dst)
                    .with_context(|| format!("create {}", dst.display()))?;
                stack.push((src, dst));
            } else if ft.is_file() {
                std::fs::copy(&src, &dst)
                    .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
                count = count.saturating_add(1);
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pakx_core::{AgentId, Integrity, RegistrySource};

    fn mk_entry(name: &str, kind: PackageType, version: &str) -> LockEntry {
        LockEntry {
            name: name.to_owned(),
            kind,
            version: version.to_owned(),
            resolved_from: format!("pakx:{name}"),
            registry: RegistrySource::Pakx,
            // Fixed valid SRI string so the type contract holds; the
            // exact digest is irrelevant for the unit tests here.
            integrity: Integrity::parse("sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=")
                .unwrap(),
            agents: vec![AgentId::new_unchecked("claude-code")],
            dependencies: vec![],
        }
    }

    #[test]
    fn default_output_name_strips_owner_prefix() {
        assert_eq!(default_output_name("alice/hello-world"), "hello-world");
        assert_eq!(default_output_name("solo"), "solo");
    }

    #[test]
    fn install_path_uses_kind_subdir_and_dashed_leaf() {
        let home = Path::new("/home/u/.claude");
        let entry = mk_entry("alice/hello", PackageType::Skills, "0.1.0");
        assert_eq!(
            install_path_for(home, &entry).unwrap(),
            home.join("skills").join("alice-hello")
        );
        let entry = mk_entry("alice/agent", PackageType::Subagents, "0.1.0");
        assert_eq!(
            install_path_for(home, &entry).unwrap(),
            home.join("agents").join("alice-agent")
        );
        let entry = mk_entry("alice/cmd", PackageType::Commands, "0.1.0");
        assert_eq!(
            install_path_for(home, &entry).unwrap(),
            home.join("commands").join("alice-cmd")
        );
        let entry = mk_entry("alice/hk", PackageType::Hooks, "0.1.0");
        assert_eq!(
            install_path_for(home, &entry).unwrap(),
            home.join("hooks").join("alice-hk")
        );
        let entry = mk_entry("alice/pr", PackageType::Prompts, "0.1.0");
        assert_eq!(
            install_path_for(home, &entry).unwrap(),
            home.join("prompts").join("alice-pr")
        );
    }

    #[test]
    fn install_path_refuses_mcp_kind() {
        let home = Path::new("/home/u/.claude");
        let entry = mk_entry("io.github.acme/srv", PackageType::Mcp, "1.0.0");
        let err = install_path_for(home, &entry).unwrap_err().to_string();
        assert!(err.contains("MCP"), "got: {err}");
    }

    #[test]
    fn pick_entry_finds_single_match() {
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            "skills/alice/hello@0.1.0".to_owned(),
            mk_entry("alice/hello", PackageType::Skills, "0.1.0"),
        );
        let picked = pick_entry(&entries, "alice/hello", None).unwrap();
        assert_eq!(picked.version, "0.1.0");
    }

    #[test]
    fn pick_entry_errors_when_ambiguous_without_kind() {
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            "skills/alice/dual@0.1.0".to_owned(),
            mk_entry("alice/dual", PackageType::Skills, "0.1.0"),
        );
        entries.insert(
            "commands/alice/dual@0.2.0".to_owned(),
            mk_entry("alice/dual", PackageType::Commands, "0.2.0"),
        );
        let err = pick_entry(&entries, "alice/dual", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("multiple kinds"), "got: {err}");
    }

    #[test]
    fn pick_entry_disambiguates_with_explicit_kind() {
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            "skills/alice/dual@0.1.0".to_owned(),
            mk_entry("alice/dual", PackageType::Skills, "0.1.0"),
        );
        entries.insert(
            "commands/alice/dual@0.2.0".to_owned(),
            mk_entry("alice/dual", PackageType::Commands, "0.2.0"),
        );
        let picked = pick_entry(&entries, "alice/dual", Some("commands")).unwrap();
        assert_eq!(picked.kind, PackageType::Commands);
        assert_eq!(picked.version, "0.2.0");
    }

    #[test]
    fn parse_kind_rejects_unknown() {
        assert!(parse_kind("widgets").is_err());
        assert!(parse_kind("skills").is_ok());
    }
}
