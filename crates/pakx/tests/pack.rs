//! Integration tests for `pakx pack`.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
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

/// Notepad (and VSCode-on-Windows with the default LF→CRLF auto-fix)
/// saves SKILL.md with `\r\n` line endings. The frontmatter fence
/// scanner previously matched only `\n`, so a CRLF-saved file fell
/// through: `name:` / `version:` parsed as body, and `read_manifest`
/// errored with "missing `name:`". Regression pin: a CRLF-encoded
/// SKILL.md packs successfully.
#[test]
fn pack_accepts_crlf_frontmatter() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    // Hand-encode CRLF — `format!` with `\n` would interpolate LF and
    // miss the regression. The fence open + close lines and the
    // intermediate field lines all end with `\r\n` to match what a
    // Windows editor produces.
    let body = "---\r\nname: demo\r\nversion: 0.1.0\r\n---\r\n# Hi\r\n";
    std::fs::write(src.path().join("SKILL.md"), body).unwrap();

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

/// Claude Code reads the SKILL.md frontmatter `description:` at
/// discovery time to decide whether to load the skill at all (see
/// <https://code.claude.com/docs/en/skills>). A SKILL.md without it
/// ships effectively dead-on-arrival, so `pakx pack` must warn —
/// non-fatally, exit 0 still — when it's absent.
#[test]
fn pack_warns_when_skill_md_missing_description() {
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
        .success()
        .stderr(predicate::str::contains("missing `description:`"))
        .stderr(predicate::str::contains("Claude Code"));
    assert!(out.path().join("demo-0.1.0.tgz").is_file());
}

/// Inverse: a SKILL.md that does declare `description:` must NOT
/// trigger the warning. Locks in the scope guard so we don't spam
/// publishers that already follow the convention.
#[test]
fn pack_quiet_when_description_present() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: demo\nversion: 0.1.0\ndescription: A tidy little skill.\n",
    );
    let assertion = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--out",
            out.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        !stderr.contains("missing `description:`"),
        "warning must not fire when description is present — got stderr:\n{stderr}"
    );
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

/// `pakx pack --json` must emit a single newline-terminated JSON
/// object on stdout, route progress to stderr, and key the object on
/// the stable contract field names (`name`, `version`, `kind`,
/// `sha256`, `sizeBytes`, `tarballPath`, `warnings`).
#[test]
fn pack_json_emits_stable_shape_on_stdout() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: demo\nversion: 0.1.0\ndescription: tidy.\n",
    );
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--out",
            out.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.ends_with('\n'),
        "stdout must end with newline: {stdout:?}"
    );
    let body = stdout.trim_end_matches('\n');
    assert!(
        !body.contains('\n'),
        "json output must be single-line: {body:?}"
    );
    let v: Value = serde_json::from_str(body).expect("stdout is valid json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["name"], "demo");
    assert_eq!(v["version"], "0.1.0");
    assert_eq!(v["kind"], "skills");
    assert_eq!(v["sizeBytes"].as_u64().unwrap(), {
        let path = out.path().join("demo-0.1.0.tgz");
        std::fs::metadata(&path).unwrap().len()
    });
    let sha = v["sha256"].as_str().expect("sha256 string");
    assert_eq!(sha.len(), 64, "sha256 is lowercase hex (64 chars)");
    assert!(
        sha.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "sha256 must be lowercase hex: {sha}"
    );
    let tarball_path = v["tarballPath"].as_str().expect("tarballPath string");
    assert!(
        tarball_path.ends_with("demo-0.1.0.tgz"),
        "tarballPath should end with the canonical filename: {tarball_path}"
    );
    let warnings = v["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.is_empty(),
        "no warnings expected when description present: {warnings:?}"
    );
    // Human progress lines belong on stderr — the JSON path keeps
    // stdout reserved for the single payload object.
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains('{'),
        "stderr must not contain json payload: {stderr}"
    );
}

/// When the SKILL.md is missing `description:`, the warning still flows
/// to stderr **and** lands in the JSON `warnings[]` array. Exit code
/// stays 0 — warnings are non-fatal.
#[test]
fn pack_json_carries_warnings_alongside_stderr() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_min_skill(src.path(), "demo", "0.1.0");
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--out",
            out.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("missing `description:`"),
        "stderr must surface the warning: {stderr}"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("stdout is valid json");
    let warnings = v["warnings"].as_array().expect("warnings array present");
    assert_eq!(
        warnings.len(),
        1,
        "expected one warning when description missing: {warnings:?}"
    );
    let msg = warnings[0].as_str().expect("warning is string");
    assert!(
        msg.contains("description:"),
        "warning text should mention description: {msg}"
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
