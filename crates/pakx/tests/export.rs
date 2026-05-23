//! Integration tests for `pakx export <id>`.
//!
//! Round-trip: stand up a wiremock-backed pakx-registry, install a
//! skill into a tempdir-scoped `--claude-home`, then run `pakx export
//! <id>` and assert the destination tree matches the install tree
//! file-for-file. Mirrors the wiremock + tarball discipline used by
//! `skills_e2e.rs` so the surface tested end-to-end is identical to
//! what real publishers see.

use std::collections::BTreeMap;
use std::io::Write;

use assert_cmd::Command;
use flate2::write::GzEncoder;
use flate2::Compression;
use predicates::prelude::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

// ---------------------------------------------------------------------------
// Tarball + registry helpers (lifted from `skills_e2e.rs`; the cargo
// integration-test layout makes each file its own compilation unit so a
// shared module would pull in unrelated symbols).
// ---------------------------------------------------------------------------

fn build_tarball(entries: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let mut buf = Vec::new();
    {
        let encoder = GzEncoder::new(&mut buf, Compression::default());
        let mut tar_builder = tar::Builder::new(encoder);
        for (p, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_path(p).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar_builder.append(&header, *contents).unwrap();
        }
        // Take the encoder back out and flush gzip footer in-place so
        // the buffer is a complete `.tgz` stream.
        let encoder = tar_builder.into_inner().unwrap();
        encoder.finish().unwrap();
    }
    let _ = std::io::sink().flush();
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

    let version_body = json!({
        "id": id,
        "version": version,
        "sha256": sha256_hex,
        "sizeBytes": tarball_bytes.len(),
        "publishedAt": "2026-05-23T00:00:00Z",
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

fn seed_skill_manifest(project: &TempDir, id: &str, version: &str) {
    let manifest =
        format!("name: demo\nversion: 0.0.0\ndependencies:\n  skills:\n    - {id}@{version}\n");
    std::fs::write(project.path().join("agents.yml"), manifest).unwrap();
}

/// Walk a directory tree and collect every file's relative path → byte
/// contents. Used to compare install vs export trees file-for-file.
fn snapshot_tree(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let ft = entry.file_type().unwrap();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Round-trip: install, then export, assert tree matches.
// ---------------------------------------------------------------------------

/// Happy path: install a skill into a tempdir-scoped Claude home,
/// then export it. The exported tree must match the install tree
/// file-for-file (same paths, same bytes), and the JSON payload must
/// carry `files` equal to the file count.
#[tokio::test]
async fn export_round_trips_installed_skill_tree() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/portable-skill";
    let version = "0.1.0";

    let (tarball, sha) = build_tarball(&[
        (
            "SKILL.md",
            b"---\nname: portable-skill\nversion: 0.1.0\n---\n# hi\n" as &[u8],
        ),
        ("reference/notes.md", b"# Notes\n"),
        ("reference/deep/nested.txt", b"deep\n"),
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
        .success();

    let install_dir = claude_home
        .path()
        .join("skills")
        .join("alice-portable-skill");
    assert!(install_dir.join("SKILL.md").is_file());
    let install_snap = snapshot_tree(&install_dir);

    // Export into a sibling-of-project tempdir to keep the test
    // hermetic. `--output` is absolute so we don't depend on cwd.
    let export_root = TempDir::new().unwrap();
    let dest = export_root.path().join("portable-skill");
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "export",
            id,
            "--output",
            dest.to_str().unwrap(),
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
    assert_eq!(
        usize::try_from(v["files"].as_u64().unwrap()).unwrap(),
        install_snap.len()
    );
    assert!(v["from"].as_str().unwrap().contains("portable-skill"));
    assert!(v["to"].as_str().unwrap().contains("portable-skill"));

    let export_snap = snapshot_tree(&dest);
    assert_eq!(
        export_snap, install_snap,
        "exported tree must match install tree file-for-file"
    );
}

/// Default output dir: when `--output` is omitted, export writes to
/// `<cwd>/<name-after-slash>`. Run the export from a fresh `current_dir`
/// (a sibling tempdir) so the assertion isn't entangled with the
/// project's own contents.
#[tokio::test]
async fn export_default_output_uses_name_after_slash() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/default-out";
    let version = "0.1.0";

    let (tarball, sha) = build_tarball(&[(
        "SKILL.md",
        b"---\nname: default-out\nversion: 0.1.0\n---\n# hi\n" as &[u8],
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
        ])
        .assert()
        .success();

    // Use the project tempdir as the export cwd; the default output
    // will land at `<project>/default-out/`.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "export",
            id,
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(
        project
            .path()
            .join("default-out")
            .join("SKILL.md")
            .is_file(),
        "default output should land at <cwd>/<name-after-slash>",
    );
}

/// Refuses to overwrite an existing destination unless `--force`.
#[tokio::test]
async fn export_refuses_existing_dest_without_force() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/overwrite";
    let version = "0.1.0";

    let (tarball, sha) = build_tarball(&[(
        "SKILL.md",
        b"---\nname: overwrite\nversion: 0.1.0\n---\n# hi\n" as &[u8],
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
        ])
        .assert()
        .success();

    let dest_root = TempDir::new().unwrap();
    let dest = dest_root.path().join("overwrite");
    std::fs::create_dir_all(&dest).unwrap();
    // Plant a pre-existing file so the post-fail assert can verify it
    // survived intact (no partial wipe).
    std::fs::write(dest.join("preexisting.txt"), b"keep me").unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "export",
            id,
            "--output",
            dest.to_str().unwrap(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
    assert!(
        dest.join("preexisting.txt").is_file(),
        "destination must be untouched on the no-force failure path"
    );

    // With --force, the prior tree is wiped + the export lands.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "export",
            id,
            "--output",
            dest.to_str().unwrap(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success();
    assert!(
        dest.join("SKILL.md").is_file(),
        "force-overwrite should land the export"
    );
    assert!(
        !dest.join("preexisting.txt").is_file(),
        "force-overwrite should wipe the prior tree"
    );
}

/// Missing lockfile → friendly error pointing at `pakx install`.
#[test]
fn export_errors_when_no_lockfile() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["export", "anyone/any"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no agents.lock"));
}

/// MCP entries cannot be exported because their install state lives in
/// `.mcp.json`, not in a per-package tree. The error message must call
/// out the kind so users know which command they wanted.
#[test]
fn export_refuses_mcp_kind_entry() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    // Hand-craft an `agents.lock` with one MCP entry. The runner won't
    // touch the FS for this id because we never run `pakx install` —
    // the test exercises the export-side rejection only.
    let lock = json!({
        "lockfileVersion": 1,
        "manifestHash": "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
        "entries": {
            "mcp/io.github.acme/srv@1.0.0": {
                "name": "io.github.acme/srv",
                "type": "mcp",
                "version": "1.0.0",
                "resolvedFrom": "official-mcp:io.github.acme/srv",
                "registry": "official-mcp",
                "integrity": "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
                "agents": ["claude-code"],
                "dependencies": []
            }
        }
    });
    std::fs::write(
        project.path().join("agents.lock"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "export",
            "io.github.acme/srv",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("MCP"));
}
