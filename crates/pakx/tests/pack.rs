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

// ---------------------------------------------------------------------------
// Feature 1 — per-kind bundle validation (warnings, never errors).
//
// Each kind's pack-time check warns when the bundle lacks the field
// Claude Code needs to load it, but the pack still SUCCEEDS (exit 0) so
// local-smoke / air-gapped uploads work. These tests assert: (a) a
// missing-field bundle emits the expected warning, (b) a valid bundle
// stays quiet, (c) the warning lands in `--json` `warnings[]`, and (d)
// every case exits 0 with the tarball written.
// ---------------------------------------------------------------------------

/// Run `pakx pack <src> --out <out>` and return the asserted command
/// output. Always asserts success (exit 0) — warnings never fail pack.
fn run_pack(src: &std::path::Path, out: &std::path::Path) -> std::process::Output {
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .clone()
}

/// subagents: a SKILL.md whose frontmatter carries both kebab-case
/// `name:` and `description:` is a valid sub-agent bundle — no warning.
#[test]
fn pack_subagent_quiet_when_name_and_description_present() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: code-reviewer\nversion: 0.1.0\nkind: subagents\ndescription: Reviews diffs.\n",
    );
    let output = run_pack(src.path(), out.path());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("sub-agents require both"),
        "no subagent warning expected when name + description present: {stderr}"
    );
    assert!(out.path().join("code-reviewer-0.1.0.tgz").is_file());
}

/// subagents: a bundle missing `description:` (only `name:` present)
/// must warn, cite the sub-agents doc, and still pack.
#[test]
fn pack_subagent_warns_when_description_missing() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    // name is kebab-case + present; description absent → warn.
    write_skill_with_frontmatter(
        src.path(),
        "name: code-reviewer\nversion: 0.1.0\nkind: subagents\n",
    );
    let output = run_pack(src.path(), out.path());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("sub-agents require both"),
        "subagent warning expected when description missing: {stderr}"
    );
    assert!(
        stderr.contains("https://code.claude.com/docs/en/sub-agents"),
        "warning must cite the sub-agents doc: {stderr}"
    );
    assert!(out.path().join("code-reviewer-0.1.0.tgz").is_file());
}

/// subagents: the warning also lands in `--json` `warnings[]` (additive)
/// and exit code stays 0.
#[test]
fn pack_subagent_warning_carried_in_json() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: code-reviewer\nversion: 0.1.0\nkind: subagents\n",
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
    let v: Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(v["kind"], "subagents");
    let warnings = v["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("sub-agents require both")),
        "json warnings[] must carry the subagent advisory: {warnings:?}"
    );
}

/// commands: a command bundle whose markdown declares `description:` is
/// valid — no warning.
#[test]
fn pack_command_quiet_when_description_present() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: deploy\nversion: 0.1.0\nkind: commands\ndescription: Deploy the app.\n",
    );
    let output = run_pack(src.path(), out.path());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("slash-command menu"),
        "no command warning expected when description present: {stderr}"
    );
    assert!(out.path().join("deploy-0.1.0.tgz").is_file());
}

/// commands: a command bundle without any `description:` frontmatter
/// must warn (recommended, not required), cite the slash-commands doc,
/// and still pack.
#[test]
fn pack_command_warns_when_description_missing() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(src.path(), "name: deploy\nversion: 0.1.0\nkind: commands\n");
    let output = run_pack(src.path(), out.path());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("slash-command menu"),
        "command warning expected when description missing: {stderr}"
    );
    assert!(
        stderr.contains("https://code.claude.com/docs/en/slash-commands"),
        "warning must cite the slash-commands doc: {stderr}"
    );
    assert!(out.path().join("deploy-0.1.0.tgz").is_file());
}

/// prompts: a prompt bundle with a non-empty file is valid — no warning.
#[test]
fn pack_prompt_quiet_when_nonempty_file_present() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    // SKILL.md body is non-empty → counts as content.
    write_skill_with_frontmatter(src.path(), "name: greet\nversion: 0.1.0\nkind: prompts\n");
    std::fs::write(
        src.path().join("prompt.txt"),
        b"Write a haiku about Rust.\n",
    )
    .unwrap();
    let output = run_pack(src.path(), out.path());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("prompt bundle has no non-empty file"),
        "no prompt warning expected when a non-empty file is present: {stderr}"
    );
    assert!(out.path().join("greet-0.1.0.tgz").is_file());
}

/// prompts: a prompt bundle whose only files are empty / whitespace must
/// warn — but still pack. The SKILL.md frontmatter alone (no body) plus
/// empty sibling files leaves nothing with content.
#[test]
fn pack_prompt_warns_when_only_empty_files() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    // SKILL.md with frontmatter only + an empty body (no markdown text
    // after the closing fence) and an empty sibling. Write SKILL.md by
    // hand so the body is whitespace-only.
    std::fs::write(
        src.path().join("SKILL.md"),
        "---\nname: greet\nversion: 0.1.0\nkind: prompts\n---\n   \n",
    )
    .unwrap();
    std::fs::write(src.path().join("empty.txt"), b"").unwrap();
    let output = run_pack(src.path(), out.path());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("prompt bundle has no non-empty file"),
        "prompt warning expected when every file is empty/whitespace: {stderr}"
    );
    assert!(out.path().join("greet-0.1.0.tgz").is_file());
}

/// hooks: a bundle declaring a recognised hook event (e.g. `PreToolUse`)
/// in any file is valid — no warning.
#[test]
fn pack_hooks_quiet_when_event_declared() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(src.path(), "name: guard\nversion: 0.1.0\nkind: hooks\n");
    std::fs::write(
        src.path().join("hooks.json"),
        br#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[]}]}}"#,
    )
    .unwrap();
    let output = run_pack(src.path(), out.path());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("declares no recognised hook event"),
        "no hooks warning expected when a known event is declared: {stderr}"
    );
    assert!(out.path().join("guard-0.1.0.tgz").is_file());
}

/// hooks: a bundle with no recognised hook event anywhere must warn,
/// cite the hooks doc, and still pack.
#[test]
fn pack_hooks_warns_when_no_event_declared() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(src.path(), "name: guard\nversion: 0.1.0\nkind: hooks\n");
    std::fs::write(
        src.path().join("notes.md"),
        b"# just some notes, no event\n",
    )
    .unwrap();
    let output = run_pack(src.path(), out.path());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("declares no recognised hook event"),
        "hooks warning expected when no event is declared: {stderr}"
    );
    assert!(
        stderr.contains("https://code.claude.com/docs/en/hooks"),
        "warning must cite the hooks doc: {stderr}"
    );
    assert!(out.path().join("guard-0.1.0.tgz").is_file());
}

/// mcp: declared as config, not a file bundle — no pack-time file check,
/// so a bare `kind: mcp` SKILL.md must NOT emit any kind-validation
/// warning. (It also must not emit the skills `description:` warning,
/// which only fires for `kind: skills`.)
#[test]
fn pack_mcp_emits_no_kind_validation_warning() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(src.path(), "name: server\nversion: 0.1.0\nkind: mcp\n");
    let output = run_pack(src.path(), out.path());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("missing `description:`"),
        "mcp kind must not trigger the skills description warning: {stderr}"
    );
    assert!(
        !stderr.contains("require both")
            && !stderr.contains("slash-command")
            && !stderr.contains("hook event"),
        "mcp kind must not trigger any bundle warning: {stderr}"
    );
    assert!(out.path().join("server-0.1.0.tgz").is_file());
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

/// Regression: prior to the fix, `pakx pack --json` hardcoded
/// `"kind": "skills"` regardless of what the SKILL.md frontmatter
/// declared. Add an explicit `kind: mcp` and assert it threads through
/// the wire contract — and the bundle still packs successfully (we
/// don't constrain the kind value at pack time; the registry validates
/// it server-side on `pakx publish`).
#[test]
fn pack_json_emits_declared_kind_not_hardcoded_skills() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: demo\nversion: 0.1.0\nkind: mcp\ndescription: tidy.\n",
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
    let v: Value = serde_json::from_str(stdout.trim()).expect("stdout is valid json");
    assert_eq!(
        v["kind"], "mcp",
        "pack --json must echo the declared frontmatter kind, not a hardcoded default: {v}"
    );
}

/// Inverse: when the SKILL.md frontmatter has no explicit `kind:` key,
/// the JSON shape must still default to `"skills"` so the historical
/// wire contract holds for existing publishers.
#[test]
fn pack_json_defaults_kind_to_skills_when_omitted() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    // `write_min_skill` only sets name + version — kind is absent.
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
    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: Value = serde_json::from_str(stdout.trim()).expect("stdout is valid json");
    assert_eq!(
        v["kind"], "skills",
        "absent frontmatter `kind:` must default to skills on the wire: {v}"
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

/// `--output` is the canonical long form (round 39 unification). The
/// historical `--out` is still accepted as an alias for one release;
/// pin both so a future removal of the alias trips this test
/// loudly + documents the breaking change.
#[test]
fn pack_accepts_output_long_form() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_min_skill(src.path(), "demo", "0.1.0");
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--output",
            out.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(out.path().join("demo-0.1.0.tgz").is_file());
}

#[test]
fn pack_accepts_out_alias() {
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

/// `pakx pack --dry-run --json` must enumerate every would-be tarball
/// entry on stdout WITHOUT writing the `.tgz` to disk. The contract:
///   - `dryRun: true` discriminator,
///   - `files: [{path, sizeBytes}]` array,
///   - exit 0 on success,
///   - no `.tgz` written.
#[test]
fn pack_dry_run_json_lists_files_without_writing_tarball() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: demo\nversion: 0.1.0\ndescription: tidy.\n",
    );
    std::fs::create_dir_all(src.path().join("reference")).unwrap();
    std::fs::write(src.path().join("reference").join("notes.md"), b"# Notes\n").unwrap();

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--output",
            out.path().to_str().unwrap(),
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let body = stdout.trim_end_matches('\n');
    let v: Value = serde_json::from_str(body).expect("stdout is valid json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["name"], "demo");
    assert_eq!(v["version"], "0.1.0");
    assert_eq!(v["dryRun"], true);
    let files = v["files"].as_array().expect("files array");
    // Two regular files: SKILL.md + reference/notes.md.
    assert_eq!(files.len(), 2);
    let paths: Vec<_> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&"SKILL.md"), "files: {paths:?}");
    assert!(paths.contains(&"reference/notes.md"), "files: {paths:?}");
    for f in files {
        let size = f["sizeBytes"].as_u64().expect("sizeBytes integer");
        assert!(size > 0, "every entry should have a non-zero size: {f}");
    }
    assert!(
        !out.path().join("demo-0.1.0.tgz").exists(),
        "dry-run must not write the .tgz to disk"
    );
}

/// `pakx pack --dry-run` (no `--json`) prints a short human summary on
/// stderr and writes nothing. Pinning the stderr text matters because
/// the `→ next:` hint is the user's discoverability path back to a
/// real pack.
#[test]
fn pack_dry_run_human_prints_summary_without_writing() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    write_skill_with_frontmatter(
        src.path(),
        "name: demo\nversion: 0.1.0\ndescription: tidy.\n",
    );
    let assertion = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--output",
            out.path().to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("would pack"), "got stderr: {stderr}");
    assert!(
        stderr.contains("pakx pack"),
        "next-step hint should reference `pakx pack`: {stderr}",
    );
    assert!(
        !out.path().join("demo-0.1.0.tgz").exists(),
        "dry-run must not write the .tgz to disk"
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
