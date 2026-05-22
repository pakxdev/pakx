//! Integration tests for `pakx pack`.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const BIN: &str = "pakx";

fn write_min_skill(dir: &std::path::Path, name: &str, version: &str) {
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\nversion: {version}\n---\n# Hi\n"),
    )
    .unwrap();
}

#[test]
fn pack_succeeds_on_plain_directory() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_min_skill(src.path(), "demo", "0.1.0");
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--out",
            out.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(out.path().join("demo-0.1.0.tgz").is_file());
}

/// A skill template author could include a symlink to `~/.ssh/id_rsa` or
/// `/etc/shadow` in the source tree. `pakx pack` must refuse — silently
/// skipping would hide the surprise; following the link would exfiltrate
/// host secrets into the tarball that `pakx publish` uploads next.
#[cfg(unix)]
#[test]
fn pack_refuses_symlinks_under_source_tree() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let secrets = TempDir::new().unwrap();
    write_min_skill(src.path(), "demo", "0.1.0");

    let target = secrets.path().join("id_rsa");
    std::fs::write(&target, b"PRETEND PRIVATE KEY").unwrap();
    std::os::unix::fs::symlink(&target, src.path().join("leaked.pem")).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--out",
            out.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("symlinks under SKILL.md src/"));

    assert!(
        !out.path().join("demo-0.1.0.tgz").is_file(),
        "tarball must not be written when a symlink is present"
    );
}

/// Windows-equivalent: symlinks require elevation to create on Windows,
/// so this only runs when the test harness has the privilege. Skipped
/// otherwise (returning early keeps CI green on developer machines that
/// run unprivileged).
#[cfg(windows)]
#[test]
fn pack_refuses_symlinks_under_source_tree() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let secrets = TempDir::new().unwrap();
    write_min_skill(src.path(), "demo", "0.1.0");

    let target = secrets.path().join("secret.txt");
    std::fs::write(&target, b"PRETEND SECRET").unwrap();
    if std::os::windows::fs::symlink_file(&target, src.path().join("leaked.txt")).is_err() {
        // Unprivileged Windows: cannot create symlinks. Skip silently —
        // the unix variant covers the regression on CI.
        eprintln!("skipping: symlink_file requires SeCreateSymbolicLinkPrivilege");
        return;
    }

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--out",
            out.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("symlinks under SKILL.md src/"));

    assert!(
        !out.path().join("demo-0.1.0.tgz").is_file(),
        "tarball must not be written when a symlink is present"
    );
}
