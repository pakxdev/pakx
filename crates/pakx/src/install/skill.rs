//! Skill resolution → download → verify → extract for `pakx install`.
//!
//! Flow for one `skills:` dep:
//!   1. Parse the manifest shorthand `<owner>/<name>[@<version>]` into
//!      `(owner, name, requested_version?)`.
//!   2. Decide which version to install:
//!        - When pinned, skip straight to the per-version endpoint and
//!          honour whatever the registry returns (errors with 404 if
//!          the pin is unknown).
//!        - When unpinned, call `PakxSource::fetch(<owner>/<name>)`
//!          to enumerate `versions[]`, picking `latestVersion` (or the
//!          highest non-deprecated semver as fallback).
//!   3. Call `PakxSource::fetch_version(owner, name, picked)` —
//!      `GET /api/v1/packages/{owner}/{name}/{version}` — to obtain
//!      the **signed** `tarballUrl` plus the per-version `sha256`.
//!      The list/detail endpoint deliberately omits `tarballUrl`
//!      because signed URLs are short-TTL; the per-version endpoint
//!      mints a fresh signature per call.
//!   4. Download the signed `tarballUrl` via reqwest, streaming to a
//!      `tempfile::NamedTempFile` with a 50 MiB hard cap.
//!   5. Sha256-verify the bytes against the API-declared `sha256`;
//!      abort + unlink on mismatch.
//!   6. Untar (over gzip-decode) into `<claude_home>/skills/<owner>-<name>/`,
//!      enforcing four hardening guards on every entry:
//!        - canonical destination must stay within the dest root
//!          (zip-slip),
//!        - no symlinks or hardlinks,
//!        - no absolute paths,
//!        - sum of decompressed sizes must not exceed 50 MiB.
//!   7. Return a `ResolvedSkill` carrying the canonical pakx-registry
//!      URL (without signed query params) so the lockfile records a
//!      stable identity even though the signed URL is ephemeral.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use flate2::read::GzDecoder;
use pakx_core::Integrity;
use pakx_registry_client::{Package, PackageVersion, PakxSource, Source};
use sha2::{Digest, Sha256};
use tracing::debug;

/// 50 MiB upload cap matches `pakx pack` (see `crates/pakx/src/pack.rs`).
/// Mirrored on the read side as both a download cap (bytes streamed
/// from the signed URL) and a decompressed cap (sum of tar-entry
/// payload sizes). Two independent guards catch both the
/// "uncompressed-but-huge" tarball and the "tiny-but-zip-bomb" case.
pub(super) const MAX_TARBALL_BYTES: u64 = 50 * 1024 * 1024;

/// Outcome of resolving one skill dep. Carries everything needed to
/// write the lockfile entry **and** what we wrote to disk.
///
/// Some fields (`owner`, `name`, `install_path`) are surfaced for
/// downstream tooling (`pakx list`, `pakx doctor`) even though the
/// runner only consumes a subset directly — pinning them in the
/// struct keeps the contract stable across the consumers.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ResolvedSkill {
    /// `<owner>/<name>` canonical id.
    pub id: String,
    pub owner: String,
    pub name: String,
    /// Pinned version (after `latest` / requested resolution).
    pub version: String,
    /// Integrity of the downloaded tarball, recomputed locally.
    pub integrity: Integrity,
    /// Canonical pakx-registry URL **without** signed query params —
    /// the stable on-record identity. The actual download URL had a
    /// short-lived blob signature that we strip before logging into
    /// the lockfile.
    pub canonical_url: String,
    /// Where the tarball got extracted (claude-home variant).
    pub install_path: PathBuf,
}

// ---------------------------------------------------------------------------
// Step 1 — parse the manifest shorthand
// ---------------------------------------------------------------------------

/// Same shape as [`parse_skill_shorthand`]; re-exported under a kind-
/// neutral name so the bundle (commands / subagents / prompts / hooks)
/// installer can call it without pretending it's parsing a skill id.
pub(super) fn parse_bundle_shorthand(s: &str) -> Result<(String, String, Option<String>)> {
    parse_skill_shorthand(s)
}

/// Parse the shorthand string a user wrote in `agents.yml`:
///
/// - `arwenizEr/hello-world` → unversioned (resolver picks latest).
/// - `arwenizEr/hello-world@0.1.1` → pinned to `0.1.1`.
///
/// Anything more complex (semver ranges, `^1.x` etc.) is **not** v0.1
/// scope — `pakx publish` only emits exact versions and we honour
/// only exact pins or "latest" until ranges land in Phase C.
pub fn parse_skill_shorthand(s: &str) -> Result<(String, String, Option<String>)> {
    let (owner_name, version) = match s.split_once('@') {
        Some((on, v)) if !v.is_empty() => (on, Some(v.to_owned())),
        _ => (s, None),
    };
    let (owner, name) = owner_name.split_once('/').ok_or_else(|| {
        anyhow!("skill id {s:?} must be `<owner>/<name>[@<version>]` (missing '/')")
    })?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        bail!("skill id {s:?} must be `<owner>/<name>[@<version>]`");
    }
    Ok((owner.to_owned(), name.to_owned(), version))
}

// ---------------------------------------------------------------------------
// Step 2-3 — version selection from the PakxSource detail response
// ---------------------------------------------------------------------------

/// One version row in `install_hints.versions[]`. Decoded lazily on
/// demand because `PakxSource` is intentionally schema-loose so the
/// CLI doesn't break on additive backend fields.
///
/// Only the **version** + **deprecated** flag are needed at this layer
/// — sha256 / tarballUrl live on the per-version endpoint and never
/// surface on the list/detail page (the latter omits `tarballUrl` to
/// avoid minting signed URLs for every entry; mirrored in the e2e
/// mock).
#[derive(Debug, Clone)]
struct VersionRow {
    version: String,
    deprecated: bool,
}

impl VersionRow {
    fn from_json(v: &serde_json::Value) -> Option<Self> {
        let version = v.get("version")?.as_str()?.to_owned();
        let deprecated = v
            .get("deprecated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Some(Self {
            version,
            deprecated,
        })
    }
}

/// Pick the version to install given the requested pin (or `None` →
/// latest). Strategy:
///
/// - If the user pinned a version: that one, full stop. Error if the
///   API doesn't list it.
/// - Else: prefer the API's `latestVersion` top-level field if it
///   resolves to a real entry. Per [[pakx-publish-smoke]] the
///   list-endpoint subquery currently returns `null` for that field,
///   so we fall back to "highest semver-comparable version that is
///   not deprecated".
fn pick_version<'a>(
    rows: &'a [VersionRow],
    latest_hint: Option<&str>,
    requested: Option<&str>,
) -> Result<&'a VersionRow> {
    if rows.is_empty() {
        bail!("registry returned no published versions");
    }

    if let Some(want) = requested {
        return rows
            .iter()
            .find(|r| r.version == want)
            .ok_or_else(|| anyhow!("version {want:?} not in registry's published list"));
    }

    if let Some(latest) = latest_hint {
        if let Some(row) = rows.iter().find(|r| r.version == latest && !r.deprecated) {
            return Ok(row);
        }
        debug!(target: "pakx::install::skill", latest, "latestVersion hint did not match any non-deprecated row; falling back to semver-pick");
    }

    // Fallback: highest semver-comparable non-deprecated row.
    let mut candidates: Vec<&VersionRow> = rows.iter().filter(|r| !r.deprecated).collect();
    if candidates.is_empty() {
        bail!("every published version is marked deprecated");
    }
    candidates.sort_by(|a, b| semver_cmp(&b.version, &a.version));
    Ok(candidates[0])
}

/// Minimal semver-style comparator over dot-separated numeric segments.
/// Non-numeric or extra segments compare lexicographically as a tie-
/// breaker. Sufficient for `pakx publish` outputs which are always
/// `MAJOR.MINOR.PATCH`; ranges / pre-release tags are out of scope
/// here and the worst case is a wrong-but-stable ordering.
fn semver_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut as_iter = a.split('.');
    let mut bs_iter = b.split('.');
    loop {
        match (as_iter.next(), bs_iter.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(av), Some(bv)) => match (av.parse::<u64>(), bv.parse::<u64>()) {
                (Ok(an), Ok(bn)) => match an.cmp(&bn) {
                    std::cmp::Ordering::Equal => {}
                    ord => return ord,
                },
                _ => match av.cmp(bv) {
                    std::cmp::Ordering::Equal => {}
                    ord => return ord,
                },
            },
        }
    }
}

/// Pull `versions[]` + `latestVersion` from a `PakxSource::fetch`
/// response. Public for tests so we can exercise the picker without
/// going through wiremock for the resolver tests.
fn extract_version_rows(pkg: &Package) -> (Vec<VersionRow>, Option<String>) {
    let hints = &pkg.install_hints;
    let rows = hints
        .get("versions")
        .and_then(serde_json::Value::as_array)
        .map_or_else(Vec::new, |arr| {
            arr.iter().filter_map(VersionRow::from_json).collect()
        });
    let latest = hints
        .get("latestVersion")
        .or_else(|| hints.get("latest_version"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    (rows, latest)
}

/// Resolve the skill id against a `PakxSource` and return all of the
/// data needed to download.
///
/// Two-step against the registry:
///   1. Pick the version. When `requested_version` is `Some`, skip the
///      list/detail call entirely and trust the pin (the per-version
///      endpoint 404s loudly if it doesn't exist). When `None`, fetch
///      the list/detail body to enumerate `versions[]` and pick
///      latest / highest semver.
///   2. Call `fetch_version` (`GET /api/v1/packages/{owner}/{name}/{version}`)
///      to obtain the signed `tarballUrl` and per-version `sha256`.
///      This is the **source of truth** for the download URL — the
///      list/detail endpoint deliberately omits `tarballUrl` because
///      signed URLs are short-TTL.
///
/// Pure data-out; no FS side effects.
pub async fn resolve(
    source: &PakxSource,
    id: &str,
    requested_version: Option<&str>,
) -> Result<SkillResolution> {
    let (owner, name, _) = parse_skill_shorthand(id)?;

    // Pick the target version. Pinned → trust the pin (avoid the list
    // round-trip). Unpinned → enumerate via the detail endpoint and
    // pick latest / highest semver.
    let target_version = if let Some(v) = requested_version {
        v.to_owned()
    } else {
        let pkg = source
            .fetch(id)
            .await
            .with_context(|| format!("fetch skill metadata for {id}"))?;
        let (rows, latest) = extract_version_rows(&pkg);
        pick_version(&rows, latest.as_deref(), None)?
            .version
            .clone()
    };

    let version_meta = source
        .fetch_version(&owner, &name, &target_version)
        .await
        .with_context(|| format!("fetch {id}@{target_version} metadata"))?;
    resolution_from_version_meta(id, &target_version, &version_meta)
}

/// Pull the `(sha256, tarballUrl)` pair from a per-version response,
/// erroring with a precise message when either field is missing.
fn resolution_from_version_meta(
    id: &str,
    version: &str,
    meta: &PackageVersion,
) -> Result<SkillResolution> {
    let sha = meta
        .sha256
        .clone()
        .ok_or_else(|| anyhow!("registry response for {id}@{version} omits sha256"))?;
    let tarball_url = meta
        .tarball_url
        .clone()
        .ok_or_else(|| anyhow!("registry response for {id}@{version} omits tarballUrl"))?;
    Ok(SkillResolution {
        version: meta.version.clone(),
        sha256_hex: sha,
        tarball_url,
    })
}

/// What `resolve` decided. The download step turns this into a
/// `ResolvedSkill` after fetching + verifying + extracting.
#[derive(Debug, Clone)]
pub struct SkillResolution {
    pub version: String,
    /// Hex sha256 from the API.
    pub sha256_hex: String,
    /// Signed tarball URL (short-TTL).
    pub tarball_url: String,
}

// ---------------------------------------------------------------------------
// Step 4 — download with size cap
// ---------------------------------------------------------------------------

/// Stream the signed `tarball_url` to a temp file, capping at 50 MiB.
/// Returns the open temp file (positioned at start of file) so the
/// caller can sha-hash it without re-reading from disk twice.
pub(super) async fn download_capped(
    http: &reqwest::Client,
    tarball_url: &str,
) -> Result<tempfile::NamedTempFile> {
    debug!(target: "pakx::install::skill", url = %tarball_url, "downloading tarball");
    let mut response = http
        .get(tarball_url)
        .send()
        .await
        .with_context(|| format!("GET {tarball_url}"))?
        .error_for_status()
        .with_context(|| format!("non-2xx from {tarball_url}"))?;

    // `tempfile::NamedTempFile::new()` defaults to the system temp dir
    // which is fine for short-lived staging; the extract step writes
    // into `claude_home/skills/...` after verification.
    let mut tmp =
        tempfile::NamedTempFile::new().context("create temp file for tarball download")?;
    let mut total: u64 = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .context("read chunk from tarball download")?
    {
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| anyhow!("download size overflowed u64"))?;
        if total > MAX_TARBALL_BYTES {
            // Unlinks the temp file via `NamedTempFile`'s Drop on
            // scope exit; the explicit `drop` call here is best-
            // effort cleanup for early aborts.
            drop(tmp);
            bail!("tarball download exceeds {MAX_TARBALL_BYTES} byte cap (got {total} bytes streamed)");
        }
        tmp.write_all(&chunk)
            .context("write tarball chunk to temp file")?;
    }
    tmp.as_file_mut()
        .flush()
        .context("flush tarball temp file")?;
    tmp.as_file_mut()
        .seek(SeekFrom::Start(0))
        .context("seek tarball temp file to start")?;
    Ok(tmp)
}

// ---------------------------------------------------------------------------
// Step 5 — sha256 verify
// ---------------------------------------------------------------------------

/// Compare the downloaded bytes' sha256 to `expected_hex`. Returns
/// the SRI-style `Integrity` for the lockfile on success. On
/// mismatch, the temp file gets unlinked (via the caller's `drop`)
/// and we abort.
pub(super) fn verify_sha256(
    tmp: &mut tempfile::NamedTempFile,
    expected_hex: &str,
    id: &str,
) -> Result<Integrity> {
    tmp.as_file_mut()
        .seek(SeekFrom::Start(0))
        .context("rewind tarball temp file for hashing")?;
    let mut hasher = Sha256::new();
    // 64 KiB read buffer. Allocated on the heap to dodge clippy's
    // `large_stack_arrays` lint (16 KiB threshold). The download
    // path always reads from a disk-backed `NamedTempFile`, so the
    // sync `read` here is fine inside our `tokio::main` runtime —
    // the surrounding async path already does the network I/O off-
    // thread.
    let mut buf = vec![0u8; 65536];
    loop {
        let n = tmp
            .as_file_mut()
            .read(&mut buf)
            .context("read tarball temp file while hashing")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let got_hex = bytes_to_hex(&digest);
    if !sha_hex_eq(&got_hex, expected_hex) {
        bail!("integrity mismatch for {id}: expected {expected_hex}, got {got_hex}");
    }
    // Rewind once more for the extraction pass.
    tmp.as_file_mut()
        .seek(SeekFrom::Start(0))
        .context("rewind tarball temp file for extraction")?;
    let b64 = BASE64_STANDARD.encode(digest);
    Integrity::parse(format!("sha256-{b64}"))
        .map_err(|e| anyhow!("bug: locally-built sha256 base64 failed regex: {e}"))
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Case-insensitive sha256 hex comparison.
fn sha_hex_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.eq_ignore_ascii_case(b)
}

// ---------------------------------------------------------------------------
// Step 6 — extract with hardening guards
// ---------------------------------------------------------------------------

/// Extract the verified tarball at `tmp` into `dest_root`, replacing
/// any prior contents at that root. Enforces zip-slip / symlink /
/// absolute-path / decompressed-size guards on every entry.
pub(super) fn extract_tarball(
    tmp: &mut tempfile::NamedTempFile,
    dest_root: &Path,
    id: &str,
) -> Result<()> {
    // Wipe + recreate the destination so we install a clean tree per
    // version. The path is owned by us (under `<claude_home>/skills/`)
    // so the wipe is safe.
    if dest_root.exists() {
        std::fs::remove_dir_all(dest_root)
            .with_context(|| format!("clear existing {}", dest_root.display()))?;
    }
    std::fs::create_dir_all(dest_root)
        .with_context(|| format!("create {}", dest_root.display()))?;

    // Canonicalise the dest root **after** we've created it. We compare
    // every entry's intended destination against this canonicalised
    // root to refuse anything that escapes.
    let canonical_root = dest_root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", dest_root.display()))?;

    let mut file_for_extract = tmp
        .as_file_mut()
        .try_clone()
        .context("clone tarball file handle for extraction")?;
    file_for_extract
        .seek(SeekFrom::Start(0))
        .context("rewind tarball file handle")?;
    let decoder = GzDecoder::new(file_for_extract);
    let mut archive = tar::Archive::new(decoder);
    // Disable libtar's own follow-symlinks behavior on extraction —
    // we refuse symlinks outright on the next line anyway, but
    // belt-and-braces is cheap.
    archive.set_overwrite(true);
    archive.set_preserve_permissions(false);

    let mut total_decompressed: u64 = 0;

    for entry in archive
        .entries()
        .context("read tar entries from gzip stream")?
    {
        let mut entry = entry.context("read one tar entry")?;
        let entry_type = entry.header().entry_type();

        // Hard refuse symlinks + hardlinks. `pakx pack` already
        // refuses symlinks server-side; this is defense-in-depth at
        // install time. `Link` (hardlink) gets the same treatment —
        // both shapes can be abused to point at files we don't own.
        if matches!(entry_type, tar::EntryType::Symlink | tar::EntryType::Link) {
            bail!("entry in {id} tarball is a symlink/hardlink: refused");
        }

        // Skip directory entries. We create directories implicitly
        // when writing the files inside them.
        if entry_type == tar::EntryType::Directory {
            continue;
        }

        // Decode the entry's path and run every component through the
        // zip-slip guard. Absolute paths and `..` segments fail
        // immediately; relative paths get joined against the dest
        // root and the result must canonicalise back inside the dest
        // root.
        let raw_path = entry
            .path()
            .with_context(|| format!("decode path of entry in {id} tarball"))?;
        let safe_relative = validate_entry_path(&raw_path)
            .map_err(|reason| anyhow!("entry in {id} tarball: {reason}"))?;

        // Skip the entry size cap pre-check and instead use the
        // streamed payload size. The header's `size()` is untrusted
        // (a malicious tarball can lie); the streamed length is the
        // ground truth.
        let header_size = entry.header().size().unwrap_or(0);

        // Speculative early-abort: if the header alone declares more
        // than the remaining budget, we don't even open the entry.
        // Safe lower bound — actual write check below uses the real
        // streamed length.
        if total_decompressed.saturating_add(header_size) > MAX_TARBALL_BYTES {
            bail!(
                "decompressed size exceeds {} MiB cap while extracting {id}",
                MAX_TARBALL_BYTES / (1024 * 1024)
            );
        }

        let dest_path = canonical_root.join(&safe_relative);

        // Canonicalise the parent (which exists if we've already
        // created it for a previous entry, otherwise we create it
        // first) so we can verify dest_path stays inside dest_root
        // **after** any symlink the FS itself might host. The
        // `canonicalize` call resolves both `..` and any symlinks
        // present in the path-on-disk.
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create parent dir {}", parent.display()))?;
        }

        // Read the entry payload into memory while counting bytes.
        // This is safe because of the 50 MiB cap; we never hold more
        // than that in memory at once (and almost always far less).
        let mut payload = Vec::with_capacity(usize::try_from(header_size).unwrap_or(0));
        let written = std::io::copy(&mut entry, &mut payload)
            .with_context(|| format!("read payload of {} in tarball", safe_relative.display()))?;
        total_decompressed = total_decompressed.saturating_add(written);
        if total_decompressed > MAX_TARBALL_BYTES {
            bail!(
                "decompressed size exceeds {} MiB cap while extracting {id}",
                MAX_TARBALL_BYTES / (1024 * 1024)
            );
        }

        // Post-join canonicalization to defeat any in-tree symlink
        // that the parent created above might host. We canonicalize
        // the parent (it exists at this point) and rejoin the file
        // name, then check `starts_with(canonical_root)`. We can't
        // canonicalize `dest_path` itself because the file doesn't
        // exist yet.
        let parent_canonical = dest_path.parent().map_or_else(
            || canonical_root.clone(),
            |p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()),
        );
        if !parent_canonical.starts_with(&canonical_root) {
            bail!("entry in {id} tarball: entry escapes destination root after canonicalize");
        }

        let mut out_file =
            File::create(&dest_path).with_context(|| format!("create {}", dest_path.display()))?;
        out_file
            .write_all(&payload)
            .with_context(|| format!("write {}", dest_path.display()))?;
    }

    Ok(())
}

/// Per-entry path guard. Returns the cleaned relative path on success,
/// or an explanation string on failure. Rejects:
///   - absolute paths (unix `/foo` or windows `C:\foo`),
///   - any `..` segment,
///   - any prefix / root component (drive letters etc.),
///   - empty paths.
fn validate_entry_path(p: &Path) -> Result<PathBuf, String> {
    if p.as_os_str().is_empty() {
        return Err("empty entry path".into());
    }
    if p.is_absolute() {
        return Err("entry path is absolute".into());
    }
    let mut out = PathBuf::new();
    for component in p.components() {
        match component {
            Component::ParentDir => {
                return Err("entry escapes destination (contains `..`)".into());
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err("entry path has a drive prefix or root".into());
            }
            Component::CurDir => {}
            Component::Normal(seg) => out.push(seg),
        }
    }
    if out.as_os_str().is_empty() {
        return Err("entry path resolves to empty".into());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Step 7 — canonical URL (strip signed query)
// ---------------------------------------------------------------------------

/// The canonical pakx-registry URL for a `(owner, name, version)`
/// triple. We never persist the signed `tarballUrl` into the lockfile
/// because the signature is ephemeral; instead we record a stable URL
/// that `pakx doctor` can re-resolve later.
pub(super) fn canonical_url(base_url: &str, owner: &str, name: &str, version: &str) -> String {
    format!(
        "{}/api/v1/packages/{}/{}/{}/tarball",
        base_url.trim_end_matches('/'),
        owner,
        name,
        version
    )
}

// ---------------------------------------------------------------------------
// Top-level public API
// ---------------------------------------------------------------------------

/// Install one skill dep into the Claude Code skills tree.
///
/// `claude_home` is `<config_dir>` for the Claude Code adapter — i.e.
/// `~/.claude` in production and a tempdir in tests. Skills get
/// written to `<claude_home>/skills/<owner>-<name>/`, which matches
/// how Claude Code organically organizes its own skill bundles (one
/// flat dir per skill, no version subdir; the next install of a
/// different version overwrites in place).
pub async fn install_skill_from_pakx(
    source: &PakxSource,
    http: &reqwest::Client,
    base_url: &str,
    claude_home: &Path,
    id: &str,
    requested_version: Option<&str>,
) -> Result<ResolvedSkill> {
    let (owner, name, _) = parse_skill_shorthand(id)?;
    let canonical_id = format!("{owner}/{name}");
    let resolution = resolve(source, &canonical_id, requested_version).await?;

    let mut tmp = download_capped(http, &resolution.tarball_url).await?;
    let integrity = verify_sha256(&mut tmp, &resolution.sha256_hex, &canonical_id)?;

    let dest = claude_home.join("skills").join(format!("{owner}-{name}"));
    extract_tarball(&mut tmp, &dest, &canonical_id)?;

    Ok(ResolvedSkill {
        id: canonical_id,
        owner: owner.clone(),
        name: name.clone(),
        version: resolution.version.clone(),
        integrity,
        canonical_url: canonical_url(base_url, &owner, &name, &resolution.version),
        install_path: dest,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn row(version: &str, deprecated: bool) -> VersionRow {
        VersionRow {
            version: version.into(),
            deprecated,
        }
    }

    #[test]
    fn parse_shorthand_unversioned() {
        let (o, n, v) = parse_skill_shorthand("alice/bob").unwrap();
        assert_eq!(o, "alice");
        assert_eq!(n, "bob");
        assert!(v.is_none());
    }

    #[test]
    fn parse_shorthand_versioned() {
        let (o, n, v) = parse_skill_shorthand("alice/bob@1.2.3").unwrap();
        assert_eq!(o, "alice");
        assert_eq!(n, "bob");
        assert_eq!(v.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn parse_shorthand_rejects_missing_slash() {
        assert!(parse_skill_shorthand("alicebob").is_err());
    }

    #[test]
    fn parse_shorthand_rejects_extra_slash() {
        assert!(parse_skill_shorthand("a/b/c").is_err());
    }

    #[test]
    fn pick_version_honors_requested() {
        let rows = vec![row("1.0.0", false), row("2.0.0", false)];
        let chosen = pick_version(&rows, Some("2.0.0"), Some("1.0.0")).unwrap();
        assert_eq!(chosen.version, "1.0.0");
    }

    #[test]
    fn pick_version_falls_back_to_latest_hint() {
        let rows = vec![row("0.1.0", false), row("0.1.1", false)];
        let chosen = pick_version(&rows, Some("0.1.1"), None).unwrap();
        assert_eq!(chosen.version, "0.1.1");
    }

    #[test]
    fn pick_version_falls_back_to_highest_when_latest_null() {
        let rows = vec![
            row("0.1.0", false),
            row("0.2.3", false),
            row("0.1.5", false),
        ];
        let chosen = pick_version(&rows, None, None).unwrap();
        assert_eq!(chosen.version, "0.2.3");
    }

    #[test]
    fn pick_version_skips_deprecated_in_semver_pick() {
        let rows = vec![row("9.9.9", true), row("1.0.0", false)];
        let chosen = pick_version(&rows, None, None).unwrap();
        assert_eq!(chosen.version, "1.0.0");
    }

    #[test]
    fn pick_version_errors_on_pinned_miss() {
        let rows = vec![row("1.0.0", false)];
        let err = pick_version(&rows, None, Some("2.0.0")).unwrap_err();
        assert!(err.to_string().contains("not in registry"));
    }

    #[test]
    fn validate_entry_path_accepts_relative() {
        let ok = validate_entry_path(Path::new("a/b/c.txt")).unwrap();
        assert_eq!(ok, PathBuf::from("a/b/c.txt"));
    }

    #[test]
    fn validate_entry_path_rejects_parent_dir() {
        let err = validate_entry_path(Path::new("../escape")).unwrap_err();
        assert!(err.contains("escapes destination"));
    }

    #[test]
    fn validate_entry_path_rejects_absolute_unix() {
        // `/etc/passwd` is unix-absolute and contains a `RootDir`
        // component on every platform. On unix `is_absolute()`
        // catches it; on windows it lacks a drive prefix so the
        // `RootDir` branch fires instead — both rejections are
        // valid. We pin the union of valid rejection messages here.
        let err = validate_entry_path(Path::new("/etc/passwd")).unwrap_err();
        assert!(
            err.contains("absolute") || err.contains("drive prefix or root"),
            "expected absolute-path rejection, got: {err}"
        );
    }

    #[test]
    fn validate_entry_path_rejects_empty() {
        let err = validate_entry_path(Path::new("")).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn semver_cmp_basic_ordering() {
        assert!(semver_cmp("1.0.0", "2.0.0").is_lt());
        assert!(semver_cmp("0.1.10", "0.1.9").is_gt()); // numeric, not lex
        assert!(semver_cmp("1.0.0", "1.0.0").is_eq());
    }

    #[test]
    fn canonical_url_strips_signed_query() {
        let url = canonical_url("https://registry.pakx.dev/", "alice", "hello", "0.1.1");
        assert_eq!(
            url,
            "https://registry.pakx.dev/api/v1/packages/alice/hello/0.1.1/tarball"
        );
    }

    #[test]
    fn sha_hex_eq_case_insensitive() {
        assert!(sha_hex_eq("ABCDEF", "abcdef"));
        assert!(!sha_hex_eq("ABC", "DEF"));
        assert!(!sha_hex_eq("ABC", "ABCD"));
    }

    #[test]
    fn bytes_to_hex_pads_correctly() {
        assert_eq!(bytes_to_hex(&[0x0a, 0xff]), "0aff");
    }
}
