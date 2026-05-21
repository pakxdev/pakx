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
fn unknown_subcommand_fails() {
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("nonsense")
        .assert()
        .failure();
}
