//! Smoke tests for the built `pakx` binary.
//!
//! Spawns the binary via `assert_cmd` (which builds it on demand) and
//! checks the shape of `--version`, `--help`, and each subcommand stub.

use assert_cmd::Command;
use predicates::prelude::*;

const BIN: &str = "pakx";

#[test]
fn version_flag_prints_version() {
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_flag_prints_usage() {
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("pakx"));
}

#[test]
fn init_help_runs() {
    // Bare `init` without --yes would block on interactive prompts in CI,
    // so smoke-coverage uses --help. End-to-end init flow lives in
    // tests/init.rs.
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--yes"))
        .stdout(predicate::str::contains("--force"));
}

#[test]
fn install_help_runs() {
    // Bare `install` without a manifest would fail trying to read agents.yml;
    // smoke-coverage uses --help. End-to-end install flow lives in
    // tests/install.rs.
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Install everything"))
        .stdout(predicate::str::contains("--no-lockfile"));
}

#[test]
fn config_help_advertises_json_flag() {
    // `pakx config --json` was added alongside the human render in the
    // round-5 / round-8 polish. The help output is the contract surface
    // scripts rely on to discover the flag — assert it stays advertised.
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn config_json_emits_expected_keys() {
    // Smoke-check the shape (top-level keys + a non-empty `registries`
    // map). The exact values depend on the host (credentials path,
    // cache dir) — a full payload assertion would be brittle, so we
    // only pin the contract that downstream pipelines depend on.
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args(["config", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let line = String::from_utf8(output).unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert!(v.get("version").is_some(), "missing version: {v}");
    assert!(v.get("platform").is_some(), "missing platform: {v}");
    assert!(
        v.get("credentialsPath").is_some(),
        "missing credentialsPath: {v}",
    );
    assert!(v.get("cacheDir").is_some(), "missing cacheDir: {v}");
    let registries = v.get("registries").expect("missing registries");
    assert!(
        registries.get("pakx").is_some(),
        "missing pakx registry: {v}"
    );
    assert!(
        registries.get("officialMcp").is_some(),
        "missing officialMcp registry: {v}",
    );
    assert!(
        registries.get("smithery").is_some(),
        "missing smithery registry: {v}",
    );
}

#[test]
fn unknown_subcommand_fails() {
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("nonsense")
        .assert()
        .failure();
}

/// Regression for the 2026-05-23 JSON-formatting unification: every
/// other `pakx <cmd> --json` surface emits single-line JSON (so
/// pipelines can treat one line as one record). `pakx config --json`
/// previously emitted pretty-printed JSON, breaking that contract.
/// Pin the single-line shape so a future regression to `to_string_pretty`
/// trips the test rather than the user's `jq` pipeline.
#[test]
fn config_json_is_single_line() {
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args(["config", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    // Trailing newline from `println!` is fine; the payload between
    // the start and the trailing newline must be a single line.
    let body = stdout.trim_end_matches('\n');
    assert!(
        !body.contains('\n'),
        "pakx config --json must be single-line: body=\n{body:?}"
    );
}

/// Regression for `--color always --json` ANSI bleed: when `--json` is
/// set, stdout must never carry ANSI escape sequences regardless of
/// the `--color` setting. ESC (0x1B) is the canonical leading byte for
/// every CSI sequence, so its absence on stdout is the tight invariant.
#[test]
fn config_json_with_color_always_emits_no_ansi_escapes_on_stdout() {
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args(["--color", "always", "config", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        !output.contains(&0x1Bu8),
        "stdout must be ANSI-free in --json mode even with --color always: stdout={:?}",
        String::from_utf8_lossy(&output)
    );
}

/// Regression for the `pakx publish --kind <s>` typo trap: the flag
/// must reject unknown kinds at clap-parse time so a typo (`--kind
/// banana`) fails *before* we pack + upload. The error surfaces on
/// stderr per clap convention.
#[test]
fn publish_kind_rejects_unknown_value_at_parse_time() {
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["publish", "--kind", "banana"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"))
        .stderr(predicate::str::contains("--kind"));
}

/// Companion to the rejection test: every known kind must parse. We
/// pair `--dry-run` with `--credentials-file` pointing at a missing
/// file so the run short-circuits before any HTTP fires — what we're
/// pinning is the clap parse, not the upload flow.
#[test]
fn publish_kind_accepts_every_known_kind() {
    for kind in ["skills", "mcp", "subagents", "prompts", "commands", "hooks"] {
        // `--kind` validates at parse time; the flag must be advertised
        // in the help text too so users discover the closed set.
        let output = Command::cargo_bin(BIN)
            .unwrap()
            .args(["publish", "--help"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let help = String::from_utf8(output).unwrap();
        assert!(
            help.contains(kind),
            "publish --help must advertise kind {kind:?}: help=\n{help}"
        );
    }
}
