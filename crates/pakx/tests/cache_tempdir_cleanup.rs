//! Integration test for the round-47 tempdir-cleanup fix.
//!
//! Five federated-query subcommands (`pakx add`, `pakx outdated`,
//! `pakx audit`, `pakx search`) used to create a per-call cache root
//! under `std::env::temp_dir()` and never remove it. A user who never
//! ran `pakx doctor --clear-cache` accumulated one
//! `pakx-<cmd>-cache-*` directory per invocation indefinitely.
//!
//! The fix wraps every per-call cache root in a `tempfile::TempDir`
//! guard that self-deletes on drop. This test pins the behaviour by
//! invoking `pakx search` end-to-end against a wiremock server and
//! asserting that no `pakx-search-cache-*` directory survives the
//! call. We use `search` because it is the simplest of the five to
//! drive (no lockfile / manifest needed) and the cleanup discipline
//! is identical across all five sites — fixing one verifies the
//! pattern for the rest.

use assert_cmd::Command;
use serde_json::json;
use std::collections::HashSet;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

/// Snapshot the set of `pakx-*-cache-*` entries currently sitting in
/// the system temp dir. Used to subtract the pre-test residue from
/// the post-test scan so the assertion focuses on entries the
/// command-under-test created, not historical clutter from other
/// processes (or earlier test runs that crashed before drop).
fn snapshot_pakx_cache_dirs() -> HashSet<std::ffi::OsString> {
    std::fs::read_dir(std::env::temp_dir())
        .map(|it| {
            it.filter_map(Result::ok)
                .map(|e| e.file_name())
                .filter(|n| {
                    let s = n.to_string_lossy();
                    s.starts_with("pakx-") && s.contains("-cache-")
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn search_cleans_up_its_per_call_cache_tempdir() {
    // Mock both Smithery and the pakx registry — `pakx search` walks
    // every enabled source even when one returns zero hits, so each
    // source must be served or the request stalls on the missing
    // mock. We disable Smithery via `--no-smithery` and the pakx
    // registry via `--no-pakx-registry` so the call only touches the
    // single MCP mock below.
    let mcp = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                { "name": "io.github.acme/sample", "description": "x", "version_detail": {"version": "1.0.0"} }
            ]
        })))
        .mount(&mcp)
        .await;

    let before = snapshot_pakx_cache_dirs();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "search",
            "sample",
            "--mcp-base-url",
            &mcp.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .success();

    let after = snapshot_pakx_cache_dirs();

    // Anything in `after` but not in `before` must have been created
    // by the `pakx search` invocation. The fix guarantees these are
    // all deleted on drop of the `TempDir` guard inside
    // `commands::search::build_client`.
    let leaked: Vec<_> = after
        .difference(&before)
        .filter(|n| n.to_string_lossy().contains("pakx-search-cache-"))
        .collect();
    assert!(
        leaked.is_empty(),
        "`pakx search` leaked cache tempdirs in {:?}: {:?}",
        std::env::temp_dir(),
        leaked,
    );
}
