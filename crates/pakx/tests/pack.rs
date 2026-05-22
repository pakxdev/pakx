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

fn write_skill_with_frontmatter(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("SKILL.md"), format!("---\n{body}---\n# Hi\n")).unwrap();
}

#[test]
fn pack_accepts_valid_sponsors_block() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: demo\nversion: 0.1.0\nsponsors:\n  - kind: github\n    url: https://github.com/sponsors/octocat\n  - kind: url\n    url: https://opencollective.com/octocat\n",
    );
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

#[test]
fn pack_rejects_malformed_github_sponsor_url() {
    // Sponsor regex requires `github.com/sponsors/<login>` host-anchored;
    // a `gitlab.com` URL under `kind: github` must trip pack-time
    // validation with the offending index in the message.
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: demo\nversion: 0.1.0\nsponsors:\n  - kind: github\n    url: https://gitlab.com/sponsors/octocat\n",
    );
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
        .stderr(predicate::str::contains("sponsors[0].url"));
    assert!(
        !out.path().join("demo-0.1.0.tgz").is_file(),
        "tarball must not be written when sponsors validation fails"
    );
}

#[test]
fn pack_rejects_too_many_sponsors() {
    use std::fmt::Write as _;
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let mut body = String::from("name: demo\nversion: 0.1.0\nsponsors:\n");
    for i in 0..6 {
        let _ = write!(
            body,
            "  - kind: github\n    url: https://github.com/sponsors/octocat{i}\n"
        );
    }
    write_skill_with_frontmatter(src.path(), &body);
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
        .stderr(predicate::str::contains("too many entries"));
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
