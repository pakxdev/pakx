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
//!
//! Round 86 adds a parallel guard for `pakx test`: the subcommand
//! previously used a bare `TempDir::new()` for its cache root, which
//! lacked the pid+nanos prefix needed to keep parallel integration
//! tests from sharing cache state when their wiremock servers
//! recycle the same loopback port. The new test (and the
//! corresponding `make_cache_tempdir("pakx-test-cache")` swap in
//! `commands::test::build_registry_client`) align `pakx test` with
//! the rest of the federated-query family.

use assert_cmd::Command;
use serde_json::json;
use std::collections::HashSet;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

/// Snapshot the set of `pakx-*-cache-*` entries currently sitting in
/// `dir`. Used to subtract the pre-test residue from the post-test
/// scan so the assertion focuses on entries the command-under-test
/// created, not historical clutter from other processes (or earlier
/// test runs that crashed before drop).
///
/// `dir` is a per-test isolated tempdir that we point the child
/// process's temp-resolution env vars at (see [`isolated_temp`]), so
/// this scan never sees `pakx-*-cache-*` dirs created by sibling
/// tests running in parallel — the source of the round-93 flake.
fn snapshot_pakx_cache_dirs(dir: &std::path::Path) -> HashSet<std::ffi::OsString> {
    std::fs::read_dir(dir)
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

/// Allocate a per-test tempdir to redirect a child `pakx` process's
/// temp resolution into. `pakx`'s per-call cache root derives from
/// `std::env::temp_dir()` (see `commands::cache_tempdir::make_cache_tempdir`),
/// which reads `TMPDIR` on Unix/macOS and `TMP`/`TEMP` on Windows.
/// Setting all three on the child (via [`apply_isolated_temp`]) lands
/// every `pakx-<cmd>-cache-*` dir inside this isolated dir, so the
/// cleanup assertion is immune to sibling tests churning the shared
/// system temp between the before/after snapshots.
fn isolated_temp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("pakx-cleanup-iso-")
        .tempdir()
        .expect("isolated temp dir")
}

/// Point a child command's temp-resolution env vars at `dir` so its
/// `std::env::temp_dir()` resolves into our isolated tempdir.
fn apply_isolated_temp(cmd: &mut Command, dir: &std::path::Path) {
    cmd.env("TMPDIR", dir).env("TMP", dir).env("TEMP", dir);
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

    let iso = isolated_temp();
    let before = snapshot_pakx_cache_dirs(iso.path());

    let mut cmd = Command::cargo_bin(BIN).unwrap();
    apply_isolated_temp(&mut cmd, iso.path());
    cmd.args([
        "search",
        "sample",
        "--mcp-base-url",
        &mcp.uri(),
        "--no-smithery",
        "--no-pakx-registry",
    ])
    .assert()
    .success();

    let after = snapshot_pakx_cache_dirs(iso.path());

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
        iso.path(),
        leaked,
    );
}

/// Mount the official-MCP search endpoint so `OfficialMcpSource::fetch`
/// resolves cleanly through the search-fallback path. Mirrors the
/// fixture shape used by `test_cmd::test_online_resolves_against_registry`
/// — the per-server detail endpoint is intentionally NOT mounted so
/// every fetch falls through to the search path (matches the
/// 2025-12-11 production schema).
async fn mount_official_mcp_search_hit(server: &MockServer, name: &str, version: &str) {
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "name": name,
                    "description": "x",
                    "version_detail": { "version": version }
                }
            ]
        })))
        .mount(server)
        .await;
}

/// Round-86 companion: `pakx test` previously built its cache root
/// via `TempDir::new()`, which produced names like `.tmpXXXXXX` with
/// no `pakx-test-cache-` prefix. The bare prefix meant the
/// cleanup-leak guard above could not see those dirs at all, AND
/// the names did not carry the pid+nanos collision-avoidance suffix
/// that `make_cache_tempdir` adds. After the swap to
/// `make_cache_tempdir("pakx-test-cache")` the dir is visible to the
/// same scanner — and self-deletes on drop just like the other five
/// federated subcommands.
#[tokio::test]
async fn test_cleans_up_its_per_call_cache_tempdir() {
    let mcp = MockServer::start().await;
    mount_official_mcp_search_hit(&mcp, "io.github.acme/sample", "1.0.0").await;

    let project = tempfile::TempDir::new().expect("project tempdir");
    std::fs::write(
        project.path().join("agents.yml"),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/sample\n",
    )
    .unwrap();

    let iso = isolated_temp();
    let before = snapshot_pakx_cache_dirs(iso.path());

    let mut cmd = Command::cargo_bin(BIN).unwrap();
    apply_isolated_temp(&mut cmd, iso.path());
    cmd.current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &mcp.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .success();

    let after = snapshot_pakx_cache_dirs(iso.path());

    let leaked: Vec<_> = after
        .difference(&before)
        .filter(|n| n.to_string_lossy().contains("pakx-test-cache-"))
        .collect();
    assert!(
        leaked.is_empty(),
        "`pakx test` leaked cache tempdirs in {:?}: {:?}",
        iso.path(),
        leaked,
    );
}

/// `pakx test --no-cache` is wired through the same `cache_dir_at`
/// helper as every other federated subcommand. The flag must parse
/// and the command must still succeed (the cache-bypass only
/// changes which lookups hit the wire, not the surface contract).
#[tokio::test]
async fn test_accepts_no_cache_flag() {
    let mcp = MockServer::start().await;
    mount_official_mcp_search_hit(&mcp, "io.github.acme/sample", "1.0.0").await;

    let project = tempfile::TempDir::new().expect("project tempdir");
    std::fs::write(
        project.path().join("agents.yml"),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/sample\n",
    )
    .unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &mcp.uri(),
            "--no-smithery",
            "--no-pakx-registry",
            "--no-cache",
        ])
        .assert()
        .success();
}
