//! End-to-end tests for `pakx install` resolving `skills:` deps
//! through the pakx-registry source.
//!
//! Every scenario builds its own tarball in-process via `tar::Builder`
//! over `flate2::GzEncoder` so the tests stay hermetic — no external
//! fixture files shipped or downloaded. Each test:
//!   1. spins up a wiremock,
//!   2. seeds a `agents.yml` with one `skills:` entry,
//!   3. invokes the real `pakx` binary with the hidden
//!      `--pakx-base-url` + `--claude-home` overrides,
//!   4. asserts on extracted files + lockfile contents (success
//!      cases) OR on the failure message (negative cases).

use std::io::Write;

use assert_cmd::Command;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

// ---------------------------------------------------------------------------
// Tarball helpers
// ---------------------------------------------------------------------------

/// One entry going into a test tarball.
struct TarEntry {
    /// Tar archive path. Pass `../escape` to exercise the zip-slip
    /// guard or absolute paths to exercise the abs-path guard.
    path: String,
    /// Payload bytes (always a regular file at v0.1).
    contents: Vec<u8>,
    /// When set, the entry is written as a symlink with this target
    /// instead of a regular file.
    symlink_target: Option<String>,
}

impl TarEntry {
    fn file(path: &str, contents: &[u8]) -> Self {
        Self {
            path: path.into(),
            contents: contents.to_vec(),
            symlink_target: None,
        }
    }

    fn symlink(path: &str, target: &str) -> Self {
        Self {
            path: path.into(),
            contents: Vec::new(),
            symlink_target: Some(target.into()),
        }
    }
}

/// Build a gzipped tarball in memory from the given entries. Returns
/// `(bytes, sha256_hex)`.
///
/// We bypass `tar::Builder::append_data` (which validates path shape
/// — exactly the validation we're trying to exercise on the extract
/// side) and instead hand-build each 512-byte ustar header. That
/// lets us put `../escape` and other shapes the install-side
/// guards must refuse into the test tarball.
fn build_test_tarball(entries: &[TarEntry]) -> (Vec<u8>, String) {
    let mut buf = Vec::new();
    {
        let mut encoder = GzEncoder::new(&mut buf, Compression::default());
        for entry in entries {
            let header_bytes = build_raw_tar_header(entry);
            encoder.write_all(&header_bytes).expect("write header");
            // Symlink entries have zero payload; regular files write
            // their bytes padded out to a 512-byte boundary.
            if entry.symlink_target.is_none() {
                encoder.write_all(&entry.contents).expect("write payload");
                let pad = (512 - (entry.contents.len() % 512)) % 512;
                if pad > 0 {
                    encoder.write_all(&vec![0u8; pad]).expect("write padding");
                }
            }
        }
        // Two trailing zero blocks signal end-of-archive per the tar
        // spec.
        encoder
            .write_all(&[0u8; 1024])
            .expect("write trailing zero blocks");
        encoder.finish().expect("finalize gzip");
    }
    let sha = bytes_to_hex(&Sha256::digest(&buf));
    (buf, sha)
}

/// Hand-build a single 512-byte ustar header. Lets the tests encode
/// paths (`../escape`, `/abs/path`) and symlink entries that the
/// high-level `tar::Builder` helpers refuse to emit.
fn build_raw_tar_header(entry: &TarEntry) -> [u8; 512] {
    let mut header = [0u8; 512];
    // name (0..100): truncated path bytes.
    let name_bytes = entry.path.as_bytes();
    let name_len = name_bytes.len().min(100);
    header[..name_len].copy_from_slice(&name_bytes[..name_len]);
    // mode (100..108).
    header[100..108].copy_from_slice(b"0000644\0");
    // uid (108..116) + gid (116..124).
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    // size (124..136): 11-octal-digit + null.
    let size = if entry.symlink_target.is_some() {
        0
    } else {
        entry.contents.len() as u64
    };
    let size_octal = format!("{size:011o}\0");
    header[124..136].copy_from_slice(size_octal.as_bytes());
    // mtime (136..148).
    header[136..148].copy_from_slice(b"00000000000\0");
    // chksum (148..156): 8 spaces (placeholder).
    header[148..156].copy_from_slice(b"        ");
    // typeflag (156): '0' regular, '2' symlink.
    header[156] = if entry.symlink_target.is_some() {
        b'2'
    } else {
        b'0'
    };
    // linkname (157..257).
    if let Some(target) = entry.symlink_target.as_deref() {
        let target_bytes = target.as_bytes();
        let target_len = target_bytes.len().min(100);
        header[157..157 + target_len].copy_from_slice(&target_bytes[..target_len]);
    }
    // magic (257..263) + version (263..265).
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    // chksum: sum of header bytes (with chksum field as spaces).
    let sum: u32 = header.iter().copied().map(u32::from).sum();
    let chksum = format!("{sum:06o}\0 ");
    header[148..156].copy_from_slice(chksum.as_bytes());
    header
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ---------------------------------------------------------------------------
// Mock server helpers
// ---------------------------------------------------------------------------

/// Mount the package-detail endpoint, the per-version endpoint, and
/// the blob download endpoint on a fresh wiremock.
///
/// The signed URL the **per-version** response points at is the same
/// wiremock — we just give it a known path the test can mount the
/// tarball response under. `?download=1&sig=ABC` is appended so the
/// strip-on-write assertion (lockfile must record a canonical URL,
/// not the signed one) has something to strip.
///
/// We register both the list/detail endpoint and the per-version
/// endpoint. The list endpoint's `versions[]` entries deliberately do
/// **not** carry `tarballUrl` — that mirrors production, where signed
/// URLs are only minted by the per-version route. Including the
/// `tarballUrl` here would mask the bug that PR #36 missed (the
/// resolver was reading from the wrong endpoint).
async fn mock_pakx_skill_registry(
    id: &str,
    version: &str,
    sha256_hex: &str,
    tarball_bytes: Vec<u8>,
) -> MockServer {
    let server = MockServer::start().await;
    let blob_path = format!("/blob/{id}/{version}");
    let signed_url = format!("{}{}?download=1&sig=ABC", server.uri(), blob_path);

    let (owner, name) = id.split_once('/').expect("id has /");

    // List/detail endpoint — versions[] WITHOUT tarballUrl (mirrors
    // production; signed URLs are minted by the per-version route only).
    let detail_body = json!({
        "id": id,
        "kind": "skill",
        "description": "test skill",
        "latestVersion": version,
        "versions": [
            {
                "version": version,
                "sha256": sha256_hex,
                "sizeBytes": tarball_bytes.len()
            }
        ]
    });
    Mock::given(method("GET"))
        .and(wm_path(format!("/api/v1/packages/{owner}/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(detail_body))
        .mount(&server)
        .await;

    // Per-version endpoint — this is what carries the signed tarballUrl.
    let version_body = json!({
        "id": id,
        "version": version,
        "sha256": sha256_hex,
        "sizeBytes": tarball_bytes.len(),
        "publishedAt": "2026-05-22T00:00:00Z",
        "deprecatedAt": null,
        "tarballUrl": signed_url,
    });
    Mock::given(method("GET"))
        .and(wm_path(format!(
            "/api/v1/packages/{owner}/{name}/{version}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(version_body))
        .mount(&server)
        .await;

    // Blob endpoint.
    Mock::given(method("GET"))
        .and(wm_path(blob_path))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(tarball_bytes)
                .insert_header("content-type", "application/gzip"),
        )
        .mount(&server)
        .await;

    server
}

/// Variant that mounts ONLY the list/detail endpoint with no
/// `tarballUrl`. Used by the regression test for PR #36 — proves the
/// resolver raises `omits tarballUrl` when the per-version endpoint
/// would have been the right call but the (legacy / broken) code
/// falls back to reading `versions[].tarballUrl` from the list page.
///
/// Even after the fix, this shape must error: the per-version endpoint
/// 404s here (we never mount it), and the resolver must surface that
/// as a missing-tarballUrl error rather than silently succeed.
async fn mock_pakx_registry_without_tarball_url(
    id: &str,
    version: &str,
    sha256_hex: &str,
    tarball_len: usize,
) -> MockServer {
    let server = MockServer::start().await;
    let (owner, name) = id.split_once('/').expect("id has /");

    let detail_body = json!({
        "id": id,
        "kind": "skill",
        "description": "test skill",
        "latestVersion": version,
        "versions": [
            {
                "version": version,
                "sha256": sha256_hex,
                "sizeBytes": tarball_len
            }
        ]
    });
    Mock::given(method("GET"))
        .and(wm_path(format!("/api/v1/packages/{owner}/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(detail_body))
        .mount(&server)
        .await;

    // Per-version endpoint returns version metadata WITHOUT tarballUrl
    // — the exact regression PR #36 introduced. The resolver must error
    // with `omits tarballUrl` rather than panicking or silently
    // succeeding.
    let version_body = json!({
        "id": id,
        "version": version,
        "sha256": sha256_hex,
        "sizeBytes": tarball_len,
        "publishedAt": "2026-05-22T00:00:00Z",
        "deprecatedAt": null
        // intentionally NO tarballUrl
    });
    Mock::given(method("GET"))
        .and(wm_path(format!(
            "/api/v1/packages/{owner}/{name}/{version}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(version_body))
        .mount(&server)
        .await;

    server
}

/// Seed a project root with `agents.yml` declaring `id@version` as a
/// `skills:` dep. We don't go through `pakx add` because that triggers
/// MCP validation; writing the YAML directly keeps the test focused
/// on the install path.
fn seed_skill_manifest(project: &TempDir, id: &str, version: &str) {
    let manifest =
        format!("name: demo\nversion: 0.0.0\ndependencies:\n  skills:\n    - {id}@{version}\n");
    std::fs::write(project.path().join("agents.yml"), manifest).unwrap();
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

/// Tarball happy path: registry returns a real signed-URL pointing at
/// a real gzipped tarball, sha256 matches, extract lands files under
/// `<claude_home>/skills/<owner>-<name>/`, lockfile records the
/// pakx-registry source + canonical (non-signed) URL.
#[tokio::test]
async fn install_skill_extracts_files_and_writes_lockfile() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/hello-world";
    let version = "0.1.1";

    let (tarball, sha) = build_test_tarball(&[
        TarEntry::file(
            "SKILL.md",
            b"---\nname: hello-world\nversion: 0.1.1\n---\n# hi\n",
        ),
        TarEntry::file("reference/notes.md", b"# Notes\n"),
    ]);
    let server = mock_pakx_skill_registry(id, version, &sha, tarball).await;

    seed_skill_manifest(&project, id, version);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
            // Force OfficialMcp into "empty" by pointing at the same
            // server (it won't have /v0/servers mocks → 404). MCP
            // deps aren't present in this manifest so the source
            // never gets queried; the override just keeps the
            // command hermetic.
            "--mcp-base-url",
            &server.uri(),
        ])
        .assert()
        .success();

    let extracted_root = claude_home.path().join("skills").join("alice-hello-world");
    assert!(
        extracted_root.join("SKILL.md").is_file(),
        "SKILL.md should be extracted"
    );
    assert!(
        extracted_root.join("reference").join("notes.md").is_file(),
        "nested file should be extracted"
    );

    let lock_body = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let lock: Value = serde_json::from_str(&lock_body).unwrap();
    let key = format!("skills/{id}@{version}");
    let entry = &lock["entries"][&key];
    assert_eq!(entry["name"], id);
    assert_eq!(entry["version"], version);
    assert_eq!(
        entry["registry"], "pakx",
        "lockfile must record pakx registry source"
    );
    let resolved_from = entry["resolvedFrom"].as_str().unwrap();
    assert!(
        !resolved_from.contains('?'),
        "canonical URL must strip signed query: {resolved_from}"
    );
    assert!(
        resolved_from.contains("/api/v1/packages/alice/hello-world/0.1.1"),
        "canonical URL must point at the registry, got: {resolved_from}"
    );
    let integrity = entry["integrity"].as_str().unwrap();
    assert!(
        integrity.starts_with("sha256-"),
        "integrity must be SRI-shape: {integrity}"
    );
}

// ---------------------------------------------------------------------------
// Sha256 mismatch
// ---------------------------------------------------------------------------

/// If the API's declared sha256 disagrees with the downloaded bytes,
/// install must fail loudly with an integrity-mismatch error. Tampering
/// or registry drift between metadata + blob storage is exactly the
/// case this guard is for.
#[tokio::test]
async fn install_skill_aborts_on_sha256_mismatch() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/mismatch";
    let version = "0.1.0";

    let (tarball, _real_sha) = build_test_tarball(&[TarEntry::file("SKILL.md", b"hello")]);
    let bogus_sha = "deadbeef".repeat(8); // 64 hex chars, not the real sha
    let server = mock_pakx_skill_registry(id, version, &bogus_sha, tarball).await;

    seed_skill_manifest(&project, id, version);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("integrity mismatch"));

    let extracted = claude_home.path().join("skills").join("alice-mismatch");
    assert!(
        !extracted.join("SKILL.md").exists(),
        "nothing must land on disk after integrity mismatch"
    );
}

// ---------------------------------------------------------------------------
// Zip-slip
// ---------------------------------------------------------------------------

/// Tarball entry whose path escapes the dest root via `..` must be
/// refused. The fixture entry path goes literally `../escape.md` so
/// the canonicalised destination would land outside the skills tree.
/// Our guard fires on the path-component scan before any FS write.
#[tokio::test]
async fn install_skill_refuses_zip_slip() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/escape";
    let version = "0.1.0";

    let (tarball, sha) = build_test_tarball(&[
        TarEntry::file("SKILL.md", b"hello"),
        TarEntry::file("../escape.md", b"evil"),
    ]);
    let server = mock_pakx_skill_registry(id, version, &sha, tarball).await;

    seed_skill_manifest(&project, id, version);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("escapes destination"));
}

/// Symlink entries in the tarball must be refused. Defense in depth:
/// `pakx pack` already refuses symlinks on the publish side, but a
/// hostile registry could still serve a tarball with a symlink that
/// points at `/etc/passwd`.
#[tokio::test]
async fn install_skill_refuses_symlink_entry() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/symlink";
    let version = "0.1.0";

    let (tarball, sha) = build_test_tarball(&[
        TarEntry::file("SKILL.md", b"hello"),
        TarEntry::symlink("evil", "/etc/passwd"),
    ]);
    let server = mock_pakx_skill_registry(id, version, &sha, tarball).await;

    seed_skill_manifest(&project, id, version);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("symlink"));
}

// ---------------------------------------------------------------------------
// Per-version endpoint regression (PR #36)
// ---------------------------------------------------------------------------

/// Regression: PR #36 wired the resolver to `GET /api/v1/packages/{owner}/{name}`
/// (the list/detail endpoint) which returns `versions[]` WITHOUT
/// `tarballUrl`. Live install against `arwenizEr/hello-world@0.1.1`
/// failed with `omits tarballUrl`. The fix is to call the per-version
/// endpoint, but the error message itself is still the correct
/// surface when the registry response actually omits the field.
///
/// This test mounts a per-version response without `tarballUrl` to
/// confirm the resolver still emits the precise diagnostic users will
/// search for in their logs.
#[tokio::test]
async fn install_skill_errors_when_registry_omits_tarball_url() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/no-url";
    let version = "0.1.0";

    let (tarball, sha) = build_test_tarball(&[TarEntry::file("SKILL.md", b"hello")]);
    let server = mock_pakx_registry_without_tarball_url(id, version, &sha, tarball.len()).await;

    seed_skill_manifest(&project, id, version);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("omits tarballUrl"));
}

// ---------------------------------------------------------------------------
// 50 MiB cap (decompressed)
// ---------------------------------------------------------------------------

/// `pakx install --json` emits a JSON array of per-entry rows on
/// stdout, mirroring the `pakx outdated --json` discipline. The
/// happy-path row carries `status: "ok"`, the entry's kind, and the
/// resolved version. Human progress + summary still render on stderr.
#[tokio::test]
async fn install_json_emits_per_entry_rows_with_status_ok() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/json-install";
    let version = "0.1.0";

    let (tarball, sha) = build_test_tarball(&[TarEntry::file(
        "SKILL.md",
        b"---\nname: json-install\nversion: 0.1.0\n---\n# hi\n",
    )]);
    let server = mock_pakx_skill_registry(id, version, &sha, tarball).await;
    seed_skill_manifest(&project, id, version);

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let body = stdout.trim_end_matches('\n');
    assert!(
        !body.contains('\n'),
        "json output must be single-line: {body:?}"
    );
    let v: Value = serde_json::from_str(body).expect("stdout is valid json");
    let arr = v.as_array().expect("top level is an array");
    assert_eq!(arr.len(), 1, "expected one entry, got: {arr:?}");
    let row = &arr[0];
    assert_eq!(row["id"], "alice/json-install");
    assert_eq!(row["status"], "ok");
    assert_eq!(row["kind"], "skills");
    assert_eq!(row["version"], "0.1.0");
    assert!(row.get("error").is_none(), "no error on success rows");
}

/// `pakx install --json` failure rows: integrity mismatch produces a
/// `status: "failed"` row with the rendered reason in `error`, and
/// the command still exits non-zero (matches the human path).
#[tokio::test]
async fn install_json_emits_failed_row_on_integrity_mismatch() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/json-fail";
    let version = "0.1.0";

    let (tarball, _real_sha) = build_test_tarball(&[TarEntry::file("SKILL.md", b"hi")]);
    let bogus_sha = "deadbeef".repeat(8);
    let server = mock_pakx_skill_registry(id, version, &bogus_sha, tarball).await;
    seed_skill_manifest(&project, id, version);

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let body = stdout.trim_end_matches('\n');
    let v: Value = serde_json::from_str(body).expect("stdout is valid json");
    let arr = v.as_array().expect("top level is an array");
    assert_eq!(arr.len(), 1);
    let row = &arr[0];
    assert_eq!(row["status"], "failed");
    assert_eq!(row["kind"], "skills");
    let err = row["error"]
        .as_str()
        .expect("error string present on failed row");
    assert!(
        err.contains("integrity mismatch"),
        "error should mention integrity mismatch: {err}"
    );
}

/// `--no-cache` flag must parse + thread through to the runner's
/// cache builders. Pin the surface so the flag stays advertised even
/// if a future refactor moves the cache wiring.
#[tokio::test]
async fn install_accepts_no_cache_flag() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/no-cache";
    let version = "0.1.0";

    let (tarball, sha) = build_test_tarball(&[TarEntry::file(
        "SKILL.md",
        b"---\nname: no-cache\nversion: 0.1.0\n---\n# hi\n",
    )]);
    let server = mock_pakx_skill_registry(id, version, &sha, tarball).await;
    seed_skill_manifest(&project, id, version);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
            "--no-cache",
        ])
        .assert()
        .success();
}

/// Decompressed-bomb guard: a tarball where the **decompressed** sum
/// of payloads exceeds 50 MiB must error. We build the tarball with
/// gzip-default compression over an incompressible (random) payload
/// so the compressed size stays under the *download* cap while the
/// decompressed total trips the second guard. Without this guard, a
/// nominally-small tarball could explode disk usage on extract.
#[tokio::test]
async fn install_skill_refuses_decompressed_bomb() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/bomb";
    let version = "0.1.0";

    // We build entries totalling > 50 MiB of compressible content
    // (the download cap doesn't fire because gzip squashes long
    // runs of zeros). We use 52 MiB of zeros split across two
    // entries so that the first entry-write stays under the cap but
    // the second cross-checks the running total. Compressed size of
    // zeros is ~5 KiB; far under the download cap.
    let chunk = vec![0u8; 30 * 1024 * 1024];
    let (tarball, sha) = build_test_tarball(&[
        TarEntry::file("SKILL.md", &chunk),
        TarEntry::file("more.bin", &chunk),
    ]);
    let server = mock_pakx_skill_registry(id, version, &sha, tarball).await;

    seed_skill_manifest(&project, id, version);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("50 MiB cap"));
}
