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
        .stdout(predicate::str::contains("0.0.0"));
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
fn init_subcommand_runs() {
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("pakx v0.0.0"));
}

#[test]
fn install_subcommand_runs() {
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("install")
        .assert()
        .success()
        .stderr(predicate::str::contains("pakx v0.0.0"));
}

#[test]
fn unknown_subcommand_fails() {
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("nonsense")
        .assert()
        .failure();
}
