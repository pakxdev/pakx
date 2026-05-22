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
const MAX_TARBALL_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    /// Sponsor links declared in the SKILL.md frontmatter, validated at
    /// pack-time against the shape in
    /// `pakx-registry/SPONSOR_LINKS_SPEC.md`. Empty when the field is
    /// missing; never `None` so downstream callers don't need to branch
    /// between "no field" and "empty list".
    pub sponsors: Vec<Sponsor>,
}

#[derive(Debug, Clone)]
pub struct PackOutput {
    pub manifest: Manifest,
    pub tarball_path: PathBuf,
    pub bytes: Vec<u8>,
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
    let manifest = read_manifest(src_dir, &cwd)?;
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

    let mut out = File::create(&tarball_path)
        .with_context(|| format!("create {}", redact_path(&tarball_path, &cwd)))?;
    out.write_all(&bytes)
        .with_context(|| format!("write {}", redact_path(&tarball_path, &cwd)))?;

    Ok(PackOutput {
        manifest,
        tarball_path,
        bytes,
    })
}

fn read_manifest(src_dir: &Path, project_root: &Path) -> Result<Manifest> {
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
    Ok(Manifest {
        name,
        version,
        sponsors,
    })
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
}

#[derive(Default)]
struct Frontmatter {
    name: Option<String>,
    version: Option<String>,
    sponsors: Option<Vec<Sponsor>>,
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
