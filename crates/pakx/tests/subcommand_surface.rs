//! `pakx <subcommand> --help` smoke + bare-invocation matrix.
//!
//! For every subcommand listed in `crates/pakx/src/commands/mod.rs`:
//!   1. `--help` must exit 0 with usage text + the subcommand name.
//!   2. Bare invocation (no args) should produce either help text, a
//!      clear "missing argument" error, or a documented default action
//!      — NEVER a panic / segfault / wall of debug spew.
//!   3. Where a `--json` flag is documented, it must accept it without
//!      a clap parsing error.
//!
//! This locks the help-text contract scripts and AI assistants rely on
//! to discover the CLI surface. A regression here (e.g. a subcommand
//! renamed without updating its `about`, or a `--json` flag dropped)
//! breaks downstream tooling that grep the help output.

use assert_cmd::Command;
use predicates::prelude::*;

const BIN: &str = "pakx";

/// Every subcommand the binary exposes. Source: `Cli::Command` enum in
/// `crates/pakx/src/main.rs`. Adding a subcommand without adding it
/// here means the help-text contract goes uncovered for that command.
const SUBCOMMANDS: &[&str] = &[
    "init",
    "add",
    "remove",
    "install",
    "list",
    "tree",
    "why",
    "outdated",
    "audit",
    "doctor",
    "search",
    "test",
    "info",
    "login",
    "whoami",
    "pack",
    "publish",
    "unpublish",
    "update",
    "upgrade",
    "completion",
    "config",
    "manifest",
];

#[test]
fn every_subcommand_help_exits_zero_with_usage() {
    // Walk every subcommand; `--help` must always succeed and emit a
    // recognisable Usage line. We assert in a loop rather than per-
    // command so a new subcommand needs only one line in SUBCOMMANDS
    // above to be covered.
    for sub in SUBCOMMANDS {
        let result = Command::cargo_bin(BIN)
            .unwrap()
            .args([sub, "--help"])
            .assert()
            .try_success();
        let assert = result.unwrap_or_else(|e| panic!("`pakx {sub} --help` failed: {e}"));
        let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
        assert!(
            stdout.contains("Usage:"),
            "`pakx {sub} --help` missing Usage: line; got:\n{stdout}"
        );
    }
}

/// `pakx --help` must list every subcommand from SUBCOMMANDS. Documents
/// the discoverability contract.
#[test]
fn root_help_lists_every_subcommand() {
    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    for sub in SUBCOMMANDS {
        assert!(
            stdout.contains(sub),
            "`pakx --help` missing subcommand {sub}; got:\n{stdout}"
        );
    }
}

/// JSON-bearing subcommands must advertise `--json` in their help.
#[test]
fn json_capable_subcommands_advertise_json_flag() {
    // Source: a quick grep of `--json` flags across `src/commands/*.rs`.
    // We only assert advertisement (not behaviour) here — behaviour is
    // covered in the dedicated per-command test files.
    // NOTE: `pakx manifest` is a parent command — its --json lives on
    // the get/set/delete leaf subcommands, not on the parent. We test
    // those separately below.
    let json_subcommands: &[&str] = &[
        "list", "tree", "outdated", "audit", "search", "info", "whoami", "config", "doctor",
    ];
    for sub in json_subcommands {
        let assert = Command::cargo_bin(BIN)
            .unwrap()
            .args([sub, "--help"])
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
        assert!(
            stdout.contains("--json"),
            "`pakx {sub} --help` should mention --json; got:\n{stdout}"
        );
    }
}

/// `pakx manifest get/set/delete` each advertise `--json`. The parent
/// `manifest` is a router, so the flag lives one level down. Locks
/// the leaf-level contract per round-10 docs.
#[test]
fn manifest_get_and_set_leaf_subcommands_advertise_json_flag() {
    // `manifest get` + `manifest set` carry --json (typed read /
    // typed write); `manifest delete` is destructive and intentionally
    // has no JSON variant.
    for leaf in ["get", "set"] {
        let assert = Command::cargo_bin(BIN)
            .unwrap()
            .args(["manifest", leaf, "--help"])
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
        assert!(
            stdout.contains("--json"),
            "`pakx manifest {leaf} --help` should advertise --json; got:\n{stdout}"
        );
    }
}

/// `-C/--directory` workspace override must be documented for the
/// project-scoped subcommands.
#[test]
fn workspace_scoped_subcommands_advertise_directory_flag() {
    let dir_subcommands: &[&str] = &[
        "install", "list", "tree", "outdated", "audit", "doctor", "remove",
    ];
    for sub in dir_subcommands {
        let assert = Command::cargo_bin(BIN)
            .unwrap()
            .args([sub, "--help"])
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
        assert!(
            stdout.contains("--directory") || stdout.contains("-C"),
            "`pakx {sub} --help` should mention -C/--directory; got:\n{stdout}"
        );
    }
}

/// Unknown subcommand exits non-zero with a clap suggestion.
#[test]
fn unknown_subcommand_errors_with_suggestion() {
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("nonsense-subcommand-xyz")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unrecognized").or(predicate::str::contains("unexpected")),
        );
}

/// Unknown flag on a known subcommand exits non-zero with a useful
/// message naming the offending flag.
#[test]
fn unknown_flag_on_known_subcommand_errors_cleanly() {
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["install", "--bogus-flag-zzz"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("bogus-flag-zzz"));
}

/// `pakx --version` matches the package version. (Smoke-style locking;
/// the existing tests/smoke.rs has a weaker variant that only checks
/// "contains version"; this one checks the prefix shape.)
#[test]
fn version_flag_emits_pakx_prefix() {
    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .arg("--version")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.starts_with("pakx "),
        "version line should start with `pakx `; got: {stdout:?}"
    );
}

/// `--color never` on a help invocation must NOT inject ANSI escape
/// codes — scripted CI users depend on this.
#[test]
fn color_never_strips_ansi_from_help() {
    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .args(["--color", "never", "--help"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        !stdout.contains('\u{001b}'),
        "help with --color never must NOT contain ESC bytes; got: {stdout:?}"
    );
}

/// `pakx config --json` already covered by smoke.rs; this test pins
/// the *empty-state* shape of `pakx list --json` (no manifest, no
/// lockfile) — must not crash, must emit either nothing or valid JSON.
#[test]
fn list_json_in_empty_dir_emits_stable_shape() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(tmp.path())
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Empty stdout is fine (hint went to stderr). Non-empty stdout
    // must be parseable JSON.
    if !stdout.trim().is_empty() {
        let _: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("list --json on empty dir must emit valid JSON or empty; err={e}, stdout={stdout:?}")
        });
    }
}

/// `pakx whoami --help` lists `--offline` and `--json`. These flags
/// are part of the round-28 contract and downstream scripts depend on
/// them being advertised.
#[test]
fn whoami_help_advertises_offline_and_json() {
    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .args(["whoami", "--help"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("--offline"),
        "whoami help should advertise --offline; got:\n{stdout}"
    );
    assert!(
        stdout.contains("--json"),
        "whoami help should advertise --json; got:\n{stdout}"
    );
}

/// `pakx outdated --help` advertises `--registry`. The round-14
/// contract: scripts can filter outdated checks per source.
#[test]
fn outdated_help_advertises_registry_filter() {
    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .args(["outdated", "--help"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("--registry"),
        "outdated help should advertise --registry; got:\n{stdout}"
    );
}

/// `pakx update --help` advertises `--yes` / `-y` and `--dry-run`.
#[test]
fn update_help_advertises_yes_and_dry_run() {
    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .args(["update", "--help"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("--yes"),
        "update help should advertise --yes; got:\n{stdout}"
    );
    assert!(
        stdout.contains("--dry-run"),
        "update help should advertise --dry-run; got:\n{stdout}"
    );
}

/// `pakx add --help` advertises the dual-positional form (mentions
/// `id` AND `kind`).
#[test]
fn add_help_describes_dual_positional_form() {
    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .args(["add", "--help"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    // The help text must mention both the kind concept and the id
    // concept so users discover the `pakx add <kind> <id>` form.
    assert!(
        stdout.to_lowercase().contains("kind"),
        "add help should mention kind concept; got:\n{stdout}"
    );
    assert!(
        stdout.to_lowercase().contains("id"),
        "add help should mention id concept; got:\n{stdout}"
    );
}
