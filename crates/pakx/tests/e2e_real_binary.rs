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

/// Scenario — full sponsors round-trip (Phase X2b). A SKILL.md
/// declaring two sponsor links must:
///   1. Survive `pakx pack` without validation error.
///   2. Publish via `POST /api/v1/packages` with a `sponsors` JSON
///      array in the body.
///   3. Surface as a `sponsors[]` field on `pakx info <id> --json`.
///
/// Wiremock fronts the registry — same shape as `login_publish.rs`'s
/// `mock_registry`, plus a GET-detail handler that echoes the sponsor
/// list. Deferred to `#[ignore]` so the default `cargo test` run stays
/// fast; opt in via `--ignored`.
#[tokio::test]
#[ignore = "e2e_real_binary — opt in via --ignored"]
#[allow(clippy::too_many_lines)] // linear end-to-end story; helpers would obscure it
async fn e2e_publish_emits_sponsors_and_info_json_round_trips() {
    let src = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let creds_path = temp.path().join("c.json");
    let server = MockServer::start().await;

    // SKILL.md with three sponsors covering github / kofi / url.
    std::fs::write(
        src.path().join("SKILL.md"),
        "---\nname: pdf\nversion: 1.0.0\nsponsors:\n  - kind: github\n    url: https://github.com/sponsors/alice\n  - kind: kofi\n    url: https://ko-fi.com/alice\n  - kind: url\n    url: https://opencollective.com/alice\n---\n# pdf\n",
    )
    .unwrap();

    // Auth + publish endpoints — POST records the body, GET detail
    // echoes the sponsor list so the round-trip can be asserted on
    // `pakx info --json`.
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/whoami"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "id": "u_1", "login": "alice", "email": null })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wm_path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "pkg_1",
            "owner": "alice",
            "name": "pdf",
            "kind": "skills",
            "created": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/packages/.+/.+/.+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/pdf",
            "version": "1.0.0",
            "sha256": "0".repeat(64),
            "sizeBytes": 123,
            "tarballUrl": "https://example.com/tarball.tgz"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/alice/pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/pdf",
            "kind": "skills",
            "description": "pdf skill",
            "sponsors": [
                { "kind": "github", "url": "https://github.com/sponsors/alice" },
                { "kind": "kofi",   "url": "https://ko-fi.com/alice" },
                { "kind": "url",    "url": "https://opencollective.com/alice" }
            ],
            "versions": [
                {
                    "version": "1.0.0",
                    "sha256": "0".repeat(64),
                    "sizeBytes": 123,
                    "publishedAt": "2026-05-22T00:00:00Z",
                    "deprecatedAt": null
                }
            ]
        })))
        .mount(&server)
        .await;

    // login → publish → info --json.
    pakx()
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            "pakx_v1_TEST_TOKEN",
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    pakx()
        .args([
            "publish",
            src.path().to_str().unwrap(),
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Inspect the recorded POST body — sponsors array must be present.
    let posts: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method == wiremock::http::Method::POST && r.url.path() == "/api/v1/packages")
        .collect();
    assert_eq!(posts.len(), 1, "exactly one POST /api/v1/packages");
    let body: Value = serde_json::from_slice(&posts[0].body).unwrap();
    let sponsors = body
        .get("sponsors")
        .and_then(Value::as_array)
        .expect("publish must emit sponsors");
    assert_eq!(sponsors.len(), 3);

    // `pakx info alice/pdf --json` must surface the same array.
    let out = pakx()
        .args(["info", "alice/pdf", "--json", "--registry", &server.uri()])
        .assert()
        .success()
        .get_output()
        .clone();
    let line = String::from_utf8_lossy(&out.stdout);
    let info: Value = serde_json::from_str(line.trim()).expect("info --json valid json");
    let sponsors = info
        .get("sponsors")
        .and_then(Value::as_array)
        .expect("info --json must include sponsors field");
    assert_eq!(sponsors.len(), 3);
    assert_eq!(sponsors[0]["kind"], "github");
    assert_eq!(sponsors[0]["url"], "https://github.com/sponsors/alice");
    assert_eq!(sponsors[1]["kind"], "kofi");
    assert_eq!(sponsors[2]["kind"], "url");
}

/// Scenario 7 — `pakx search --json` must surface hits from *both*
/// the pakx-registry source and Smithery in the same federated merge.
///
/// Regression for the 2026-05 incident: against production
/// (`registry.pakx.dev`), `pakx search hello-world --json` returned 10
/// Smithery hits and **zero** pakx-registry hits even though
/// `arwenizEr/hello-world@0.1.1` was live. Root cause turned out to
/// upstream of the CLI — the registry list endpoint's `latestVersion`
/// subquery was returning `null`, and the CLI's `list_into_package`
/// fallback was producing a `"0.0.0"` version that still merged into
/// the output… but the prior shape mismatch on the `packages[].id`
/// field meant entries deserialized cleanly yet the surrounding
/// federated test coverage never pinned the dual-source merge. The
/// list endpoint has since been fixed; this test pins the contract so
/// the same regression can never recur silently on the CLI side.
///
/// Mocks `OfficialMcp` empty, `Smithery` with one hit, `pakx-registry`
/// with one hit, then asserts the JSON output contains **both** the
/// Smithery hit (`source: "smithery"`) and the pakx-registry hit
/// (`source: "pakx"`, `version: "0.1.1"` — the live version that
/// originally failed to surface).
#[tokio::test]
#[ignore = "e2e_real_binary — opt in via --ignored"]
async fn e2e_search_json_surfaces_pakx_registry_and_smithery() {
    let official = MockServer::start().await;
    fixture_official_mcp_empty(&official).await;

    let smithery = MockServer::start().await;
    fixture_smithery_package(&smithery, "kapilthakare-cyberpunk/hello-server", "1.0.0").await;

    // pakx-registry list endpoint — mirrors the prod shape after the
    // `latestVersion` subquery fix: `{ packages: [{ id, kind,
    // description, latestVersion }] }`.
    let pakx_registry = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packages": [
                {
                    "id": "arwenizEr/hello-world",
                    "kind": "skill",
                    "description": "first published pakx skill",
                    "visibility": "public",
                    "latestVersion": "0.1.1"
                }
            ]
        })))
        .mount(&pakx_registry)
        .await;

    let out = pakx()
        .args([
            "search",
            "hello-world",
            "--json",
            "--mcp-base-url",
            &official.uri(),
            "--smithery-base-url",
            &smithery.uri(),
            "--pakx-base-url",
            &pakx_registry.uri(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Value = serde_json::from_str(stdout.trim()).expect("search --json valid json");
    let hits = arr.as_array().expect("search --json is an array");

    let pakx_hit = hits
        .iter()
        .find(|h| h.get("source").and_then(Value::as_str) == Some("pakx"))
        .unwrap_or_else(|| {
            panic!("expected at least one pakx-registry hit in federated merge; got: {hits:?}")
        });
    assert_eq!(pakx_hit["id"], "arwenizEr/hello-world");
    assert_eq!(
        pakx_hit["version"], "0.1.1",
        "pakx hit must surface the registry-supplied latestVersion (not the 0.0.0 fallback); \
         got: {pakx_hit}"
    );

    let smithery_hit = hits
        .iter()
        .find(|h| h.get("source").and_then(Value::as_str) == Some("smithery"))
        .unwrap_or_else(|| {
            panic!("expected at least one smithery hit in federated merge; got: {hits:?}")
        });
    assert_eq!(smithery_hit["id"], "kapilthakare-cyberpunk/hello-server");
}
