//! Integration tests for `pakx update` covering surface-level guards
//! that don't need a full registry mock.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const BIN: &str = "pakx";

/// Round 39 fix: `validate_base_url(--pakx-base-url)` now fires at the
/// TOP of `run` BEFORE any registry probe / outdated render. Pin the
/// behaviour with a userinfo-smuggle attempt — the rejection must
/// surface as the only output (no `gather_outdated` per-entry stderr
/// noise preceding it).
#[test]
fn update_rejects_pakx_base_url_with_smuggled_userinfo() {
    let project = TempDir::new().unwrap();
    // Seed a manifest so `pakx update` doesn't bail with "no manifest"
    // before our guard fires. The lockfile is intentionally absent so
    // even if validation slipped, we'd hit a different stderr line —
    // making the validate-first assertion unambiguous.
    std::fs::write(
        project.path().join("agents.yml"),
        "name: demo\nversion: 0.0.0\ndependencies:\n  skills:\n    - alice/hello@0.1.0\n",
    )
    .unwrap();

    let assertion = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "alice/hello",
            "--pakx-base-url",
            "http://localhost:8080@evil.com/",
            "--yes",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("refusing to use registry base URL"),
        "validation rejection should surface as the only error: {stderr}"
    );
    // Negative: the outdated probe must NOT have rendered any per-entry
    // warning line ahead of the validation. The presence of
    // `[warn] alice/hello: ...` would indicate the validate-after-
    // probe regression has reappeared.
    assert!(
        !stderr.contains("[warn] alice/hello"),
        "validation must precede per-entry probes: {stderr}",
    );
}

/// Same guard, applied to `--mcp-base-url` — keeps the three URL flags
/// in lock-step. A future code path that quietly skips one of them
/// trips this test.
#[test]
fn update_rejects_mcp_base_url_with_plaintext_http() {
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("agents.yml"),
        "name: demo\nversion: 0.0.0\n",
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["update", "--mcp-base-url", "http://evil.com", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to use registry base URL",
        ));
}
