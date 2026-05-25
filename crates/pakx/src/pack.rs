//! Pack a local skill directory into a gzipped tarball.
//!
//! Conventions:
//!   - Input is a directory containing at least `SKILL.md` with YAML
//!     frontmatter `name:` and `version:` keys.
//!   - Output is `<name>-<version>.tgz` written to the caller-chosen
//!     output dir.
//!   - Tar entries are sorted alphabetically so two builds of the same
//!     source produce byte-identical tarballs (modulo gzip timestamps,
//!     which we zero out below).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use pakx_core::{validate_sponsors, Sponsor};
use serde::Deserialize;

use crate::redact::redact_path;

const SKILL_MD: &str = "SKILL.md";
const README_MD: &str = "README.md";
const MAX_TARBALL_BYTES: u64 = 50 * 1024 * 1024;
/// Hard cap on the README markdown captured from the bundle's
/// `README.md`. 256 KiB matches the registry-side `PublishBody.readme`
/// cap (`packages.readme` in pakx-registry); a larger source is
/// truncated with a warning rather than packed. NOT a hard error: the
/// publisher can ship the bundle regardless and trim later.
const README_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    /// Declared package kind (`skills`, `mcp`, `subagents`, `prompts`,
    /// `commands`, `hooks`). When the SKILL.md frontmatter omits the
    /// field, defaults to `"skills"` — the historical implicit kind for
    /// every SKILL.md bundle. Surfacing it here lets `pakx pack --json`
    /// emit the actual declared kind on the stable wire-contract instead
    /// of unconditionally claiming `"skills"`.
    pub kind: String,
    /// Sponsor links declared in the SKILL.md frontmatter, validated at
    /// pack-time against the shape in
    /// `pakx-registry/SPONSOR_LINKS_SPEC.md`. Empty when the field is
    /// missing; never `None` so downstream callers don't need to branch
    /// between "no field" and "empty list".
    pub sponsors: Vec<Sponsor>,
    /// Long-form README markdown captured at pack time from
    /// `<src>/README.md` when present. `None` when the bundle omits a
    /// README. Forwarded by `pakx publish` to the registry so the
    /// pakx-web `/p/*` detail page can render it. Capped at
    /// `README_MAX_BYTES` (256 KiB) — oversize sources are truncated
    /// with a warning so the publish itself still succeeds.
    pub readme: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PackOutput {
    pub manifest: Manifest,
    pub tarball_path: PathBuf,
    pub bytes: Vec<u8>,
    /// Non-fatal advisories surfaced by `read_manifest`. Currently:
    /// missing `description:` in the SKILL.md frontmatter (skills
    /// packages only). Callers (`pakx pack`, `pakx publish`) decide
    /// whether to print to stderr — the pack itself still succeeds.
    pub warnings: Vec<String>,
}

/// Pack `src_dir` into a `.tgz` written to `out_dir`. Returns both the
/// on-disk path and the in-memory bytes (for `pakx publish` which
/// uploads without re-reading the file).
pub fn pack_dir(src_dir: &Path, out_dir: &Path) -> Result<PackOutput> {
    // CI logs / pasted error output would otherwise embed the full
    // host-absolute paths to `src_dir` / `out_dir` (typically a
    // `C:\Users\<name>\…` or `/home/runner/…` tempdir). Render paths
    // relative to the cwd when possible, and fall back to the basename
    // when they live outside the project root.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (manifest, warnings) = read_manifest(src_dir, &cwd)?;
    let tarball_name = format!("{}-{}.tgz", manifest.name, manifest.version);
    let tarball_path = out_dir.join(&tarball_name);

    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("create output dir {}", redact_path(out_dir, &cwd)))?;

    let bytes = build_tarball(src_dir, &cwd)?;
    if bytes.len() as u64 > MAX_TARBALL_BYTES {
        bail!(
            "tarball {} bytes exceeds {} byte limit",
            bytes.len(),
            MAX_TARBALL_BYTES
        );
    }

    // Direct `File::create` (not `atomic_write`) — pack always writes a
    // fresh tarball into a caller-supplied out_dir; there is no prior
    // version to lose on crash, and atomic_write's `<name>.tmp` side
    // channel collides under parallel `cargo test` when multiple test
    // fixtures share the same `<name>-<version>` shorthand.
    let mut out = File::create(&tarball_path)
        .with_context(|| format!("create {}", redact_path(&tarball_path, &cwd)))?;
    out.write_all(&bytes)
        .with_context(|| format!("write {}", redact_path(&tarball_path, &cwd)))?;

    Ok(PackOutput {
        manifest,
        tarball_path,
        bytes,
        warnings,
    })
}

/// Per-entry summary for `pakx pack --dry-run --json`. Mirrors what
/// would end up in the tarball but without compressing / writing the
/// `.tgz`. The `path` is the relative-in-archive form (always forward
/// slashes); `size_bytes` is the **uncompressed** payload length so a
/// pipeline can sum the total decompressed size without reading the
/// tarball back.
#[derive(Debug, Clone)]
pub struct DryRunEntry {
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct DryRunOutput {
    pub manifest: Manifest,
    pub warnings: Vec<String>,
    pub entries: Vec<DryRunEntry>,
}

/// Read the SKILL.md frontmatter + enumerate every file that would be
/// packed, WITHOUT building or writing the tarball. The entry list is
/// sorted alphabetically by path so two dry-runs over the same source
/// produce identical output — same discipline `build_tarball` uses for
/// its own tar entries.
pub fn dry_run_dir(src_dir: &Path) -> Result<DryRunOutput> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (manifest, warnings) = read_manifest(src_dir, &cwd)?;
    let mut files = collect_files(src_dir, &cwd)?;
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    let entries = files
        .into_iter()
        .map(|f| DryRunEntry {
            size_bytes: f.contents.len() as u64,
            path: f.relative,
        })
        .collect();
    Ok(DryRunOutput {
        manifest,
        warnings,
        entries,
    })
}

fn read_manifest(src_dir: &Path, project_root: &Path) -> Result<(Manifest, Vec<String>)> {
    let skill_md = src_dir.join(SKILL_MD);
    let text = std::fs::read_to_string(&skill_md)
        .with_context(|| format!("read {}", redact_path(&skill_md, project_root)))?;
    let frontmatter = extract_frontmatter(&text)?;
    let name = frontmatter
        .name
        .ok_or_else(|| anyhow!("{SKILL_MD} frontmatter missing `name:`"))?;
    let version = frontmatter
        .version
        .ok_or_else(|| anyhow!("{SKILL_MD} frontmatter missing `version:`"))?;
    validate_name(&name)?;
    let sponsors = frontmatter.sponsors.unwrap_or_default();
    validate_sponsors(&sponsors)
        .map_err(|e| anyhow!("{SKILL_MD} sponsor-link validation failed: {e}"))?;
    let mut warnings = Vec::new();
    // Default to `"skills"` when the SKILL.md frontmatter omits an
    // explicit `kind:` — historically every SKILL.md bundle is a
    // skills package and the existing JSON contract is `"kind": "skills"`.
    // A publisher who packs a non-skills bundle can override by adding
    // `kind: mcp` (or one of the other five known kinds) to the
    // frontmatter; unrecognised strings fall through verbatim so the
    // registry validates them server-side.
    let kind = frontmatter
        .kind
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "skills".to_string());
    // Per-kind bundle validation. Each kind has a Claude Code spec
    // (skills / sub-agents / slash-commands / hooks); we warn — never
    // hard-error — when the bundle is missing the field Claude Code
    // needs to load it, so technically-malformed bundles still pack for
    // local smoke / air-gapped uploads (mirrors the round-32 skills
    // `description:` warning). MCP is config, not a file bundle, so it
    // has no pack-time file checks.
    validate_kind_bundle(
        &kind,
        src_dir,
        frontmatter.description.as_deref(),
        &mut warnings,
    );
    // Optionally capture the bundle's `README.md`. Absent file → `None`
    // (every bundle prior to this round shipped without one, and the
    // registry column is nullable). Oversize source → truncate to
    // `README_MAX_BYTES` and emit a non-fatal warning so the publisher
    // notices but the publish still goes through. Read failures (other
    // than "not found") bail — a permission error on README.md is the
    // kind of surprise the publisher needs to see immediately, not
    // silently dropped.
    let readme = load_readme(src_dir, project_root, &mut warnings)?;
    Ok((
        Manifest {
            name,
            version,
            kind,
            sponsors,
            readme,
        },
        warnings,
    ))
}

/// Claude Code documentation URLs cited inline in the per-kind pack
/// warnings so a publisher knows the authoritative source for the field
/// they're missing.
const SKILLS_DOC_URL: &str = "https://code.claude.com/docs/en/skills";
const SUBAGENTS_DOC_URL: &str = "https://code.claude.com/docs/en/sub-agents";
const COMMANDS_DOC_URL: &str = "https://code.claude.com/docs/en/slash-commands";
const HOOKS_DOC_URL: &str = "https://code.claude.com/docs/en/hooks";

/// Known Claude Code hook event names. A `hooks` bundle declares at
/// least one of these (typically inside a `hooks:` block in
/// `settings.json` / a hook config file) so Claude Code knows when to
/// fire the hook. See <https://code.claude.com/docs/en/hooks>.
const HOOK_EVENTS: [&str; 8] = [
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "Notification",
    "Stop",
    "SubagentStop",
    "PreCompact",
    "SessionStart",
];

/// Run the per-kind, pack-time bundle validation and append any advisory
/// to `warnings`. Never errors — a publisher must always be able to pack
/// a technically-malformed bundle for local smoke / air-gapped uploads
/// (same discipline as the round-32 skills `description:` warning). The
/// `kind` selects which checks run; `skill_description` is the SKILL.md
/// frontmatter `description:` already parsed by the caller.
///
/// Unknown kinds (anything outside the six known `PackageType`s) are
/// left to the registry to validate server-side, so they get no
/// pack-time check here.
fn validate_kind_bundle(
    kind: &str,
    src_dir: &Path,
    skill_description: Option<&str>,
    warnings: &mut Vec<String>,
) {
    let has_description = skill_description
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    // Each arm guards on the *missing-field* condition: when the field
    // is present (guard is false) the arm doesn't match and control
    // falls through to `_ => {}` — i.e. no warning. `mcp` is server
    // config, not a file bundle, so it never warns; unknown kinds fall
    // through to registry-side validation.
    match kind {
        // Claude Code reads the SKILL.md frontmatter `description` field
        // at discovery time to decide whether to load a skill at all, so
        // a missing description ships a package that is effectively
        // dead-on-arrival inside Claude Code.
        "skills" if !has_description => {
            warnings.push(format!(
                "{SKILL_MD} is missing `description:` \u{2014} Claude Code uses this field to decide when to load the skill; consider adding one (see {SKILLS_DOC_URL})."
            ));
        }
        // Claude Code sub-agents require YAML frontmatter with both
        // `name:` (kebab-case) and `description:`; without them the agent
        // file is unusable. We scan the bundle's markdown for a file
        // carrying both. SKILL.md (the pack entry point) is the natural
        // candidate, but any `.md` in the bundle counts.
        "subagents" if !bundle_has_subagent_frontmatter(src_dir) => {
            warnings.push(format!(
                "subagent bundle has no markdown with both `name:` (kebab-case) and `description:` frontmatter \u{2014} Claude Code sub-agents require both (see {SUBAGENTS_DOC_URL})."
            ));
        }
        // Slash-command files use frontmatter `description:` to show a
        // one-line summary in the slash-command menu. Optional but
        // recommended — warn if no command markdown declares one.
        "commands" if !bundle_has_markdown_with_description(src_dir) => {
            warnings.push(format!(
                "command bundle has no markdown with a `description:` frontmatter \u{2014} Claude Code shows it in the slash-command menu; consider adding one (see {COMMANDS_DOC_URL})."
            ));
        }
        // No public frontmatter spec for prompts; keep it light — just
        // warn when the bundle ships nothing but empty files.
        "prompts" if !bundle_has_nonempty_file(src_dir) => {
            warnings.push(
                "prompt bundle has no non-empty file \u{2014} a prompt bundle should ship at least one prompt file with content.".to_string(),
            );
        }
        // Hooks need an event (PreToolUse, PostToolUse, …) so Claude Code
        // knows when to fire them. Best-effort: scan the bundle for any
        // known event name; warn if none is found.
        "hooks" if !bundle_declares_hook_event(src_dir) => {
            warnings.push(format!(
                "hooks bundle declares no recognised hook event ({}) \u{2014} Claude Code hooks need an event + matcher (see {HOOKS_DOC_URL}).",
                HOOK_EVENTS.join(", ")
            ));
        }
        _ => {}
    }
}

/// Read every regular file directly under `src_dir` (recursively) whose
/// name ends in `.md`, returning `(relative_name, contents)` pairs.
/// Best-effort: unreadable files are skipped (validation is advisory,
/// never fatal). Skips the same noise dirs `collect_files` skips.
fn read_markdown_files(src_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if matches!(name, ".git" | "node_modules" | "target" | "__pycache__") {
                        continue;
                    }
                }
                stack.push(path);
            } else if ft.is_file() {
                let is_md = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"));
                if is_md {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        out.push(text);
                    }
                }
            }
        }
    }
    out
}

/// True when at least one markdown file in the bundle has YAML
/// frontmatter declaring BOTH a kebab-case `name:` and a non-empty
/// `description:`. Used by the `subagents` pack check.
fn bundle_has_subagent_frontmatter(src_dir: &Path) -> bool {
    read_markdown_files(src_dir).iter().any(|text| {
        extract_frontmatter(text).is_ok_and(|fm| {
            let name_ok = fm.name.as_deref().map(str::trim).is_some_and(is_kebab_case);
            let desc_ok = fm
                .description
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty());
            name_ok && desc_ok
        })
    })
}

/// True when at least one markdown file in the bundle has a non-empty
/// `description:` in its frontmatter. Used by the `commands` pack check.
fn bundle_has_markdown_with_description(src_dir: &Path) -> bool {
    read_markdown_files(src_dir).iter().any(|text| {
        extract_frontmatter(text).is_ok_and(|fm| {
            fm.description
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
        })
    })
}

/// True when the bundle ships at least one prompt file with
/// non-whitespace content. The top-level `SKILL.md` (the pack manifest
/// itself, always present and always carrying frontmatter) is NOT
/// counted — otherwise every prompt bundle would trivially pass on the
/// manifest alone. A prompt bundle must ship actual prompt content
/// alongside the manifest. Used by the `prompts` pack check.
fn bundle_has_nonempty_file(src_dir: &Path) -> bool {
    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let is_root = dir == src_dir;
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if matches!(name, ".git" | "node_modules" | "target" | "__pycache__") {
                        continue;
                    }
                }
                stack.push(path);
            } else if ft.is_file() {
                // Skip the top-level pack manifest — its frontmatter is
                // always non-empty and would mask a content-less bundle.
                if is_root
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == SKILL_MD)
                {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if meta.len() == 0 {
                        continue;
                    }
                }
                // Non-empty by byte length, but a file of only
                // whitespace is effectively empty for a prompt. Read
                // and check for any non-whitespace char.
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if !text.trim().is_empty() {
                        return true;
                    }
                } else {
                    // Non-UTF8 binary file with bytes counts as content.
                    return true;
                }
            }
        }
    }
    false
}

/// True when any file in the bundle mentions a recognised Claude Code
/// hook event name. Best-effort substring scan over file contents (hook
/// configs live in JSON / settings files, not markdown, so we read all
/// regular files, not just `.md`). Used by the `hooks` pack check.
fn bundle_declares_hook_event(src_dir: &Path) -> bool {
    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if matches!(name, ".git" | "node_modules" | "target" | "__pycache__") {
                        continue;
                    }
                }
                stack.push(path);
            } else if ft.is_file() {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if HOOK_EVENTS.iter().any(|ev| text.contains(ev)) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Lightweight kebab-case check for sub-agent `name:` frontmatter:
/// lowercase ASCII letters / digits separated by single hyphens, no
/// leading / trailing / doubled hyphens. Matches the Claude Code
/// sub-agent naming convention.
fn is_kebab_case(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.starts_with('-') || s.ends_with('-') || s.contains("--") {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Read `<src_dir>/README.md`, truncate to `README_MAX_BYTES` if needed,
/// and surface a warning on truncation. Returns `Ok(None)` when the
/// file does not exist (the historical zero-README behavior) and `Err`
/// on any other I/O failure (permission denied, unreadable bytes) so
/// the publisher sees the problem before upload instead of silently
/// shipping a no-README bundle.
fn load_readme(
    src_dir: &Path,
    project_root: &Path,
    warnings: &mut Vec<String>,
) -> Result<Option<String>> {
    let path = src_dir.join(README_MD);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("read {}", redact_path(&path, project_root)));
        }
    };
    // Decode as UTF-8 with `from_utf8_lossy` — a README with invalid
    // bytes (a stray BOM-less Latin-1 paste, a binary file mistakenly
    // named README.md) becomes a string with replacement chars rather
    // than a hard failure. The registry stores text anyway, so this
    // keeps the publish surface friendly while the publisher fixes
    // the source. `into_owned` so we don't carry a Cow through the
    // Manifest.
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    if decoded.len() > README_MAX_BYTES {
        warnings.push(format!(
            "{README_MD} is {} bytes; truncating to {README_MAX_BYTES} bytes (registry cap)",
            decoded.len()
        ));
        // Truncate at a char boundary so the truncated string remains
        // valid UTF-8 even if the cap lands inside a multi-byte
        // codepoint. `char_indices` gives us the byte offset of every
        // char start; pick the largest start ≤ cap.
        let cap = decoded
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= README_MAX_BYTES)
            .last()
            .unwrap_or(0);
        // Trim at the char boundary, then clone so the returned String
        // owns its bytes. `String::truncate` would also work but
        // requires `mut`; this expresses the intent cleaner.
        return Ok(Some(decoded[..cap].to_owned()));
    }
    Ok(Some(decoded))
}

/// Strongly-typed view of the SKILL.md YAML frontmatter we consume at
/// pack time. We deliberately do **not** `deny_unknown_fields` here — a
/// SKILL.md frontmatter is the author's surface and may carry arbitrary
/// keys the CLI doesn't model (icon, tags, category, …). Only the
/// fields the publish flow cares about are pulled out.
#[derive(Debug, Default, Deserialize)]
struct FrontmatterRaw {
    #[serde(default)]
    name: Option<serde_yaml_ng::Value>,
    #[serde(default)]
    version: Option<serde_yaml_ng::Value>,
    #[serde(default)]
    sponsors: Option<Vec<Sponsor>>,
    /// Optional declared package kind. Most SKILL.md files omit it (it
    /// defaults to `"skills"` because the file is a SKILL.md); a
    /// publisher who packs a non-skills bundle can override it here so
    /// the JSON wire contract emitted by `pakx pack --json` reflects
    /// the actual kind instead of the historical hardcode.
    #[serde(default)]
    kind: Option<serde_yaml_ng::Value>,
    /// Free-form description Claude Code uses at skill-load decision
    /// time (see <https://code.claude.com/docs/en/skills>). Pulled out
    /// at pack-time so we can warn — non-fatally — when it's absent.
    #[serde(default)]
    description: Option<serde_yaml_ng::Value>,
}

#[derive(Default)]
struct Frontmatter {
    name: Option<String>,
    version: Option<String>,
    sponsors: Option<Vec<Sponsor>>,
    kind: Option<String>,
    description: Option<String>,
}

fn extract_frontmatter(text: &str) -> Result<Frontmatter> {
    // Normalise line endings before fence detection. A SKILL.md saved
    // by Notepad / VSCode-on-Windows with the default LF→CRLF setting
    // emits `---\r\n` for its fences; the previous LF-only matchers
    // (`strip_prefix("---\n")` + `find("\n---")`) silently fell
    // through, so `name:` / `version:` parsed as part of the body
    // instead of the frontmatter and `read_manifest` errored with
    // "missing `name:`". Collapsing CRLF → LF up front keeps the rest
    // of the parser simple and platform-independent.
    let normalised = if text.contains('\r') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    let text = normalised.as_ref();

    // Locate the fenced YAML block (`---` … `---`). SKILL.md is
    // markdown-with-frontmatter, so the block ends at the first `---`
    // that begins a line. Missing fences → fall back to the whole
    // document (Phase A behaviour) so a frontmatter-less file still
    // surfaces "missing name:" rather than a YAML parse error.
    let stripped = text.strip_prefix("---\n").unwrap_or(text);
    let block = stripped
        .find("\n---")
        .map_or(stripped, |end| &stripped[..end]);

    // A frontmatter-less SKILL.md leaves `block` as the whole markdown
    // body, which is not a YAML mapping. `serde_yaml_ng` would error
    // out on the first non-mapping line; treat an empty / non-mapping
    // block as "no fields" so the missing-`name:` error from
    // `read_manifest` still fires with a clean message.
    if block.trim().is_empty() {
        return Ok(Frontmatter::default());
    }
    // Walk via `Value` first so a frontmatter-less body that happens to
    // be valid YAML (a markdown comment `# Hi` is a YAML comment, etc.)
    // decodes to `Null` and surfaces as "missing name:" — not a parse
    // error — unless the author explicitly opened a `---` fence.
    let value: serde_yaml_ng::Value = match serde_yaml_ng::from_str(block) {
        Ok(v) => v,
        Err(e) => {
            if text.starts_with("---\n") {
                bail!("{SKILL_MD} frontmatter is not valid YAML: {e}");
            }
            return Ok(Frontmatter::default());
        }
    };
    if !value.is_mapping() {
        if text.starts_with("---\n") {
            bail!("{SKILL_MD} frontmatter must be a YAML mapping");
        }
        return Ok(Frontmatter::default());
    }
    let raw: FrontmatterRaw = serde_yaml_ng::from_value(value)
        .map_err(|e| anyhow!("{SKILL_MD} frontmatter failed to deserialize: {e}"))?;
    Ok(Frontmatter {
        name: raw.name.and_then(scalar_to_string),
        version: raw.version.and_then(scalar_to_string),
        sponsors: raw.sponsors,
        kind: raw.kind.and_then(scalar_to_string),
        description: raw.description.and_then(scalar_to_string),
    })
}

/// Flatten a YAML scalar (`String`, `Number`, `Bool`) into the string
/// form the registry expects. Returns `None` for sequences / mappings /
/// null so `read_manifest` surfaces the missing-field error.
fn scalar_to_string(v: serde_yaml_ng::Value) -> Option<String> {
    match v {
        serde_yaml_ng::Value::String(s) => Some(s),
        serde_yaml_ng::Value::Number(n) => Some(n.to_string()),
        serde_yaml_ng::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

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

fn build_tarball(src_dir: &Path, project_root: &Path) -> Result<Vec<u8>> {
    let mut files = collect_files(src_dir, project_root)?;
    files.sort_by(|a, b| a.relative.cmp(&b.relative));

    let mut buf = Vec::new();
    {
        let encoder = GzEncoder::new(&mut buf, Compression::default());
        let mut tar = tar::Builder::new(encoder);
        tar.mode(tar::HeaderMode::Deterministic);
        for entry in &files {
            let mut header = tar::Header::new_gnu();
            header.set_size(entry.contents.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append_data(&mut header, &entry.relative, entry.contents.as_slice())
                .with_context(|| format!("write tar entry {}", entry.relative))?;
        }
        tar.into_inner()
            .context("finalise tar stream")?
            .finish()
            .context("finalise gzip stream")?;
    }
    Ok(buf)
}

struct PackedFile {
    relative: String,
    contents: Vec<u8>,
}

fn collect_files(src_dir: &Path, project_root: &Path) -> Result<Vec<PackedFile>> {
    let mut out = Vec::new();
    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read_dir {}", redact_path(&dir, project_root)))?
        {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            // Refuse symlinks explicitly. `file_type().is_file()` follows
            // symlinks, so without this check a malicious skill template
            // could include a symlink to `~/.ssh/id_rsa` or `/etc/shadow`
            // and `pakx pack` would read the target and pack it into the
            // tarball that `pakx publish` then uploads. Refusing (not
            // silently skipping) is the right UX: a publish-time error
            // makes the surprise visible to the author before upload.
            //
            // The path here is rendered in its `src_dir`-relative form
            // so the author can locate the offending file without the
            // error leaking their host-absolute tempdir / home path.
            if ft.is_symlink() {
                bail!(
                    "symlinks under SKILL.md src/ are not allowed: {}",
                    redact_path(&path, src_dir)
                );
            }
            if ft.is_dir() {
                // Skip common noise dirs.
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if matches!(name, ".git" | "node_modules" | "target" | "__pycache__") {
                        continue;
                    }
                }
                stack.push(path);
            } else if ft.is_file() {
                let relative = path
                    .strip_prefix(src_dir)
                    .expect("path is under src_dir")
                    .to_string_lossy()
                    .replace('\\', "/");
                let mut contents = Vec::new();
                File::open(&path)
                    .with_context(|| format!("open {}", redact_path(&path, project_root)))?
                    .read_to_end(&mut contents)
                    .with_context(|| format!("read {}", redact_path(&path, project_root)))?;
                out.push(PackedFile { relative, contents });
            }
        }
    }
    if out.is_empty() {
        bail!(
            "no files found under {}",
            redact_path(src_dir, project_root)
        );
    }
    Ok(out)
}

#[cfg(test)]
mod readme_capture_tests {
    //! Pack-time README capture is the publisher-facing wire path that
    //! carries `README.md` from a bundle to the registry's `packages.readme`
    //! column. These tests pin the capture / truncation / absent
    //! semantics that `pakx publish` relies on. They live alongside the
    //! production code (instead of `tests/pack.rs`) because the binary
    //! crate doesn't expose `pack_dir` to integration tests.
    use super::{pack_dir, README_MAX_BYTES};
    use tempfile::TempDir;

    fn write_skill(dir: &std::path::Path, frontmatter: &str) {
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\n{frontmatter}---\n# Hi\n"),
        )
        .unwrap();
    }

    /// A bundle that ships a `README.md` alongside `SKILL.md` must have
    /// its README captured verbatim in `Manifest.readme` so `pakx
    /// publish` can forward the markdown to the registry.
    #[test]
    fn captures_readme_when_present() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        write_skill(
            src.path(),
            "name: demo\nversion: 0.1.0\ndescription: tidy.\n",
        );
        let body = "# demo\n\nLong-form usage docs that the registry will store.\n";
        std::fs::write(src.path().join("README.md"), body).unwrap();

        let result = pack_dir(src.path(), out.path()).expect("pack succeeds");
        assert_eq!(result.manifest.readme.as_deref(), Some(body));
        assert!(
            result.warnings.is_empty(),
            "no warnings expected when README fits and description is present: {:?}",
            result.warnings
        );
    }

    /// Bundles without a `README.md` must surface `Manifest.readme` as
    /// `None`. The CLI uses `None` (omit) vs `Some` (set) to drive the
    /// registry's omit-vs-explicit semantics on republish — `None` means
    /// "no change", which is what a publisher with no README expects.
    #[test]
    fn readme_is_none_when_absent() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        write_skill(
            src.path(),
            "name: demo\nversion: 0.1.0\ndescription: tidy.\n",
        );

        let result = pack_dir(src.path(), out.path()).expect("pack succeeds");
        assert!(result.manifest.readme.is_none());
    }

    /// An oversized README (>256 KiB) must be truncated at pack time —
    /// non-fatally — so the publish itself still succeeds and the wire
    /// payload stays under the registry cap. Truncation must land on a
    /// UTF-8 char boundary; the warning text must mention README and
    /// truncation so the publisher notices.
    #[test]
    fn truncates_oversize_readme_with_warning() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        write_skill(
            src.path(),
            "name: demo\nversion: 0.1.0\ndescription: tidy.\n",
        );
        let oversized = "x".repeat(README_MAX_BYTES + (50 * 1024));
        std::fs::write(src.path().join("README.md"), &oversized).unwrap();

        let result = pack_dir(src.path(), out.path()).expect("pack succeeds");
        let readme = result
            .manifest
            .readme
            .as_deref()
            .expect("manifest still captures README (truncated)");
        assert!(
            readme.len() <= README_MAX_BYTES,
            "truncated README must fit under the cap: got {} bytes (cap {README_MAX_BYTES})",
            readme.len()
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("README.md") && w.contains("truncating")),
            "a truncation warning must surface: warnings={:?}",
            result.warnings
        );
    }

    /// A README with multi-byte UTF-8 right at the truncation boundary
    /// must not produce a torn codepoint. The cap is in bytes, but the
    /// result must always be valid UTF-8 — otherwise downstream
    /// `serde_json::to_string` on the publish body would refuse the
    /// string. Synthesise a README with a multi-byte char straddling
    /// the cap and confirm the truncated string is still well-formed.
    #[test]
    fn truncation_respects_utf8_char_boundaries() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        write_skill(
            src.path(),
            "name: demo\nversion: 0.1.0\ndescription: tidy.\n",
        );
        // Build a string whose byte length crosses the cap inside a
        // 3-byte codepoint (`€` = U+20AC, 3 bytes). The exact offset
        // doesn't matter — what matters is that we have multi-byte
        // content past the cap so the naive `[..N]` slice would panic.
        let head = "a".repeat(README_MAX_BYTES - 1);
        let body = format!("{head}€{}", "b".repeat(1024));
        std::fs::write(src.path().join("README.md"), &body).unwrap();

        let result = pack_dir(src.path(), out.path()).expect("pack succeeds");
        let readme = result.manifest.readme.expect("readme captured");
        // The slice must be valid UTF-8 by construction (String
        // guarantees it) — assert the byte length is at or below cap
        // and that the trailing multi-byte char was dropped, not torn.
        assert!(readme.len() <= README_MAX_BYTES);
        assert!(
            !readme.ends_with(char::REPLACEMENT_CHARACTER),
            "truncation must not leave a replacement char tail: {:?}",
            &readme[readme.len().saturating_sub(4)..]
        );
    }
}
