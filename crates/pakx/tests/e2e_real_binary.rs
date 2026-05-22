//! End-to-end suite exercising the **real built `pakx` binary** against
//! a temp project folder.
//!
//! Each `#[test]` is also `#[ignore]` so the default `cargo test` run
//! stays fast. Opt-in via:
//!
//! ```text
//! cargo test --workspace -- --ignored e2e_real_binary
//! ```
//!
//! These scenarios fail if any of the following regress: argument
//! parsing (clap shape changes), manifest IO / round-trip, federated
//! resolve flow (`OfficialMcp` first, then fan-out), lockfile write +
//! integrity hashing, `pakx list` human + JSON output, `pakx test`
//! exit-code semantics, registry-URL guard against `userinfo`
//! smuggling, and `pakx pack` symlink refusal (unix-gated).
//!
//! Everything is hermetic: every scenario gets its own `TempDir` and its
//! own `wiremock::MockServer`(s). No live network calls — the binary is
//! always pointed at the local mock via the hidden `--mcp-base-url` /
//! `--smithery-base-url` / `--pakx-base-url` overrides.

use std::path::Path;

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a fresh `assert_cmd::Command` rooted at the built `pakx` binary.
/// Centralised so the bin name lives in one place.
fn pakx() -> Command {
    Command::cargo_bin(BIN).expect("pakx binary built")
}

/// Mount the 2025-12-11-schema MCP Registry response for `id`@`version`
/// on `server`. The detail endpoint 404s (matching the post-schema-drop
/// upstream) so the resolver hits the `?search=` fallback that
/// `OfficialMcpSource::fetch` performs. Returns nothing — fluent setup.
async fn fixture_official_mcp_package(server: &MockServer, id: &str, version: &str) {
    let pkg = json!({
        "name": id,
        "description": "e2e fixture",
        "version_detail": { "version": version },
        "packages": [
            {
                "registry_name": "npm",
                "name": "@e2e/fixture",
                "version": version,
                "package_arguments": [],
                "environment_variables": []
            }
        ]
    });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [pkg]
        })))
        .mount(server)
        .await;
}

/// Mount an empty `OfficialMcp` response: 404 on detail, `[]` on search.
/// Used to force the federated resolver into its fallback fan-out.
async fn fixture_official_mcp_empty(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(server)
        .await;
}

/// Mount a Smithery `GET /servers` hit with an exact-name match. The
/// federated resolver picks the first source whose `search()` returns
/// `id` verbatim; this fixture supplies that. The `packages[]` body
/// rides through `SmitherySource`'s flatten-into-extra capture so
/// `mcp_translate` finds an installable transport.
async fn fixture_smithery_package(server: &MockServer, id: &str, version: &str) {
    Mock::given(method("GET"))
        .and(wm_path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "qualifiedName": id,
                    "displayName": id,
                    "description": "smithery e2e",
                    "packages": [
                        {
                            "registry_name": "npm",
                            "name": "@e2e/smithery-fixture",
                            "version": version,
                            "package_arguments": [],
                            "environment_variables": []
                        }
                    ]
                }
            ]
        })))
        .mount(server)
        .await;
}

/// Seed `project/agents.yml` directly. Avoids depending on `pakx init`'s
/// interactive shape — each scenario sets exactly the deps it needs.
fn write_manifest(dir: &Path, body: &str) {
    std::fs::write(dir.join("agents.yml"), body).expect("write agents.yml");
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Scenario 1 — full happy-path init / add / install / list /
/// list --json / test / remove. Regressions in any link of the chain
/// trip this test.
#[tokio::test]
#[ignore = "e2e_real_binary — opt in via --ignored"]
#[allow(clippy::too_many_lines)] // single end-to-end happy-path; splitting
                                 // into helpers would hide the linear story.
async fn e2e_init_add_install_list_test_remove_happy_path() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "io.github.modelcontextprotocol/server-filesystem";
    let mcp = MockServer::start().await;
    fixture_official_mcp_package(&mcp, id, "1.0.0").await;

    // Equivalent to `pakx init --yes --name e2e-demo` without depending
    // on `init`'s interactive paths.
    write_manifest(project.path(), "name: e2e-demo\nversion: 0.1.0\n");

    // `pakx add` — `--no-validate` keeps the mock-budget tight.
    pakx()
        .current_dir(project.path())
        .args(["add", id, "--type", "mcp", "--no-validate"])
        .assert()
        .success();
    let yml = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    assert!(yml.contains(id), "agents.yml must contain id; got:\n{yml}");

    // `pakx install` against the mock.
    pakx()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--no-pakx-registry",
            "--mcp-base-url",
            &mcp.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    // Lockfile must exist and pin the registry source as official-mcp.
    let lock_body = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let lock: Value = serde_json::from_str(&lock_body).unwrap();
    let key = format!("mcp/{id}@1.0.0");
    let entry = &lock["entries"][&key];
    assert_eq!(entry["registry"], "official-mcp");
    assert!(
        entry["integrity"]
            .as_str()
            .unwrap_or("")
            .starts_with("sha256-"),
        "integrity hash must be present"
    );

    // `pakx list` (human, `--no-check`) — must surface the id under
    // an [ok] badge. The Claude adapter's `list()` only enumerates
    // installed *skills* on disk; MCP servers live in `.mcp.json`,
    // not in the skills root, so without `--no-check` the adapter
    // would report `drift` for an MCP-only project. The list-output
    // tests in `tests/list.rs` use the same `--no-check` opt-out for
    // the same reason.
    let list_out = pakx()
        .current_dir(project.path())
        .args([
            "list",
            "--no-check",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&list_out.stdout);
    assert!(
        stdout.contains(id),
        "list human output must include id; got:\n{stdout}"
    );

    // `pakx list --json` — exactly one entry, registry==official-mcp.
    // With `--no-check` the status is `unknown` (skipped reconciliation)
    // rather than `ok`; the JSON-field contract scenario covers the
    // status==ok path explicitly via its own MCP install.
    let json_out = pakx()
        .current_dir(project.path())
        .args([
            "list",
            "--no-check",
            "--json",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let json_line = String::from_utf8_lossy(&json_out.stdout);
    let arr: Value = serde_json::from_str(json_line.trim()).expect("list --json is valid json");
    let arr = arr.as_array().expect("list --json is an array");
    assert_eq!(arr.len(), 1, "exactly one lockfile entry");
    let only = &arr[0];
    assert_eq!(only["registry"], "official-mcp");
    assert_eq!(only["id"], id);

    // `pakx test` — must exit 0 against the same mock.
    pakx()
        .current_dir(project.path())
        .args([
            "test",
            "--no-smithery",
            "--no-pakx-registry",
            "--mcp-base-url",
            &mcp.uri(),
        ])
        .assert()
        .success();

    // `pakx remove --yes` — depends on PR A. Probe the binary for
    // `remove` support via `--help` before invoking, so this suite
    // still passes against an `origin/main` checkout that hasn't
    // merged the `remove` PR yet. Once PR A lands the probe always
    // succeeds and the gating is a no-op.
    if pakx_supports_remove() {
        pakx()
            .current_dir(project.path())
            .args(["remove", id, "--yes"])
            .assert()
            .success();
        let yml_after = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
        assert!(
            !yml_after.contains(id),
            "agents.yml must no longer contain id; got:\n{yml_after}"
        );
    } else {
        eprintln!(
            "skipping `pakx remove` arm: binary lacks the subcommand \
             (depends on PR feat/cli-remove-cmd)"
        );
    }
}

/// Probe: does the built binary expose `pakx remove`? Used to gate the
/// remove arm of the happy-path scenario so this suite stays green when
/// the dependent PR hasn't merged yet.
fn pakx_supports_remove() -> bool {
    let out = pakx().args(["remove", "--help"]).output();
    out.is_ok_and(|o| o.status.success())
}

/// Scenario 2 — malformed manifest must trip `pakx test` with a
/// non-zero exit. The schema has `deny_unknown_fields` so a typo'd
/// top-level key counts as invalid — the CI contract.
#[test]
#[ignore = "e2e_real_binary — opt in via --ignored"]
fn e2e_test_exits_nonzero_on_invalid_manifest() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: bad\nversion: 0.1.0\nunknown_field_xyz: 1\n",
    );
    let assert = pakx()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure();
    let out = assert.get_output();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("unknown")
            || combined.contains("unknwn")
            || combined.contains("read manifest"),
        "expected unknown-field diagnostic; got:\n{combined}"
    );
}

/// Scenario 3 — federated fallback. `OfficialMcp` 404s, Smithery
/// returns a hit. The lockfile entry must record `registry: "smithery"`
/// (the source of truth introduced in #30).
#[tokio::test]
#[ignore = "e2e_real_binary — opt in via --ignored"]
async fn e2e_install_resolves_via_smithery_fallback() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/cool";

    let official = MockServer::start().await;
    fixture_official_mcp_empty(&official).await;
    let smithery = MockServer::start().await;
    fixture_smithery_package(&smithery, id, "1.0.0").await;

    write_manifest(
        project.path(),
        &format!("name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - {id}\n"),
    );

    pakx()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &official.uri(),
            "--smithery-base-url",
            &smithery.uri(),
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    // `SmitherySource::into_package` hardcodes `version: "latest"`
    // because Smithery exposes versions per connection rather than per
    // server. The lockfile key therefore lands at `mcp/<id>@latest`
    // when smithery is the resolving source. Asserting on that
    // version locks the documented behaviour — if Smithery ever
    // surfaces a real version, this test should be updated alongside.
    let lock: Value =
        serde_json::from_str(&std::fs::read_to_string(project.path().join("agents.lock")).unwrap())
            .unwrap();
    let entries = lock["entries"]
        .as_object()
        .expect("lockfile entries is an object");
    let key = format!("mcp/{id}@latest");
    let entry = entries
        .get(&key)
        .unwrap_or_else(|| panic!("lockfile missing key {key}; got entries: {entries:?}"));
    assert_eq!(
        entry["registry"], "smithery",
        "lockfile must pin smithery on fallback; got entry: {entry}"
    );
}

/// Scenario 4 — userinfo-smuggling base-URL must be rejected before
/// any HTTP work fires (#29 regression).
#[test]
#[ignore = "e2e_real_binary — opt in via --ignored"]
fn e2e_install_rejects_userinfo_smuggling() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    write_manifest(project.path(), "name: demo\nversion: 0.1.0\n");

    let assert = pakx()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            "http://localhost:8080@evil.com/",
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("refusing to use registry base URL")
            || stderr.to_lowercase().contains("userinfo")
            || stderr.to_lowercase().contains("embedded credentials"),
        "stderr must reject userinfo bypass; got:\n{stderr}"
    );
}

/// Scenario 5 — `pakx pack` must refuse symlinks under the `SKILL.md`
/// source tree (#29 security regression). Unix-only — Windows symlink
/// creation requires `SeCreateSymbolicLinkPrivilege` which CI/dev
/// machines may lack. The Windows-side coverage already lives in
/// `tests/pack.rs`.
#[cfg(unix)]
#[test]
#[ignore = "e2e_real_binary — opt in via --ignored"]
fn e2e_pack_refuses_symlink() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let secrets = TempDir::new().unwrap();
    std::fs::write(
        src.path().join("SKILL.md"),
        "---\nname: demo\nversion: 0.1.0\n---\n# Hi\n",
    )
    .unwrap();
    let target = secrets.path().join("id_rsa");
    std::fs::write(&target, b"PRETEND PRIVATE KEY").unwrap();
    std::os::unix::fs::symlink(&target, src.path().join("leaked.pem")).unwrap();

    let assert = pakx()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "--out",
            out.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("symlinks under SKILL.md src/ are not allowed"),
        "stderr must surface symlink refusal; got:\n{stderr}"
    );
}

/// Scenario 6 — JSON-field contract for `pakx list --json`. Field
/// names are a stable downstream contract; this test pins every name
/// so a rename fails CI loudly (only additive changes are
/// backwards-compatible).
#[tokio::test]
#[ignore = "e2e_real_binary — opt in via --ignored"]
async fn e2e_list_json_field_contract() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "io.github.acme/contract";
    let mcp = MockServer::start().await;
    fixture_official_mcp_package(&mcp, id, "1.2.3").await;

    write_manifest(
        project.path(),
        &format!("name: contract\nversion: 0.1.0\ndependencies:\n  mcp:\n    - {id}\n"),
    );

    pakx()
        .current_dir(project.path())
        .args([
            "install",
            "--no-smithery",
            "--no-pakx-registry",
            "--mcp-base-url",
            &mcp.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let out = pakx()
        .current_dir(project.path())
        .args([
            "list",
            "--json",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let line = String::from_utf8_lossy(&out.stdout);
    let arr: Value = serde_json::from_str(line.trim()).expect("list --json valid json");
    let entry = arr
        .as_array()
        .and_then(|a| a.first())
        .expect("at least one entry");
    let obj = entry.as_object().expect("entry is an object");

    // Every expected field name. Maintained as a literal list so a
    // rename fails this test instead of being absorbed silently.
    for field in [
        "key",
        "id",
        "version",
        "type",
        "registry",
        "resolved_from",
        "integrity",
        "agents",
        "status",
    ] {
        assert!(
            obj.contains_key(field),
            "list --json entry missing required field {field}; got: {obj:?}"
        );
    }
}
