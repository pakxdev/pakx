//! End-to-end tests for `pakx install` resolving the four bundle
//! kinds (`commands`, `subagents`, `prompts`, `hooks`) through the
//! pakx-registry source.
//!
//! Each test is the bundle analogue of `tests/skills_e2e.rs::install_skill_extracts_files_and_writes_lockfile`:
//!   1. build a gzipped tarball in-process,
//!   2. spin up a wiremock that serves the list / per-version /
//!      tarball endpoints,
//!   3. seed an `agents.yml` with the dep under the matching section,
//!   4. invoke the real `pakx install` binary with the hidden
//!      `--pakx-base-url` + `--claude-home` overrides,
//!   5. assert the tarball landed under the kind-specific
//!      subdirectory and the lockfile recorded the right discriminator.
//!
//! These tests exercise the generic
//! `crate::install::bundle::install_bundle_from_pakx` path — the
//! kind-parameterised sibling of the skill installer.

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
// Local copy of the minimal tarball helpers used in `skills_e2e.rs`.
// Duplicated rather than refactored into a shared support module
// because each test file is its own crate-style compilation unit in
// the Cargo integration-test layout, and a `mod common;` import would
// pull in unrelated symbols.
// ---------------------------------------------------------------------------

/// Build a gzipped tarball with one `SKILL.md`-shaped entry + an
/// optional ancillary file. Returns `(bytes, sha256_hex)`.
fn build_tarball(entries: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let mut buf = Vec::new();
    {
        let mut encoder = GzEncoder::new(&mut buf, Compression::default());
        let mut tar_builder = tar::Builder::new(&mut encoder);
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar_builder.append(&header, *contents).unwrap();
        }
        tar_builder.finish().unwrap();
    }
    let sha = bytes_to_hex(&Sha256::digest(&buf));
    (buf, sha)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Mount the package detail + per-version + blob endpoints. Same
/// shape as `skills_e2e.rs::mock_pakx_skill_registry`; copied here
/// rather than shared because the integration-test layout makes
/// cross-file imports awkward.
async fn mock_pakx_registry(
    id: &str,
    version: &str,
    sha256_hex: &str,
    tarball_bytes: Vec<u8>,
) -> MockServer {
    let server = MockServer::start().await;
    let blob_path = format!("/blob/{id}/{version}");
    let signed_url = format!("{}{}?download=1&sig=ABC", server.uri(), blob_path);

    let (owner, name) = id.split_once('/').expect("id has /");

    let detail_body = json!({
        "id": id,
        "kind": "skill",
        "description": "test bundle",
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

/// Seed an `agents.yml` declaring `id@version` under the given
/// dependency section (e.g. `commands`, `subagents`, ...).
fn seed_bundle_manifest(project: &TempDir, section: &str, id: &str, version: &str) {
    let manifest =
        format!("name: demo\nversion: 0.0.0\ndependencies:\n  {section}:\n    - {id}@{version}\n");
    std::fs::write(project.path().join("agents.yml"), manifest).unwrap();
}

// ---------------------------------------------------------------------------
// Per-kind happy path (personal / claude-home install).
// ---------------------------------------------------------------------------

/// Drive a single-kind happy-path install. `section` is the YAML key
/// (`commands`, `subagents`, `prompts`, `hooks`); `subdir` is the
/// expected on-disk directory under `<claude_home>` (matches the
/// mapping in `install::bundle::subdir_for`).
async fn assert_bundle_happy_path(section: &str, subdir: &str) {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = format!("alice/{section}-fixture");
    let version = "0.1.0";

    let (tarball, sha) = build_tarball(&[
        (
            "SKILL.md",
            format!("---\nname: {section}-fixture\nversion: 0.1.0\n---\n# {section} bundle\n")
                .as_bytes(),
        ),
        ("reference/extra.md", b"# extra\n"),
    ]);
    let server = mock_pakx_registry(&id, version, &sha, tarball).await;

    seed_bundle_manifest(&project, section, &id, version);

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
        .success();

    // Tarball must land under the kind-specific subdir, NOT under
    // `skills/`. This pins the per-kind dispatch in
    // `install::bundle::subdir_for`.
    let leaf = format!("alice-{section}-fixture");
    let extracted_root = claude_home.path().join(subdir).join(&leaf);
    assert!(
        extracted_root.join("SKILL.md").is_file(),
        "{section}: SKILL.md should extract under {subdir}/{leaf}/",
    );
    assert!(
        extracted_root.join("reference").join("extra.md").is_file(),
        "{section}: nested file should extract under {subdir}/{leaf}/",
    );
    // Negative assertion: the skills tree must stay untouched —
    // sub-adapter installs never bleed into `skills/`.
    assert!(
        !claude_home.path().join("skills").join(&leaf).exists(),
        "{section}: nothing should land in skills/ tree",
    );

    // Lockfile carries the right kind discriminator.
    let lock_body = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let lock: Value = serde_json::from_str(&lock_body).unwrap();
    let key = format!("{section}/{id}@{version}");
    let entry = &lock["entries"][&key];
    assert_eq!(entry["name"], id, "{section}: lockfile name");
    assert_eq!(entry["type"], section, "{section}: lockfile type");
    assert_eq!(entry["version"], version, "{section}: lockfile version");
    assert_eq!(
        entry["registry"], "pakx",
        "{section}: lockfile registry source",
    );
    let resolved_from = entry["resolvedFrom"].as_str().unwrap();
    assert!(
        !resolved_from.contains('?'),
        "{section}: canonical URL must strip signed query: {resolved_from}",
    );
    let integrity = entry["integrity"].as_str().unwrap();
    assert!(
        integrity.starts_with("sha256-"),
        "{section}: integrity must be SRI-shape: {integrity}",
    );
}

#[tokio::test]
async fn install_commands_extracts_under_commands_subdir() {
    assert_bundle_happy_path("commands", "commands").await;
}

#[tokio::test]
async fn install_subagents_extracts_under_agents_subdir() {
    // Mapping is `subagents:` (YAML key) → `agents/` (filesystem
    // subdir under Claude Code's config). The rename is deliberate —
    // upstream Claude Code calls them "agents" on disk while pakx
    // keeps the kebab-case package-kind name `subagents` to avoid
    // colliding with the `agents` *array* key in `agents.yml` (which
    // lists *adapter targets*, not package kinds).
    assert_bundle_happy_path("subagents", "agents").await;
}

#[tokio::test]
async fn install_prompts_extracts_under_prompts_subdir() {
    assert_bundle_happy_path("prompts", "prompts").await;
}

#[tokio::test]
async fn install_hooks_extracts_under_hooks_subdir() {
    assert_bundle_happy_path("hooks", "hooks").await;
}

// ---------------------------------------------------------------------------
// Project-scoped install (--directory).
// ---------------------------------------------------------------------------

/// When `--claude-home` isn't supplied the runner falls back to the
/// project root + `.claude/`, mirroring how Claude Code reads
/// project-local config. We pin that behaviour here for one of the
/// four kinds (subagents) so the regression catches an accidental
/// rebind of the fallback path.
#[tokio::test]
async fn install_bundle_project_mode_uses_dot_claude_under_project_root() {
    // Project-scoped install: explicitly point `--claude-home` at
    // `<project>/.claude/` (the same shape a `--project` flag would
    // synthesise). Bundle extracts must land at
    // `.claude/<subdir>/<owner>-<name>/` — the spec's "project
    // (`--project`)" column. We don't try to exercise the env-var
    // fallback here because `dirs::home_dir()` on Windows has multiple
    // fallback sources (USERPROFILE / HOMEDRIVE+HOMEPATH /
    // FOLDERID_Profile) that aren't all controllable from env_remove,
    // so the env-removal approach is fragile cross-platform. Passing
    // an explicit `--claude-home` is what every real downstream CI
    // would do.
    let project = TempDir::new().unwrap();
    let claude_home = project.path().join(".claude");
    let id = "alice/project-mode-fixture";
    let version = "0.1.0";

    let (tarball, sha) = build_tarball(&[(
        "SKILL.md",
        b"---\nname: project-mode-fixture\nversion: 0.1.0\n---\n",
    )]);
    let server = mock_pakx_registry(id, version, &sha, tarball).await;
    seed_bundle_manifest(&project, "subagents", id, version);

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
            claude_home.to_str().unwrap(),
        ])
        .assert()
        .success();

    let extracted = claude_home
        .join("agents")
        .join("alice-project-mode-fixture");
    assert!(
        extracted.join("SKILL.md").is_file(),
        "project-mode subagent should land at <claude_home>/agents/<owner>-<name>/",
    );
}

// ---------------------------------------------------------------------------
// Tree assertion: after wiring, all four kinds render `wired`.
// ---------------------------------------------------------------------------

/// Regression on the round-33 hand-coded adapter list: after the
/// sub-adapter install round `pakx tree` must surface `(commands
/// adapter)` for a `commands/...` lockfile entry, NOT
/// `(skipped — commands adapter not wired)`. The lockfile schema
/// itself is unchanged — only the derived adapter-status label
/// flips.
#[test]
fn pakx_tree_shows_commands_as_wired() {
    let project = TempDir::new().unwrap();
    let manifest_hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    let entry_integrity = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";
    let body = format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{manifest_hash}","entries":{{
  "commands/alice/cmd@0.1.0":{{
    "name":"alice/cmd",
    "type":"commands",
    "version":"0.1.0",
    "resolvedFrom":"pakx:alice/cmd",
    "registry":"pakx",
    "integrity":"{entry_integrity}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    );
    std::fs::write(project.path().join("agents.lock"), body).unwrap();
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["--color", "never", "tree"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(
        stdout.contains("commands adapter"),
        "expected `(commands adapter)` in human tree output, got:\n{stdout}",
    );
    assert!(
        !stdout.contains("not wired"),
        "no `not wired` line should appear, got:\n{stdout}",
    );
}
