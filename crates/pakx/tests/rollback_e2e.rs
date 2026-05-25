//! Integration tests for `pakx install --rollback-on-error` (PM-5).
//!
//! Each test drives the real `pakx` binary against a wiremock that
//! serves one or more skill packages, using the hidden `--pakx-base-url`
//! + `--claude-home` overrides so nothing touches the real home dir.
//!
//! NB: this file is named `rollback_e2e.rs` rather than the more obvious
//! `install_rollback.rs` on purpose. Windows' installer-detection
//! heuristic (UAC "this looks like a setup program" elevation) fires on
//! any executable whose name contains `install` / `setup` / `update`,
//! and `cargo test` names the per-file test binary after the file. A
//! file called `install_rollback.rs` produces `install_rollback-*.exe`,
//! which the OS then refuses to launch without elevation (os error 740).
//! Keeping `install` out of the test-binary name sidesteps that.
//!
//! Scenarios covered:
//!   a. multi-dep install with the flag set, all deps good → everything
//!      installs (no spurious rollback);
//!   b. one good + one bad (sha mismatch) dep with the flag set →
//!      everything is reverted (newly-created dirs removed, pre-existing
//!      dirs restored to prior contents);
//!   c. the same failure WITHOUT the flag → the partial install survives
//!      (regression guard on the historical default);
//!   d. rollback restores the prior *contents* of a dir that pre-existed
//!      (not merely its presence).

use assert_cmd::Command;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

// ---------------------------------------------------------------------------
// Tarball helper (high-level builder; these tests only need well-formed
// tarballs — the zip-slip / symlink shapes live in `skills_e2e.rs`).
// ---------------------------------------------------------------------------

/// Build a gzipped tarball from `(path, contents)` pairs. Returns
/// `(bytes, sha256_hex)`.
fn build_tarball(entries: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let mut buf = Vec::new();
    {
        let enc = GzEncoder::new(&mut buf, Compression::default());
        let mut builder = tar::Builder::new(enc);
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *body).unwrap();
        }
        let enc = builder.into_inner().unwrap();
        enc.finish().unwrap();
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

// ---------------------------------------------------------------------------
// Mock server: mount one skill package (detail + per-version + blob).
// `declared_sha` is what the registry advertises — pass a bogus value to
// force an integrity-mismatch failure on the install side.
// ---------------------------------------------------------------------------

async fn mount_skill(
    server: &MockServer,
    id: &str,
    version: &str,
    declared_sha: &str,
    tarball: Vec<u8>,
) {
    let (owner, name) = id.split_once('/').expect("id has /");
    let blob_path = format!("/blob/{id}/{version}");
    let signed_url = format!("{}{}?download=1&sig=ABC", server.uri(), blob_path);

    let detail_body = json!({
        "id": id,
        "kind": "skill",
        "description": "test skill",
        "latestVersion": version,
        "versions": [{ "version": version, "sha256": declared_sha, "sizeBytes": tarball.len() }]
    });
    Mock::given(method("GET"))
        .and(wm_path(format!("/api/v1/packages/{owner}/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(detail_body))
        .mount(server)
        .await;

    let version_body = json!({
        "id": id,
        "version": version,
        "sha256": declared_sha,
        "sizeBytes": tarball.len(),
        "publishedAt": "2026-05-22T00:00:00Z",
        "deprecatedAt": null,
        "tarballUrl": signed_url,
    });
    Mock::given(method("GET"))
        .and(wm_path(format!(
            "/api/v1/packages/{owner}/{name}/{version}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(version_body))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(wm_path(blob_path))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(tarball)
                .insert_header("content-type", "application/gzip"),
        )
        .mount(server)
        .await;
}

/// Seed `agents.yml` with the given pinned skill deps.
fn seed_manifest(project: &TempDir, deps: &[(&str, &str)]) {
    use std::fmt::Write as _;
    let mut body = String::from("name: demo\nversion: 0.0.0\ndependencies:\n  skills:\n");
    for (id, version) in deps {
        let _ = writeln!(body, "    - {id}@{version}");
    }
    std::fs::write(project.path().join("agents.yml"), body).unwrap();
}

/// Install-dir leaf for a skill id (`owner/name` → `owner-name`).
fn skill_dir(claude_home: &TempDir, id: &str) -> std::path::PathBuf {
    let leaf = id.replace('/', "-");
    claude_home.path().join("skills").join(leaf)
}

#[allow(clippy::too_many_arguments)]
fn run_install(
    project: &TempDir,
    claude_home: &TempDir,
    server_uri: &str,
    rollback: bool,
) -> assert_cmd::assert::Assert {
    let mut cmd = Command::cargo_bin(BIN).unwrap();
    cmd.current_dir(project.path()).args([
        "install",
        "--pakx-base-url",
        server_uri,
        "--no-smithery",
        "--mcp-base-url",
        server_uri,
        "--claude-home",
        claude_home.path().to_str().unwrap(),
    ]);
    if rollback {
        cmd.arg("--rollback-on-error");
    }
    cmd.assert()
}

// ---------------------------------------------------------------------------
// (a) Successful multi-dep install with the flag set: everything lands,
//     nothing is rolled back.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rollback_flag_leaves_all_installed_on_full_success() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let server = MockServer::start().await;

    let (tar_a, sha_a) = build_tarball(&[("SKILL.md", b"# a")]);
    let (tar_b, sha_b) = build_tarball(&[("SKILL.md", b"# b")]);
    mount_skill(&server, "alice/one", "0.1.0", &sha_a, tar_a).await;
    mount_skill(&server, "alice/two", "0.1.0", &sha_b, tar_b).await;

    seed_manifest(&project, &[("alice/one", "0.1.0"), ("alice/two", "0.1.0")]);

    run_install(&project, &claude_home, &server.uri(), true).success();

    assert!(
        skill_dir(&claude_home, "alice/one")
            .join("SKILL.md")
            .is_file(),
        "first dep should be installed"
    );
    assert!(
        skill_dir(&claude_home, "alice/two")
            .join("SKILL.md")
            .is_file(),
        "second dep should be installed"
    );
    assert!(
        project.path().join("agents.lock").is_file(),
        "lockfile written on clean run"
    );
}

// ---------------------------------------------------------------------------
// (b) Failing install WITH the flag: newly-created dirs are removed and a
//     pre-existing dir is restored to its prior contents.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rollback_reverts_partial_install_on_failure() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let server = MockServer::start().await;

    // `alice/good` installs cleanly; `alice/bad` advertises a bogus sha
    // so its install fails after `good` already landed.
    let (tar_good, sha_good) = build_tarball(&[("SKILL.md", b"# good")]);
    let (tar_bad, _real) = build_tarball(&[("SKILL.md", b"# bad")]);
    let bogus = "deadbeef".repeat(8);
    mount_skill(&server, "alice/good", "0.1.0", &sha_good, tar_good).await;
    mount_skill(&server, "alice/bad", "0.1.0", &bogus, tar_bad).await;

    // A pre-existing, UNRELATED install that the run does NOT touch must
    // survive untouched (snapshot only covers declared deps).
    let bystander = claude_home.path().join("skills").join("eve-bystander");
    std::fs::create_dir_all(&bystander).unwrap();
    std::fs::write(bystander.join("SKILL.md"), b"untouched").unwrap();

    // Order matters: `good` is dispatched before `bad` (dispatch order is
    // the manifest order within a kind), so `good` lands first.
    seed_manifest(&project, &[("alice/good", "0.1.0"), ("alice/bad", "0.1.0")]);

    run_install(&project, &claude_home, &server.uri(), true).failure();

    assert!(
        !skill_dir(&claude_home, "alice/good").exists(),
        "the successfully-installed dep must be rolled back (dir removed)"
    );
    assert!(
        !skill_dir(&claude_home, "alice/bad").exists(),
        "the failed dep must leave nothing behind"
    );
    assert!(
        !project.path().join("agents.lock").exists(),
        "no lockfile on a failed/rolled-back run"
    );
    // Bystander untouched.
    assert_eq!(
        std::fs::read(bystander.join("SKILL.md")).unwrap(),
        b"untouched",
        "unrelated pre-existing install must not be disturbed"
    );
}

// ---------------------------------------------------------------------------
// (c) Failing install WITHOUT the flag: the partial install survives
//     (regression guard on the historical default behavior).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_flag_leaves_partial_install_on_failure() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let server = MockServer::start().await;

    let (tar_good, sha_good) = build_tarball(&[("SKILL.md", b"# good")]);
    let (tar_bad, _real) = build_tarball(&[("SKILL.md", b"# bad")]);
    let bogus = "deadbeef".repeat(8);
    mount_skill(&server, "alice/good", "0.1.0", &sha_good, tar_good).await;
    mount_skill(&server, "alice/bad", "0.1.0", &bogus, tar_bad).await;

    seed_manifest(&project, &[("alice/good", "0.1.0"), ("alice/bad", "0.1.0")]);

    // No `--rollback-on-error`.
    run_install(&project, &claude_home, &server.uri(), false).failure();

    assert!(
        skill_dir(&claude_home, "alice/good")
            .join("SKILL.md")
            .is_file(),
        "default behavior: the good dep stays installed even when a later dep fails"
    );
    assert!(
        !skill_dir(&claude_home, "alice/bad").exists(),
        "the failed dep itself still leaves nothing behind"
    );
}

// ---------------------------------------------------------------------------
// (d) Rollback restores the prior CONTENTS of a dir that pre-existed —
//     a reinstall-over-existing scenario where one dep in the run fails.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rollback_restores_prior_contents_of_preexisting_dir() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let server = MockServer::start().await;

    // `alice/good` is being RE-installed: a prior version already sits on
    // disk with a distinctive marker file. The run's other dep fails, so
    // rollback must bring the prior contents back byte-for-byte.
    let prior = skill_dir(&claude_home, "alice/good");
    std::fs::create_dir_all(&prior).unwrap();
    std::fs::write(prior.join("OLD.md"), b"prior-version").unwrap();

    let (tar_good, sha_good) = build_tarball(&[("NEW.md", b"# new content")]);
    let (tar_bad, _real) = build_tarball(&[("SKILL.md", b"# bad")]);
    let bogus = "deadbeef".repeat(8);
    mount_skill(&server, "alice/good", "0.2.0", &sha_good, tar_good).await;
    mount_skill(&server, "alice/bad", "0.1.0", &bogus, tar_bad).await;

    seed_manifest(&project, &[("alice/good", "0.2.0"), ("alice/bad", "0.1.0")]);

    run_install(&project, &claude_home, &server.uri(), true).failure();

    assert!(
        prior.join("OLD.md").is_file(),
        "prior contents must be restored on rollback"
    );
    assert_eq!(
        std::fs::read(prior.join("OLD.md")).unwrap(),
        b"prior-version",
        "restored contents must be the prior bytes, not the failed run's"
    );
    assert!(
        !prior.join("NEW.md").exists(),
        "the failed run's freshly-written contents must be wiped"
    );
}
