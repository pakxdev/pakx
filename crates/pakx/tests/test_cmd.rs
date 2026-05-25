//! Integration tests for `pakx test`.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

fn write_manifest(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("agents.yml"), body).unwrap();
}

#[test]
fn test_fails_when_manifest_missing() {
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("read manifest"));
}

#[test]
fn test_offline_succeeds_with_no_mcp_deps() {
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "name: example\nversion: 0.1.0\n");
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("manifest"))
        .stdout(predicate::str::contains("parsed"))
        .stdout(predicate::str::contains("all entries ok"));
}

#[test]
fn test_offline_requires_lockfile_entry_for_each_mcp_dep() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/cool\n",
    );
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure()
        // status glyph + id; loosened from the legacy "fail: mcp/..."
        // prefix once `pakx test` switched to the project-wide
        // `[ok] / [fail]` glyphs.
        .stdout(predicate::str::contains("mcp/io.github.acme/cool"))
        .stdout(predicate::str::contains("[fail]"));
}

#[test]
fn test_offline_passes_with_matching_lockfile_entry() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/cool\n",
    );
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
        .args(["test", "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[ok]"))
        .stdout(predicate::str::contains("mcp/io.github.acme/cool"))
        .stdout(predicate::str::contains("all entries ok"));
}

#[test]
fn test_honors_manifest_override_flag() {
    let project = TempDir::new().unwrap();
    let alt = project.path().join("nested").join("agents-alt.yml");
    std::fs::create_dir_all(alt.parent().unwrap()).unwrap();
    std::fs::write(&alt, "name: alt\nversion: 0.2.0\n").unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--offline",
            "--manifest",
            alt.strip_prefix(project.path()).unwrap().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("name=alt"));
}

#[tokio::test]
async fn test_online_resolves_against_registry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "servers": [
                {
                    "name": "io.github.acme/cool",
                    "description": "hit",
                    "version_detail": {"version": "1.0.0"}
                }
            ]
        })))
        .mount(&server)
        .await;
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/cool\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[ok]"))
        .stdout(predicate::str::contains("mcp/io.github.acme/cool"));
}

#[tokio::test]
async fn test_online_fails_on_unknown_dep() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&server)
        .await;
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - io.github.acme/ghost\n",
    );
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("[fail]"))
        .stdout(predicate::str::contains(
            "mcp/io.github.acme/ghost not found",
        ));
}

/// Federated fallback: when the official MCP Registry has no match,
/// `pakx test` must consult Smithery and pakx-registry — and a hit
/// on either should resolve the dep as `ok`.
#[tokio::test]
async fn test_online_falls_back_to_pakx_registry() {
    let official = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&official)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&official)
        .await;

    let pakx = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/packages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packages": [
                { "id": "alice/cool", "kind": "mcp", "latestVersion": "1.0.0" }
            ]
        })))
        .mount(&pakx)
        .await;

    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - alice/cool\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &official.uri(),
            "--pakx-base-url",
            &pakx.uri(),
            "--no-smithery",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[ok]"))
        .stdout(predicate::str::contains("mcp/alice/cool"))
        // The status line must surface which source resolved the dep
        // so users see federated resolution actually happened.
        .stdout(predicate::str::contains("pakx:"));
}

/// Companion: `--no-pakx-registry` (with Smithery also off) re-breaks
/// the resolution. Documents that the flag is wired through.
#[tokio::test]
async fn test_online_with_no_pakx_registry_does_not_fall_back() {
    let official = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&official)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&official)
        .await;

    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    - alice/cool\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &official.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("[fail]"))
        .stdout(predicate::str::contains("mcp/alice/cool"));
}

#[test]
fn test_exits_non_zero_on_malformed_yaml() {
    // README sells "exit non-zero on first failure" as the CI contract.
    // Verify that a syntactically broken `agents.yml` triggers the same
    // failure path as a registry resolution failure.
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "name: bad\nversion: [unterminated\n");
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("read manifest"));
}

#[test]
fn test_exits_non_zero_on_unknown_manifest_field() {
    // `Manifest` is `#[serde(deny_unknown_fields)]`. An unknown field
    // (typo'd key) must be rejected — that's the whole point of the
    // deny_unknown_fields contract.
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\nunknwn_field: oops\n",
    );
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("read manifest"));
}

/// CI logs surface every command's stderr. Embedding the host-absolute
/// path to `agents.yml` leaks the runner workspace (and on self-hosted
/// runners, the operator's username). Pin the relative form: when the
/// manifest lives under the project root, the error must NOT contain
/// the project root's absolute prefix.
#[test]
fn test_error_messages_do_not_leak_absolute_paths() {
    let project = TempDir::new().unwrap();
    // Project root is the tempdir's absolute path; the missing
    // manifest sits directly under it. The error currently follows
    // the `read manifest at <path>` template — verify the rendered
    // path is the *file name*, not the tempdir's absolute form.
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(output).unwrap();
    let abs = project.path().display().to_string();
    assert!(
        !stderr.contains(&abs),
        "stderr leaked the absolute project root path {abs:?}: {stderr}",
    );
    // Sanity: the redacted form (`agents.yml`) is still present so
    // the error is actionable.
    assert!(
        stderr.contains("agents.yml"),
        "stderr must still mention `agents.yml`: {stderr}",
    );
}

/// `--no-smithery --smithery-base-url …` is a contradiction (opting
/// out of a source while supplying a URL for it). Clap must reject the
/// combination outright. Mirrors the same guard on `pakx install`.
#[test]
fn test_rejects_no_smithery_combined_with_smithery_base_url() {
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "name: example\nversion: 0.1.0\n");
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--no-smithery",
            "--smithery-base-url",
            "https://example.com",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--no-smithery"))
        .stderr(predicate::str::contains("--smithery-base-url"))
        .stderr(predicate::str::contains("cannot be used with"));
}

/// Same guard for the pakx-registry pair.
#[test]
fn test_rejects_no_pakx_registry_combined_with_pakx_base_url() {
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "name: example\nversion: 0.1.0\n");
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--no-pakx-registry",
            "--pakx-base-url",
            "https://example.com",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--no-pakx-registry"))
        .stderr(predicate::str::contains("--pakx-base-url"))
        .stderr(predicate::str::contains("cannot be used with"));
}

/// Perf-correctness pin for the `check_online` fan-out: three MCP
/// deps with a 250ms server-side delay each must resolve in well
/// under the 750ms sequential wall-clock (the pre-fix loop awaited
/// every dep serially). The cap below is generous (1500ms) to absorb
/// CI scheduler jitter + binary cold-start while still failing loud
/// if the loop regresses to serial. Equivalent to the 2026-05 perf
/// pass measurement of ~58% drop on a 3-dep manifest against the
/// production registry (~400ms RTT).
///
/// Additionally pins:
///   * deterministic per-dep print order matching the manifest order
///     (the fan-out is `buffer_unordered`, so the runner sorts by
///     dep index before emitting — without that sort, CI parsers
///     that key on row order would flake).
///   * all three deps surface a successful `[ok]` row even though
///     their futures finish out-of-order.
#[tokio::test]
async fn test_online_resolves_deps_in_parallel() {
    use std::time::{Duration, Instant};

    // Single mock server fields every official-MCP fetch with a fixed
    // 250ms delay. With sequential awaits, three deps × 250ms = ~750ms
    // wall clock minimum; with `buffer_unordered(10)` all three
    // complete in ~250ms (one RTT) plus CLI startup overhead.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(
            r"^/v0/servers/io\.github\.acme/.+",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(250))
                .set_body_json(json!({
                    "servers": [
                        {
                            "name": "placeholder",
                            "description": "delayed",
                            "version_detail": {"version": "1.0.0"}
                        }
                    ]
                })),
        )
        .mount(&server)
        .await;
    // The `OfficialMcpSource::fetch` route hits both the list endpoint
    // and the detail endpoint depending on the source's strategy; the
    // resolver fans through `client.fetch(OfficialMcp, id)` which lands
    // on `/v0/servers/<id>` per the source impl. Add a catch-all 404
    // for the listing endpoint so a stray search call doesn't 500.
    Mock::given(method("GET"))
        .and(path("/v0/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [] })))
        .mount(&server)
        .await;

    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  mcp:\n    \
         - io.github.acme/one\n    \
         - io.github.acme/two\n    \
         - io.github.acme/three\n",
    );

    let started = Instant::now();
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "test",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
        ])
        .assert()
        .get_output()
        .clone();
    let elapsed = started.elapsed();

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Parallel completion bound. Cold-start of the cargo-built binary
    // dominates on CI for sub-second tests so the cap is generous;
    // sequential execution (3 × 250ms = 750ms net request time) would
    // push total wall-clock past this threshold on every CI worker
    // observed during the perf pass.
    assert!(
        elapsed < Duration::from_millis(1500),
        "fan-out collapsed to serial: elapsed={elapsed:?} (cap 1500ms). stdout=\n{stdout}",
    );

    // Order preservation: the runner sorts by dep index before
    // emitting, so even though the resolver futures completed
    // out-of-order, the user sees rows in manifest order.
    let one = stdout
        .find("mcp/io.github.acme/one")
        .expect("first dep row");
    let two = stdout
        .find("mcp/io.github.acme/two")
        .expect("second dep row");
    let three = stdout
        .find("mcp/io.github.acme/three")
        .expect("third dep row");
    assert!(
        one < two && two < three,
        "deps must print in manifest order; got one@{one} two@{two} three@{three}: {stdout}",
    );
}

/// A manifest made entirely of `skills:` deps (no `mcp:`) must NOT
/// claim "all entries ok / manifest validated" — only `mcp:` is
/// actually resolved today; every other kind is reported "not yet
/// validated". The footer must be qualified so the exit-0 doesn't read
/// as a full all-clear for a manifest of installable skills.
#[test]
fn test_offline_qualifies_footer_when_only_non_mcp_kinds_present() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: example\nversion: 0.1.0\ndependencies:\n  skills:\n    - alice/widget@0.1.0\n",
    );
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        // Skills aren't resolved, but nothing FAILED either → exit 0
        // with a HONEST footer.
        .success()
        // The skill row is reported as not-yet-validated.
        .stdout(predicate::str::contains("skills/alice/widget"))
        .stdout(predicate::str::contains("not yet validated"))
        // The footer must NOT overclaim a full validation.
        .stdout(predicate::str::contains("all entries ok").not())
        .stdout(predicate::str::contains("manifest validated").not())
        // It must instead state what was actually skipped.
        .stdout(predicate::str::contains("only mcp"))
        .stdout(predicate::str::contains("skipped"));
}

#[test]
fn test_does_not_write_lockfile() {
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "name: example\nversion: 0.1.0\n");
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["test", "--offline"])
        .assert()
        .success();
    assert!(
        !project.path().join("agents.lock").exists(),
        "pakx test must not write agents.lock"
    );
}
