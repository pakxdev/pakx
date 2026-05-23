//! Full-lifecycle integration tests: `add` → `install` → `remove` →
//! `install` against wiremock-backed registries.
//!
//! These extend the round-trip surface from `add_install_roundtrip.rs`
//! with a *removal* step — exercising the path where the lockfile and
//! on-disk install tree must stay in sync as a dep is added and then
//! taken away. The bug shape this catches: a stale lockfile entry left
//! behind after `pakx remove` would silently re-install the dropped
//! dep on the next `pakx install`, or worse, fail to install a
//! different dep because the lockfile contradicted the manifest.

use assert_cmd::Command;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

fn build_tarball(entries: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let mut buf = Vec::new();
    {
        let mut encoder = GzEncoder::new(&mut buf, Compression::default());
        let mut tar_builder = tar::Builder::new(&mut encoder);
        for (p, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_path(p).unwrap();
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

async fn mock_pakx_skill(
    id: &str,
    version: &str,
    sha256_hex: &str,
    tarball_bytes: Vec<u8>,
) -> MockServer {
    let server = MockServer::start().await;
    let blob_path = format!("/blob/{id}/{version}");
    let signed_url = format!("{}{}?download=1&sig=ABC", server.uri(), blob_path);
    let (owner, name) = id.split_once('/').unwrap();

    Mock::given(method("GET"))
        .and(wm_path(format!("/api/v1/packages/{owner}/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id,
            "kind": "skills",
            "description": "lifecycle fixture",
            "latestVersion": version,
            "versions": [
                { "version": version, "sha256": sha256_hex, "sizeBytes": tarball_bytes.len() }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path(format!(
            "/api/v1/packages/{owner}/{name}/{version}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id,
            "version": version,
            "sha256": sha256_hex,
            "sizeBytes": tarball_bytes.len(),
            "publishedAt": "2026-05-22T00:00:00Z",
            "deprecatedAt": null,
            "tarballUrl": signed_url,
        })))
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

/// Full lifecycle on a skill: add → install → remove → install. After
/// the second install, the lockfile MUST drop the entry the manifest
/// no longer references. The disk tree under `<claude_home>/skills/`
/// is **not** swept by `pakx remove` at v0.1 (documented "stale files
/// stay on disk" behaviour); we only assert the lockfile reconciles.
#[tokio::test]
async fn lifecycle_add_install_remove_install_skills_lockfile_drops_entry() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/lifecycle-skill";
    let version = "0.1.0";

    let (tarball, sha) = build_tarball(&[(
        "SKILL.md",
        b"---\nname: lifecycle-skill\nversion: 0.1.0\n---\n",
    )]);
    let server = mock_pakx_skill(id, version, &sha, tarball).await;

    // 1. add
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", "skills", id, "--no-validate"])
        .assert()
        .success();

    // 2. install — lockfile gains the entry.
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

    let lock_after_install = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let v: Value = serde_json::from_str(&lock_after_install).unwrap();
    let key = format!("skills/{id}@{version}");
    assert!(
        v["entries"].get(&key).is_some(),
        "lockfile should have entry {key} after first install; got:\n{lock_after_install}"
    );

    // 3. remove
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["remove", id, "--kind", "skills", "--yes"])
        .assert()
        .success();

    // Sanity: manifest has no skills section anymore.
    let manifest_body = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    let manifest = pakx_core::parse_manifest(&manifest_body, None).unwrap();
    assert!(
        manifest.dependencies.skills.is_none(),
        "skills section should be pruned post-remove, got:\n{manifest_body}"
    );

    // 4. install again — lockfile should now be empty.
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

    let lock_after_remove = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let v2: Value = serde_json::from_str(&lock_after_remove).unwrap();
    assert!(
        v2["entries"].as_object().unwrap().is_empty(),
        "lockfile entries should be empty after remove + reinstall; got:\n{lock_after_remove}"
    );
}

/// Same lifecycle on an MCP server. `.mcp.json` is not pruned at v0.1
/// (Claude Code's mcp.json is shared across projects and removing
/// entries blindly would break unrelated installs) — we only assert
/// the lockfile reconciles.
#[tokio::test]
async fn lifecycle_add_install_remove_install_mcp_lockfile_drops_entry() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "io.github.acme/lifecycle-mcp";
    let version = "1.0.0";

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": id,
            "version_detail": { "version": version },
            "packages": [
                {
                    "registry_name": "npm",
                    "name": "@acme/mcp-lifecycle",
                    "version": version,
                    "environment_variables": []
                }
            ]
        })))
        .mount(&server)
        .await;

    // add
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", "mcp", id, "--mcp-base-url", &server.uri()])
        .assert()
        .success();

    // install
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let key = format!("mcp/{id}@{version}");
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(project.path().join("agents.lock")).unwrap())
            .unwrap();
    assert!(
        v["entries"].get(&key).is_some(),
        "lockfile should contain entry {key} after first install"
    );

    // remove
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["remove", id, "--kind", "mcp", "--yes"])
        .assert()
        .success();

    // install
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let v2: Value =
        serde_json::from_str(&std::fs::read_to_string(project.path().join("agents.lock")).unwrap())
            .unwrap();
    assert!(
        v2["entries"].as_object().unwrap().is_empty(),
        "lockfile entries should be empty after remove+install for mcp"
    );
}

/// Re-add the same dep after removing it. Catches a state-leak where
/// the runner caches the resolved dep across `add`/`remove` calls
/// inside one process — at the CLI surface this would manifest as
/// either a second `add` being a no-op (it ISN'T because the manifest
/// was rewritten) or the second `install` skipping the dep.
#[tokio::test]
async fn lifecycle_readd_after_remove_reinstalls_clean() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/readd-skill";
    let version = "0.1.0";

    let (tarball, sha) =
        build_tarball(&[("SKILL.md", b"---\nname: readd-skill\nversion: 0.1.0\n---\n")]);
    let server = mock_pakx_skill(id, version, &sha, tarball).await;

    let install_args: [&str; 9] = [
        "install",
        "--pakx-base-url",
        &server.uri(),
        "--no-smithery",
        "--mcp-base-url",
        &server.uri(),
        "--claude-home",
        claude_home.path().to_str().unwrap(),
        // Marker arg so the array shape stays even when args change.
        "--no-lockfile",
    ];
    // First add/install (with lockfile).
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", "skills", id, "--no-validate"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(&install_args[..install_args.len() - 1])
        .assert()
        .success();
    // remove
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["remove", id, "--kind", "skills", "--yes"])
        .assert()
        .success();
    // re-add
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", "skills", id, "--no-validate"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(&install_args[..install_args.len() - 1])
        .assert()
        .success();

    // Lockfile must again carry the entry (process is hermetic, but the
    // shared cache dir could in theory have stuck stale data).
    let key = format!("skills/{id}@{version}");
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(project.path().join("agents.lock")).unwrap())
            .unwrap();
    assert!(
        v["entries"].get(&key).is_some(),
        "re-added entry should appear in lockfile after second install"
    );

    // And the extracted file should still be on disk.
    let extracted = claude_home
        .path()
        .join("skills")
        .join("alice-readd-skill")
        .join("SKILL.md");
    assert!(
        extracted.is_file(),
        "SKILL.md should be re-extracted at {}",
        extracted.display()
    );
}
