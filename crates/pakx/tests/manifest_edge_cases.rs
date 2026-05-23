//! Manifest-schema edge-case coverage.
//!
//! Existing `tests/manifest.rs` covers `pakx manifest get/set/delete`
//! against a well-formed fixture. This file covers the input shapes
//! the *install / list / doctor* surfaces see in the wild: empty deps,
//! all-six-kinds populated, typo'd keys, and the `deny_unknown_fields`
//! discipline the schema relies on. Every test invokes the real
//! `pakx` binary so the diagnostic surface (stderr wording, exit code)
//! is exercised end-to-end.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = "pakx";

fn write_manifest(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("agents.yml"), body).unwrap();
}

/// Empty `dependencies:` block. `pakx install` must no-op cleanly and
/// still produce an empty lockfile so downstream `pakx list` / `pakx
/// doctor` don't trip over a missing file.
#[test]
fn install_empty_dependencies_block_writes_empty_lockfile() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies: {}\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let lock_body = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let v: Value = serde_json::from_str(&lock_body).unwrap();
    assert!(
        v["entries"].as_object().unwrap().is_empty(),
        "empty deps must produce empty entries map; got:\n{lock_body}"
    );
}

/// No `dependencies` key at all (just `name` + `version`). Should
/// behave identically to the empty-deps case.
#[test]
fn install_omits_dependencies_key_no_ops() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(project.path(), "name: demo\nversion: 0.1.0\n");

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();
}

/// All six kinds populated. `pakx list --json` must list them all in
/// the source manifest (the lockfile gets built per `pakx install`,
/// but the manifest-parse path runs through `read_from` here just to
/// fail loudly on any kind serde overlooks).
#[test]
fn manifest_with_all_six_kinds_parses_cleanly() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\n\
         version: 0.1.0\n\
         dependencies:\n\
         \x20\x20skills:\n    - alice/s\n\
         \x20\x20mcp:\n    - io.github.acme/m\n\
         \x20\x20subagents:\n    - alice/a\n\
         \x20\x20prompts:\n    - alice/p\n\
         \x20\x20commands:\n    - alice/c\n\
         \x20\x20hooks:\n    - alice/h\n",
    );

    // `pakx manifest get dependencies --json` is the cheapest
    // round-trip through the parser that emits structured output we
    // can inspect.
    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["manifest", "get", "dependencies", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let deps = v.as_object().expect("dependencies must be object");
    for kind in ["skills", "mcp", "subagents", "prompts", "commands", "hooks"] {
        assert!(
            deps.contains_key(kind),
            "missing kind {kind} in --json output: {deps:?}"
        );
    }
}

/// Typo'd kind (`skill:` instead of `skills:`). The Dependencies
/// struct is `#[serde(deny_unknown_fields)]` so a typo'd key must
/// surface a *parse error* — not silently get ignored.
#[test]
fn manifest_with_typoed_kind_key_errors_loudly() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skill:\n    - alice/oops\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        // serde_yaml_ng's deny_unknown_fields message includes the
        // offending key name (`skill`).
        .stderr(predicate::str::contains("skill"));
}

/// Top-level YAML scalar (not a mapping). `pakx manifest get`'s path
/// walker treats this as an empty mapping and reports "path not
/// found" — not a crash. Lock that behaviour so a future strict-shape
/// rewrite doesn't silently flip the error surface.
#[test]
fn manifest_top_level_scalar_does_not_crash() {
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "just a string");

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["manifest", "get", "name"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path not found"));
}

/// Top-level YAML scalar with `pakx install`. The full `Manifest`
/// schema parse path rejects non-mapping inputs explicitly with the
/// "top level must be a YAML mapping" diagnostic.
#[test]
fn install_against_scalar_manifest_errors_with_mapping_hint() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(project.path(), "just a string");

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("mapping")
                .or(predicate::str::contains("invalid type"))
                .or(predicate::str::contains("must be")),
        );
}

/// `pakx pack` over a manifest with valid 4-entry sponsors works, but
/// with an invalid 5th entry the offending index must appear in the
/// error so the user can find the line in their SKILL.md frontmatter.
#[test]
fn pack_sponsors_block_with_invalid_index_reports_index() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    std::fs::write(
        src.path().join("SKILL.md"),
        "---\nname: demo\nversion: 0.1.0\n\
         sponsors:\n\
         \x20\x20- kind: github\n    url: https://github.com/sponsors/octocat\n\
         \x20\x20- kind: github\n    url: https://github.com/sponsors/alice\n\
         \x20\x20- kind: github\n    url: https://github.com/sponsors/bob\n\
         \x20\x20- kind: github\n    url: https://github.com/sponsors/carol\n\
         \x20\x20- kind: github\n    url: not-even-a-url\n\
         ---\n# Hi\n",
    )
    .unwrap();

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
        // Offending index `[4]` must appear so the user can locate
        // the bad row in frontmatter.
        .stderr(predicate::str::contains("sponsors[4]"));
}

/// `pakx add foo/bar` (no `-t`, no kind positional) currently writes
/// to the `mcp` section because `infer_kind` defaults that way for
/// shapes other than `<owner>/skills/<name>`. **This test locks the
/// status-quo** so a future change that flips the default (e.g. a
/// pakx-registry probe) shows up as a test diff a reviewer must
/// approve, rather than silently landing.
///
/// NOTE: The 2026-05-23 user-reported bug is that this default is
/// wrong for genuine pakx-registry skills. The fix is in flight on a
/// parallel branch — when it lands, this test should be inverted to
/// assert the probe-discovered kind takes precedence. Until then,
/// this is the pinned default.
#[test]
fn add_bare_id_no_type_flag_defaults_to_mcp() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", "alice/regular-shape", "--no-validate"])
        .assert()
        .success();

    let manifest_body = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    let manifest = pakx_core::parse_manifest(&manifest_body, None).unwrap();
    assert!(
        manifest.dependencies.mcp.is_some(),
        "current behaviour: `<owner>/<name>` shape defaults to mcp; got:\n{manifest_body}"
    );
    assert!(
        manifest.dependencies.skills.is_none(),
        "current behaviour: should NOT route to skills without probe; got:\n{manifest_body}"
    );
}

/// CRLF line endings on the manifest. Mirrors the SKILL.md CRLF guard
/// in `tests/pack.rs` — VSCode/Notepad on Windows save with `\r\n` and
/// the YAML parser must handle that.
#[test]
fn manifest_with_crlf_line_endings_parses() {
    let project = TempDir::new().unwrap();
    let body = "name: demo\r\nversion: 0.1.0\r\ndependencies:\r\n  mcp:\r\n    - a/b\r\n";
    write_manifest(project.path(), body);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["manifest", "get", "name"])
        .assert()
        .success()
        .stdout("demo\n");
}

/// Whitespace-only manifest. Should error with a clear diagnostic
/// rather than silently treating the file as an empty mapping.
#[test]
fn manifest_whitespace_only_errors() {
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "   \n\n\n");

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["manifest", "get", "name"])
        .assert()
        .failure();
}

/// `pakx remove` on a manifest with the typo'd kind must fail cleanly
/// — same parse path as install. Documents that *every* read entry
/// point shares the same schema-error surface.
#[test]
fn remove_against_typoed_manifest_fails_at_parse() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skill:\n    - a/b\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["remove", "a/b", "--yes"])
        .assert()
        .failure();
}
