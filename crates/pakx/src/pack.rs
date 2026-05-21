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

const SKILL_MD: &str = "SKILL.md";
const MAX_TARBALL_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
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
    let manifest = read_manifest(src_dir)?;
    let tarball_name = format!("{}-{}.tgz", manifest.name, manifest.version);
    let tarball_path = out_dir.join(&tarball_name);

    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("create output dir {}", out_dir.display()))?;

    let bytes = build_tarball(src_dir)?;
    if bytes.len() as u64 > MAX_TARBALL_BYTES {
        bail!(
            "tarball {} bytes exceeds {} byte limit",
            bytes.len(),
            MAX_TARBALL_BYTES
        );
    }

    let mut out = File::create(&tarball_path)
        .with_context(|| format!("create {}", tarball_path.display()))?;
    out.write_all(&bytes)
        .with_context(|| format!("write {}", tarball_path.display()))?;

    Ok(PackOutput {
        manifest,
        tarball_path,
        bytes,
    })
}

fn read_manifest(src_dir: &Path) -> Result<Manifest> {
    let skill_md = src_dir.join(SKILL_MD);
    let text = std::fs::read_to_string(&skill_md)
        .with_context(|| format!("read {}", skill_md.display()))?;
    let frontmatter = extract_frontmatter(&text);
    let name = frontmatter
        .name
        .ok_or_else(|| anyhow!("{SKILL_MD} frontmatter missing `name:`"))?;
    let version = frontmatter
        .version
        .ok_or_else(|| anyhow!("{SKILL_MD} frontmatter missing `version:`"))?;
    validate_name(&name)?;
    Ok(Manifest { name, version })
}

#[derive(Default)]
struct Frontmatter {
    name: Option<String>,
    version: Option<String>,
}

fn extract_frontmatter(text: &str) -> Frontmatter {
    // Permissive parse: only the `name:` and `version:` lines matter
    // for pack/publish at v0.1. Future Phase C+ may grow this into a
    // proper YAML loader.
    let stripped = text.strip_prefix("---\n").unwrap_or(text);
    let block_end = stripped.find("\n---").unwrap_or(stripped.len());
    let block = &stripped[..block_end];

    let mut out = Frontmatter::default();
    for line in block.lines() {
        let line = line.trim_end();
        if let Some(v) = line.strip_prefix("name:") {
            out.name = Some(clean(v));
        } else if let Some(v) = line.strip_prefix("version:") {
            out.version = Some(clean(v));
        }
    }
    out
}

fn clean(s: &str) -> String {
    s.trim()
        .trim_matches(|c: char| c == '"' || c == '\'')
        .to_owned()
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

fn build_tarball(src_dir: &Path) -> Result<Vec<u8>> {
    let mut files = collect_files(src_dir)?;
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

fn collect_files(src_dir: &Path) -> Result<Vec<PackedFile>> {
    let mut out = Vec::new();
    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
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
                    .with_context(|| format!("open {}", path.display()))?
                    .read_to_end(&mut contents)
                    .with_context(|| format!("read {}", path.display()))?;
                out.push(PackedFile { relative, contents });
            }
        }
    }
    if out.is_empty() {
        bail!("no files found under {}", src_dir.display());
    }
    Ok(out)
}
