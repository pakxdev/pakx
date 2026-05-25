//! `pakx new <kind> <name>` — scaffold a publishable bundle for a kind.
//!
//! `pakx init` writes a *consumer* `agents.yml`; `pakx new` writes a
//! *publisher* bundle — the `SKILL.md` + supporting files an author
//! uploads with `pakx pack` / `pakx publish`. Without it a publisher has
//! to hand-author frontmatter against the Claude Code docs they first
//! have to find. `pakx new skills my-skill` produces a correct starter
//! tree in one command, and the result is guaranteed to pass the
//! per-kind `pakx pack` validation (see `crate::pack::validate_kind_bundle`)
//! with zero warnings.
//!
//! Output modes mirror the rest of the CLI:
//!
//! - **Human (default).** A created-file tree + a `→ next:` hint go to
//!   stderr with the project glyph cadence. Nothing on stdout.
//! - **`--json`.** A single machine-readable object on stdout listing
//!   the created files; progress stays on stderr.
//!
//! `mcp` is intentionally **rejected**: an MCP server is registry
//! *config* (see `pakx add mcp <id>`), not a packable file bundle, so
//! there is nothing honest to scaffold. The error points the user at the
//! right command rather than emitting a stub that `pakx pack` would then
//! have nothing to do with.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::Args;
use inquire::Text;
use pakx_core::atomic_write;
use pakx_core::manifest::PackageType;

use crate::redact::{project_root_for, redact_path};
use crate::ui;

#[derive(Debug, Clone, Args)]
pub struct NewArgs {
    /// Package kind to scaffold: `skills`, `subagents`, `prompts`,
    /// `commands`, or `hooks`. `mcp` is rejected — an MCP server is
    /// registry config, not a file bundle (see `pakx add mcp <id>`).
    pub kind: String,

    /// Bundle name. Also the default output directory (`./<name>/`) and
    /// the `name:` field written into the generated frontmatter. Must be
    /// lowercase ASCII + `.`/`_`/`-` (the registry's package-name rule).
    pub name: String,

    /// One-line description embedded in the generated frontmatter. When
    /// omitted, an interactive prompt asks for it (unless `--yes`); a
    /// non-empty placeholder is written so the bundle never trips the
    /// per-kind `pakx pack` validation.
    #[arg(short = 'd', long)]
    pub description: Option<String>,

    /// Write the bundle somewhere other than `./<name>/`.
    #[arg(short = 'o', long = "output", alias = "dir")]
    pub output: Option<PathBuf>,

    /// Overwrite files in a non-empty target directory.
    #[arg(short = 'f', long)]
    pub force: bool,

    /// Skip the interactive description prompt and take a default
    /// placeholder when `--description` is not supplied.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Emit a single machine-readable JSON object on stdout listing the
    /// created files. Progress lines still go to stderr.
    #[arg(long)]
    pub json: bool,
}

/// Default semver written into every scaffolded bundle's frontmatter.
const DEFAULT_VERSION: &str = "0.1.0";

#[allow(clippy::unused_async)] // matches the other commands::*::run signatures
pub async fn run(args: NewArgs) -> Result<()> {
    let kind = parse_scaffold_kind(&args.kind)?;

    // Validate the name against the same rule `pakx pack` enforces on the
    // SKILL.md `name:` so a freshly-scaffolded bundle never fails pack on
    // a name the registry would reject anyway.
    validate_name(&args.name)?;

    let cwd = env::current_dir().context("cannot read current working directory")?;
    let target = args.output.clone().unwrap_or_else(|| cwd.join(&args.name));

    ensure_target_free(&target, args.force)?;

    // `pick_description` prompts for a one-line description when neither
    // `--description` nor `--yes` is supplied. Fail fast if that prompt
    // would have no TTY to read from rather than blocking forever.
    if args.description.is_none() {
        ui::ensure_interactive(args.yes, "scaffold the bundle")?;
    }
    let description = pick_description(args.description.clone(), &args.name, args.yes)?;

    let files = templates_for(kind, &args.name, &description);

    // Create the target dir + every nested parent before writing. The
    // `atomic_write` helper requires the parent dir to exist (it writes a
    // sibling `.tmp` then renames), so we materialise the directory tree
    // up front.
    std::fs::create_dir_all(&target)
        .with_context(|| format!("create {}", redact_path(&target, &cwd)))?;

    let project_root = project_root_for(&target);
    let mut written: Vec<String> = Vec::with_capacity(files.len());
    for file in &files {
        let path = target.join(&file.relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", redact_path(parent, &project_root)))?;
        }
        atomic_write(&path, file.contents.as_bytes())
            .with_context(|| format!("write {}", redact_path(&path, &project_root)))?;
        written.push(file.relative.clone());
    }
    // Stable, deterministic ordering so the JSON + human output don't
    // depend on the order the template table happened to be authored in.
    written.sort();

    if args.json {
        emit_json(kind, &args.name, &target, &written);
        return Ok(());
    }

    emit_human(kind, &args.name, &target, &written);
    Ok(())
}

/// The five scaffoldable kinds. `mcp` is deliberately excluded — see the
/// module doc — and any non-kind token is rejected with the same list of
/// valid kinds `pakx add` reports.
fn parse_scaffold_kind(s: &str) -> Result<PackageType> {
    match s {
        "skills" => Ok(PackageType::Skills),
        "subagents" => Ok(PackageType::Subagents),
        "prompts" => Ok(PackageType::Prompts),
        "commands" => Ok(PackageType::Commands),
        "hooks" => Ok(PackageType::Hooks),
        "mcp" => Err(anyhow!(
            "mcp packages are configured in agents.yml, not scaffolded \u{2014} \
             an MCP server is registry config, not a packable file bundle. \
             Use `pakx add mcp <id>` to declare one (see the official MCP Registry)."
        )),
        other => Err(anyhow!(
            "'{other}' is not a scaffoldable kind; expected one of \
             skills|subagents|prompts|commands|hooks \
             (mcp is config, not a bundle \u{2014} see `pakx add mcp <id>`)"
        )),
    }
}

/// A single file the scaffold will write, addressed relative to the
/// target dir. Forward-slash separators in `relative` are split back into
/// path components at write time so nested files (`reference/x.md`) land
/// correctly on every platform.
struct TemplateFile {
    relative: String,
    contents: String,
}

/// Build the file set for `kind`. Every template that supports a
/// `description:` embeds the caller's description verbatim so the
/// generated bundle satisfies `crate::pack::validate_kind_bundle` with
/// zero warnings the moment it's written.
fn templates_for(kind: PackageType, name: &str, description: &str) -> Vec<TemplateFile> {
    match kind {
        PackageType::Skills => skills_templates(name, description),
        PackageType::Subagents => subagents_templates(name, description),
        PackageType::Prompts => prompts_templates(name, description),
        PackageType::Commands => commands_templates(name, description),
        PackageType::Hooks => hooks_templates(name, description),
        // Unreachable: `parse_scaffold_kind` rejects mcp before we ever
        // reach the template table. Kept exhaustive so a future kind
        // addition forces a compile error here rather than silently
        // scaffolding nothing.
        PackageType::Mcp => Vec::new(),
    }
}

/// skills: a `SKILL.md` with `name:` + `version:` + `description:`
/// frontmatter. The `description:` is what Claude Code reads to decide
/// when to load the skill — its presence is exactly what the skills pack
/// check guards on.
fn skills_templates(name: &str, description: &str) -> Vec<TemplateFile> {
    let desc = yaml_scalar(description);
    let skill_md = format!(
        "---\n\
         name: {name}\n\
         version: {DEFAULT_VERSION}\n\
         kind: skills\n\
         description: {desc}\n\
         ---\n\
         \n\
         # {name}\n\
         \n\
         Describe what this skill does and when an agent should reach for it.\n\
         \n\
         ## Usage\n\
         \n\
         Document the steps, inputs, and outputs here.\n"
    );
    vec![
        TemplateFile {
            relative: "SKILL.md".to_string(),
            contents: skill_md,
        },
        TemplateFile {
            relative: "README.md".to_string(),
            contents: readme_template(name, description, "skill"),
        },
    ]
}

/// subagents: a markdown file whose frontmatter carries BOTH a kebab-case
/// `name:` and a `description:` — exactly what the subagents pack check
/// scans for. The scaffold name is already validated lowercase, so we
/// only need to map `_`/`.` to `-` to guarantee kebab-case.
fn subagents_templates(name: &str, description: &str) -> Vec<TemplateFile> {
    let agent_name = to_kebab_case(name);
    let desc = yaml_scalar(description);
    let skill_md = format!(
        "---\n\
         name: {agent_name}\n\
         version: {DEFAULT_VERSION}\n\
         kind: subagents\n\
         description: {desc}\n\
         ---\n\
         \n\
         # {agent_name}\n\
         \n\
         You are a focused sub-agent. State the system prompt that governs\n\
         this agent's behaviour below.\n\
         \n\
         ## Responsibilities\n\
         \n\
         - Describe what this sub-agent is responsible for.\n"
    );
    vec![
        TemplateFile {
            relative: "SKILL.md".to_string(),
            contents: skill_md,
        },
        TemplateFile {
            relative: "README.md".to_string(),
            contents: readme_template(name, description, "sub-agent"),
        },
    ]
}

/// commands: a markdown file with a `description:` frontmatter — Claude
/// Code shows it in the slash-command menu, and it's what the commands
/// pack check looks for.
fn commands_templates(name: &str, description: &str) -> Vec<TemplateFile> {
    let desc = yaml_scalar(description);
    let skill_md = format!(
        "---\n\
         name: {name}\n\
         version: {DEFAULT_VERSION}\n\
         kind: commands\n\
         description: {desc}\n\
         ---\n\
         \n\
         # /{name}\n\
         \n\
         Document the slash command's prompt body here. Reference\n\
         arguments with `$ARGUMENTS` or `$1`, `$2`, ... as needed.\n"
    );
    vec![
        TemplateFile {
            relative: "SKILL.md".to_string(),
            contents: skill_md,
        },
        TemplateFile {
            relative: "README.md".to_string(),
            contents: readme_template(name, description, "command"),
        },
    ]
}

/// prompts: at least one non-empty prompt file alongside the manifest.
/// The prompts pack check explicitly ignores `SKILL.md` (its frontmatter
/// is always non-empty), so the scaffold ships a real `prompt.md` with
/// content.
fn prompts_templates(name: &str, description: &str) -> Vec<TemplateFile> {
    let desc = yaml_scalar(description);
    let skill_md = format!(
        "---\n\
         name: {name}\n\
         version: {DEFAULT_VERSION}\n\
         kind: prompts\n\
         description: {desc}\n\
         ---\n\
         \n\
         # {name}\n\
         \n\
         A reusable prompt bundle. The prompt body lives in `prompt.md`.\n"
    );
    let prompt_md = format!(
        "# {name}\n\
         \n\
         {description}\n\
         \n\
         Replace this with the prompt text the agent should run. Use\n\
         placeholders like `{{input}}` for values the caller fills in.\n"
    );
    vec![
        TemplateFile {
            relative: "SKILL.md".to_string(),
            contents: skill_md,
        },
        TemplateFile {
            relative: "prompt.md".to_string(),
            contents: prompt_md,
        },
        TemplateFile {
            relative: "README.md".to_string(),
            contents: readme_template(name, description, "prompt"),
        },
    ]
}

/// hooks: a bundle that declares a recognised hook event + matcher shape.
/// The hooks pack check substring-scans every file for one of the known
/// event names, so the scaffold ships a `hooks.json` declaring a
/// `PreToolUse` hook on the `Bash` matcher.
fn hooks_templates(name: &str, description: &str) -> Vec<TemplateFile> {
    let desc = yaml_scalar(description);
    let skill_md = format!(
        "---\n\
         name: {name}\n\
         version: {DEFAULT_VERSION}\n\
         kind: hooks\n\
         description: {desc}\n\
         ---\n\
         \n\
         # {name}\n\
         \n\
         A Claude Code hook bundle. The hook configuration lives in\n\
         `hooks.json`; point Claude Code at it from your settings.\n"
    );
    // A valid hooks config declaring a PreToolUse event on the Bash
    // matcher. The `command` is a harmless no-op placeholder the author
    // replaces. Hand-written with two-space indent so it reads cleanly in
    // the generated bundle.
    let hooks_json = "{\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\n        \"matcher\": \"Bash\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"echo replace-me\"\n          }\n        ]\n      }\n    ]\n  }\n}\n"
        .to_string();
    vec![
        TemplateFile {
            relative: "SKILL.md".to_string(),
            contents: skill_md,
        },
        TemplateFile {
            relative: "hooks.json".to_string(),
            contents: hooks_json,
        },
        TemplateFile {
            relative: "README.md".to_string(),
            contents: readme_template(name, description, "hook"),
        },
    ]
}

/// Render `s` as a YAML double-quoted scalar safe to drop after a
/// `key:` in the generated frontmatter. A user-supplied (or default)
/// description can contain a colon (`fixes the foo: bar case`), a `#`, a
/// leading `-`, or other chars that break a plain YAML scalar and make
/// `pakx pack` reject the SKILL.md as "not valid YAML". Double-quoting
/// and escaping `\` + `"` sidesteps every plain-scalar ambiguity in one
/// shot — YAML double-quoted strings only need those two escapes for the
/// printable input a description ever carries.
fn yaml_scalar(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Shared README body. `kind_label` is the human-readable singular noun
/// ("skill", "sub-agent", ...) used in the prose.
fn readme_template(name: &str, description: &str, kind_label: &str) -> String {
    format!(
        "# {name}\n\
         \n\
         {description}\n\
         \n\
         This is a pakx {kind_label} bundle. To pack and publish it:\n\
         \n\
         ```sh\n\
         pakx pack\n\
         pakx publish\n\
         ```\n"
    )
}

/// Map a registry-valid package name (lowercase ASCII + `.`/`_`/`-`) onto
/// a kebab-case sub-agent name: `_` and `.` collapse to `-`, runs of `-`
/// collapse to one, and leading/trailing `-` are trimmed. The input is
/// already lowercase ASCII (enforced by `validate_name`), so this is a
/// pure separator normalisation. Falls back to `"agent"` if the input
/// reduces to empty (e.g. a name of only separators).
fn to_kebab_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = true; // suppress a leading dash
    for c in name.chars() {
        if c == '_' || c == '.' || c == '-' {
            if !last_was_dash {
                out.push('-');
                last_was_dash = true;
            }
        } else {
            out.push(c);
            last_was_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "agent".to_string()
    } else {
        out
    }
}

/// Same name rule `pakx pack` enforces on the SKILL.md `name:` (see
/// `crate::pack::validate_name`). Duplicated here rather than imported so
/// the scaffold rejects a bad name before writing any files — a name the
/// registry would reject is one the author should never get a bundle for.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 {
        bail!("name must be 1-128 chars, got {} chars", name.len());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
    {
        bail!("name {name:?} must be lowercase ASCII + `.`/`_`/`-` only (registry rule)");
    }
    Ok(())
}

/// Resolve the description: explicit `--description` wins, then an
/// interactive prompt (unless `--yes`), then a non-empty placeholder. The
/// placeholder matters — an empty description would trip the skills /
/// subagents / commands pack checks, defeating the whole point of the
/// scaffold.
fn pick_description(supplied: Option<String>, name: &str, yes: bool) -> Result<String> {
    if let Some(d) = supplied {
        let trimmed = d.trim();
        if trimmed.is_empty() {
            return Ok(default_description(name));
        }
        return Ok(trimmed.to_string());
    }
    if yes {
        return Ok(default_description(name));
    }
    let answer = Text::new("One-line description?")
        .with_default(&default_description(name))
        .prompt()
        .map_err(|e| anyhow!("prompt failed: {e}"))?;
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        Ok(default_description(name))
    } else {
        Ok(trimmed.to_string())
    }
}

/// Non-empty placeholder description. Phrased as a TODO so the author
/// knows to replace it, but still satisfies every per-kind pack check
/// (all of which only require the field to be *present and non-empty*).
fn default_description(name: &str) -> String {
    format!("TODO: describe what {name} does")
}

/// Fail when the target dir already holds files, unless `--force`. An
/// empty dir (or a missing one) is fine — that's the common case where a
/// user `mkdir`'d first. Mirrors the refuse-then-`--force` discipline of
/// `pakx init`.
fn ensure_target_free(target: &Path, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    let Ok(mut entries) = std::fs::read_dir(target) else {
        // Missing dir (or unreadable) → nothing to clobber; the write
        // path creates it. An unreadable existing dir surfaces its own
        // error at `create_dir_all` time with a clearer message.
        return Ok(());
    };
    if entries.next().is_some() {
        bail!(
            "target {} is not empty; pass --force to scaffold into it anyway",
            target.display()
        );
    }
    Ok(())
}

/// Human output: the created-file tree + a `→ next:` hint, both on
/// stderr (stdout stays reserved for the `--json` payload, consistent
/// with `pakx pack`).
fn emit_human(kind: PackageType, name: &str, target: &Path, written: &[String]) {
    eprintln!(
        "{} scaffolded {} ({}) in {}",
        ui::glyph_ok_err(),
        ui::success_err(name),
        kind.as_str(),
        target.display(),
    );
    for rel in written {
        eprintln!("  {rel}");
    }
    // Single dimmed next-step hint, matching the action-command cadence.
    eprintln!(
        "{}",
        ui::dim_err(&format!("\u{2192} next: cd {name} && pakx pack"))
    );
}

/// JSON output: a single newline-terminated object on stdout. Field
/// names are a stable camelCase contract (`ok`, `kind`, `name`, `dir`,
/// `files`) consistent with `pakx pack --json`.
fn emit_json(kind: PackageType, name: &str, target: &Path, written: &[String]) {
    crate::ui::force_stdout_no_color();
    let payload = serde_json::json!({
        "ok": true,
        "kind": kind.as_str(),
        "name": name,
        "dir": target.display().to_string(),
        "files": written,
    });
    let line = serde_json::to_string(&payload).expect("serialize new json");
    println!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scaffold_kind_accepts_five_bundle_kinds() {
        assert_eq!(parse_scaffold_kind("skills").unwrap(), PackageType::Skills);
        assert_eq!(
            parse_scaffold_kind("subagents").unwrap(),
            PackageType::Subagents
        );
        assert_eq!(
            parse_scaffold_kind("prompts").unwrap(),
            PackageType::Prompts
        );
        assert_eq!(
            parse_scaffold_kind("commands").unwrap(),
            PackageType::Commands
        );
        assert_eq!(parse_scaffold_kind("hooks").unwrap(), PackageType::Hooks);
    }

    #[test]
    fn parse_scaffold_kind_rejects_mcp_with_pointer() {
        let err = parse_scaffold_kind("mcp").unwrap_err().to_string();
        assert!(err.contains("pakx add mcp"), "got: {err}");
        assert!(err.contains("config"), "got: {err}");
    }

    #[test]
    fn parse_scaffold_kind_rejects_unknown() {
        let err = parse_scaffold_kind("widgets").unwrap_err().to_string();
        assert!(err.contains("not a scaffoldable kind"), "got: {err}");
    }

    #[test]
    fn to_kebab_case_normalises_separators() {
        assert_eq!(to_kebab_case("code-reviewer"), "code-reviewer");
        assert_eq!(to_kebab_case("code_reviewer"), "code-reviewer");
        assert_eq!(to_kebab_case("my.cool.agent"), "my-cool-agent");
        assert_eq!(to_kebab_case("a__b"), "a-b");
        assert_eq!(to_kebab_case("-trim-"), "trim");
        assert_eq!(to_kebab_case("___"), "agent");
    }

    #[test]
    fn validate_name_rejects_uppercase_and_spaces() {
        assert!(validate_name("Good").is_err());
        assert!(validate_name("has space").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("ok-name_1.2").is_ok());
    }

    #[test]
    fn default_description_is_nonempty() {
        assert!(!default_description("x").trim().is_empty());
    }

    #[test]
    fn yaml_scalar_quotes_and_escapes() {
        // A bare colon would break a plain YAML scalar; quoting fixes it.
        assert_eq!(
            yaml_scalar("fixes the foo: bar case"),
            "\"fixes the foo: bar case\""
        );
        // Embedded quotes + backslashes are escaped.
        assert_eq!(yaml_scalar(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(yaml_scalar(r"a\b"), r#""a\\b""#);
        // The double-quoted output must parse back to the original via a
        // YAML reader (round-trip sanity for the frontmatter we emit).
        let yaml = format!("description: {}\n", yaml_scalar("a: b # c"));
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(v["description"].as_str(), Some("a: b # c"));
    }
}
