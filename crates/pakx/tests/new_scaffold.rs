//! Integration tests for `pakx new <kind> <name>` — the publisher
//! scaffold generator.
//!
//! The load-bearing assertion is the one tying this command to the
//! per-kind `pakx pack` validation (PR #78): every scaffolded bundle,
//! when packed with `--json`, must produce an EMPTY `warnings[]` array.
//! If a future template change drops a required field, that integration
//! test trips immediately.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = "pakx";

/// Run `pakx new <kind> <name> --yes` inside `cwd` and assert success.
/// `--yes` skips the interactive description prompt so the run is
/// non-interactive in CI.
fn run_new(cwd: &std::path::Path, kind: &str, name: &str) -> std::process::Output {
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(cwd)
        .args(["new", kind, name, "--yes"])
        .assert()
        .success()
        .get_output()
        .clone()
}

/// Pack the scaffolded bundle at `dir` with `--json` and return the
/// parsed payload. `--out` is redirected to a sibling tempdir so the
/// `.tgz` doesn't land inside the bundle.
fn pack_json(dir: &std::path::Path, out: &std::path::Path) -> Value {
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str(stdout.trim()).expect("pack --json emits valid json")
}

/// Regression for the non-TTY hang: `pakx new <kind> <name>` WITHOUT
/// `--yes` and WITHOUT `--description` prompts for a one-line
/// description via `inquire::Text`, which blocks forever on a closed
/// stdin. It must fail fast with the "not a TTY" hint instead.
/// `assert_cmd` runs the child with a non-TTY stdin by default.
#[test]
fn new_without_yes_and_no_tty_bails_instead_of_hanging() {
    let work = TempDir::new().unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .write_stdin("")
        .args(["new", "skills", "my-skill"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("stdin is not a TTY"))
        .stderr(predicate::str::contains("--yes"));

    // No bundle dir created — the guard fires before any file write.
    assert!(
        !work.path().join("my-skill").exists(),
        "new must not scaffold a bundle when it bails on a missing TTY"
    );
}

/// `pakx new` with `--description` supplied (but no `--yes`) must NOT
/// bail on a missing TTY: the description prompt is the only interactive
/// step and supplying it means nothing needs to be read from stdin.
#[test]
fn new_with_description_flag_does_not_require_a_tty() {
    let work = TempDir::new().unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .write_stdin("")
        .args([
            "new",
            "skills",
            "my-skill",
            "--description",
            "a hand-supplied description",
        ])
        .assert()
        .success();

    assert!(
        work.path().join("my-skill").join("SKILL.md").is_file(),
        "bundle must scaffold when --description supplies the only prompt"
    );
}

// ---------------------------------------------------------------------------
// Per-kind: scaffold creates the expected files AND the bundle then packs
// with ZERO kind-validation warnings. This is the #2 ↔ #78 contract.
// ---------------------------------------------------------------------------

#[test]
fn skills_scaffold_creates_files_and_packs_without_warnings() {
    let work = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    run_new(work.path(), "skills", "my-skill");

    let dir = work.path().join("my-skill");
    assert!(dir.join("SKILL.md").is_file(), "SKILL.md must be created");
    let skill = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
    assert!(
        skill.contains("description:"),
        "frontmatter has description"
    );

    let v = pack_json(&dir, out.path());
    assert_eq!(v["kind"], "skills");
    let warnings = v["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.is_empty(),
        "scaffolded skills bundle must pack with zero warnings: {warnings:?}"
    );
}

#[test]
fn subagents_scaffold_creates_files_and_packs_without_warnings() {
    let work = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    run_new(work.path(), "subagents", "code-reviewer");

    let dir = work.path().join("code-reviewer");
    let skill = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
    // Both name (kebab-case) and description must be present — that's
    // what the subagents pack check scans for.
    assert!(skill.contains("name: code-reviewer"), "kebab name: {skill}");
    assert!(skill.contains("description:"), "description: {skill}");

    let v = pack_json(&dir, out.path());
    assert_eq!(v["kind"], "subagents");
    let warnings = v["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.is_empty(),
        "scaffolded subagents bundle must pack with zero warnings: {warnings:?}"
    );
}

#[test]
fn commands_scaffold_creates_files_and_packs_without_warnings() {
    let work = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    run_new(work.path(), "commands", "deploy");

    let dir = work.path().join("deploy");
    assert!(dir.join("SKILL.md").is_file());

    let v = pack_json(&dir, out.path());
    assert_eq!(v["kind"], "commands");
    let warnings = v["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.is_empty(),
        "scaffolded commands bundle must pack with zero warnings: {warnings:?}"
    );
}

#[test]
fn prompts_scaffold_creates_files_and_packs_without_warnings() {
    let work = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    run_new(work.path(), "prompts", "haiku");

    let dir = work.path().join("haiku");
    // The prompts pack check ignores SKILL.md, so the scaffold must ship
    // a real prompt file with content.
    assert!(dir.join("prompt.md").is_file(), "prompt.md must be created");
    let prompt = std::fs::read_to_string(dir.join("prompt.md")).unwrap();
    assert!(!prompt.trim().is_empty(), "prompt.md must be non-empty");

    let v = pack_json(&dir, out.path());
    assert_eq!(v["kind"], "prompts");
    let warnings = v["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.is_empty(),
        "scaffolded prompts bundle must pack with zero warnings: {warnings:?}"
    );
}

#[test]
fn hooks_scaffold_creates_files_and_packs_without_warnings() {
    let work = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    run_new(work.path(), "hooks", "guard");

    let dir = work.path().join("guard");
    assert!(
        dir.join("hooks.json").is_file(),
        "hooks.json must be created"
    );
    let hooks = std::fs::read_to_string(dir.join("hooks.json")).unwrap();
    // A recognised hook event must be declared so the hooks pack check
    // is satisfied.
    assert!(hooks.contains("PreToolUse"), "hooks.json declares an event");

    let v = pack_json(&dir, out.path());
    assert_eq!(v["kind"], "hooks");
    let warnings = v["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.is_empty(),
        "scaffolded hooks bundle must pack with zero warnings: {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// mcp rejection + bad-kind rejection.
// ---------------------------------------------------------------------------

/// mcp is config, not a file bundle — `pakx new mcp` must refuse and
/// point at `pakx add mcp`. Nothing should be written.
#[test]
fn new_mcp_is_rejected_with_pointer_to_add() {
    let work = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args(["new", "mcp", "server", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pakx add mcp"));
    assert!(
        !work.path().join("server").exists(),
        "mcp rejection must not create a directory"
    );
}

/// An unknown kind token errors cleanly naming the valid kinds.
#[test]
fn new_unknown_kind_errors_cleanly() {
    let work = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args(["new", "widgets", "thing", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a scaffoldable kind"));
}

// ---------------------------------------------------------------------------
// --force / refuse-existing behaviour.
// ---------------------------------------------------------------------------

/// A non-empty target dir is refused unless `--force`. The pre-existing
/// file must survive the refusal untouched.
#[test]
fn new_refuses_nonempty_target_without_force() {
    let work = TempDir::new().unwrap();
    let dir = work.path().join("my-skill");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("keep.txt"), b"keep me\n").unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args(["new", "skills", "my-skill", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not empty"));

    // The pre-existing file is untouched and no SKILL.md was written.
    assert_eq!(
        std::fs::read_to_string(dir.join("keep.txt")).unwrap(),
        "keep me\n"
    );
    assert!(
        !dir.join("SKILL.md").exists(),
        "refusal must not scaffold into a non-empty dir"
    );
}

/// `--force` scaffolds into a non-empty dir, overwriting the bundle
/// files while leaving unrelated files in place.
#[test]
fn new_force_scaffolds_into_nonempty_target() {
    let work = TempDir::new().unwrap();
    let dir = work.path().join("my-skill");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("keep.txt"), b"keep me\n").unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args(["new", "skills", "my-skill", "--yes", "--force"])
        .assert()
        .success();

    assert!(
        dir.join("SKILL.md").is_file(),
        "force must scaffold SKILL.md"
    );
    // The unrelated file is left alone.
    assert_eq!(
        std::fs::read_to_string(dir.join("keep.txt")).unwrap(),
        "keep me\n"
    );
}

/// An empty pre-existing dir is fine (the common `mkdir foo && cd foo`
/// case) — no `--force` needed.
#[test]
fn new_scaffolds_into_empty_existing_dir() {
    let work = TempDir::new().unwrap();
    let dir = work.path().join("my-skill");
    std::fs::create_dir_all(&dir).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args(["new", "skills", "my-skill", "--yes"])
        .assert()
        .success();
    assert!(dir.join("SKILL.md").is_file());
}

// ---------------------------------------------------------------------------
// Flags: --description, --output, --json.
// ---------------------------------------------------------------------------

/// `--description` is embedded verbatim into the generated frontmatter.
#[test]
fn description_flag_lands_in_frontmatter() {
    let work = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args([
            "new",
            "skills",
            "my-skill",
            "--description",
            "Tidies up imports.",
        ])
        .assert()
        .success();
    let skill = std::fs::read_to_string(work.path().join("my-skill").join("SKILL.md")).unwrap();
    // The description is emitted as a YAML double-quoted scalar so a
    // colon / `#` in a real description can't break the frontmatter.
    assert!(
        skill.contains("description: \"Tidies up imports.\""),
        "description flag must land in frontmatter: {skill}"
    );
}

/// Regression: a description containing a colon would break a plain
/// YAML scalar and make `pakx pack` reject the SKILL.md as invalid YAML.
/// The scaffold must double-quote it so the bundle still packs cleanly.
#[test]
fn description_with_colon_still_packs() {
    let work = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args([
            "new",
            "skills",
            "my-skill",
            "--description",
            "handles the foo: bar edge case",
        ])
        .assert()
        .success();
    let dir = work.path().join("my-skill");
    let v = pack_json(&dir, out.path());
    let warnings = v["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.is_empty(),
        "colon-bearing description must still pack with zero warnings: {warnings:?}"
    );
}

/// `--output` redirects the bundle to an arbitrary directory.
#[test]
fn output_flag_redirects_target_dir() {
    let work = TempDir::new().unwrap();
    let custom = work.path().join("elsewhere").join("bundle");
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args([
            "new",
            "skills",
            "my-skill",
            "--yes",
            "--output",
            custom.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(
        custom.join("SKILL.md").is_file(),
        "--output must redirect the scaffold target"
    );
    assert!(
        !work.path().join("my-skill").exists(),
        "default ./<name>/ must not be created when --output is given"
    );
}

/// `pakx new --json` emits a single newline-terminated object on stdout
/// keyed on the stable contract (`ok`, `kind`, `name`, `dir`, `files`).
#[test]
fn json_emits_stable_shape_on_stdout() {
    let work = TempDir::new().unwrap();
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args(["new", "skills", "my-skill", "--yes", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.ends_with('\n'), "stdout must end with newline");
    let body = stdout.trim_end_matches('\n');
    assert!(!body.contains('\n'), "json must be single-line: {body:?}");
    let v: Value = serde_json::from_str(body).expect("stdout is valid json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["kind"], "skills");
    assert_eq!(v["name"], "my-skill");
    let files = v["files"].as_array().expect("files array");
    let names: Vec<_> = files.iter().map(|f| f.as_str().unwrap()).collect();
    assert!(names.contains(&"SKILL.md"), "files: {names:?}");
    assert!(names.contains(&"README.md"), "files: {names:?}");
    // Human progress must NOT leak onto stdout in --json mode.
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains('{'),
        "stderr must not carry json: {stderr}"
    );
}

/// A name the registry would reject (uppercase / spaces) is refused
/// before any files are written.
#[test]
fn invalid_name_is_rejected() {
    let work = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args(["new", "skills", "Bad Name", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("lowercase ASCII"));
}

/// Human mode prints the created-file tree + a `→ next:` hint on stderr,
/// nothing on stdout.
#[test]
fn human_mode_prints_tree_and_next_hint_on_stderr() {
    let work = TempDir::new().unwrap();
    let assertion = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args(["new", "skills", "my-skill", "--yes"])
        .assert()
        .success();
    let out = assertion.get_output();
    assert!(out.stdout.is_empty(), "human mode keeps stdout empty");
    let stderr = String::from_utf8(out.stderr.clone()).unwrap();
    assert!(stderr.contains("scaffolded"), "got: {stderr}");
    assert!(
        stderr.contains("SKILL.md"),
        "tree should list files: {stderr}"
    );
    assert!(stderr.contains("pakx pack"), "next hint: {stderr}");
}

/// The `scaffold` alias resolves to the same command.
#[test]
fn scaffold_alias_works() {
    let work = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(work.path())
        .args(["scaffold", "skills", "my-skill", "--yes"])
        .assert()
        .success();
    assert!(work.path().join("my-skill").join("SKILL.md").is_file());
}
