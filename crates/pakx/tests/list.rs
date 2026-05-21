//! Integration tests for `pakx list`.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const BIN: &str = "pakx";

#[test]
fn list_without_lockfile_prints_hint() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no agents.lock"));
}

#[test]
fn list_empty_lockfile() {
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("agents.lock"),
        r#"{"lockfileVersion":1,"manifestHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","entries":{}}
"#,
    )
    .unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list", "--no-check"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no entries"));
}

#[test]
fn list_shows_entries_from_lockfile() {
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("agents.lock"),
        r#"{"lockfileVersion":1,"manifestHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","entries":{
  "mcp/io.github.acme/cool@1.2.3":{
    "name":"io.github.acme/cool",
    "type":"mcp",
    "version":"1.2.3",
    "resolvedFrom":"official-mcp:io.github.acme/cool",
    "registry":"official-mcp",
    "integrity":"sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=",
    "agents":["claude-code"],
    "dependencies":[]
  }
}}
"#,
    )
    .unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["list", "--no-check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("io.github.acme/cool"))
        .stdout(predicate::str::contains("1.2.3"));
}
