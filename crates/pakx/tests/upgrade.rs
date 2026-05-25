//! End-to-end coverage for `pakx upgrade` against a wiremock server
//! standing in for the GitHub Releases API.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

fn workspace_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

#[tokio::test]
async fn reports_up_to_date_when_release_matches_workspace() {
    let server = MockServer::start().await;
    let tag = format!("v{}", workspace_version());
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": tag,
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("is the latest release"));
}

#[tokio::test]
async fn reports_newer_release_with_upgrade_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v999.999.999",
            "html_url": "https://example.invalid/release-notes",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("A newer pakx is available"))
        .stdout(predicate::str::contains("999.999.999"))
        .stdout(predicate::str::contains("brew upgrade pakx"))
        .stdout(predicate::str::contains("scoop update pakx"))
        .stdout(predicate::str::contains(
            "cargo install pakx-cli --force --locked",
        ));
}

/// The cargo-test harness binary lives under `target/`, which
/// `detect_channel` classifies as `Unknown`. So an upgrade-available run
/// from the test binary must fall through to the read-only channel menu
/// and NEVER spawn a package manager / installer. This is the property
/// that keeps every other integration test safe: the menu is printed,
/// nothing runs, exit code stays 0.
#[tokio::test]
async fn unknown_channel_prints_menu_and_does_not_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v999.999.999",
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    // No `--yes`: assert_cmd has no TTY, but for the Unknown channel the
    // TTY/--yes guard is never reached — the menu is printed regardless.
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "upgrade via your install channel:",
        ));
}

/// `--check` is read-only: even when an upgrade is available it must do
/// the version check, report status, and never run an upgrade command.
/// (Under the test harness the channel is Unknown, so we additionally
/// confirm the menu path — the contract that `--check` never spawns is
/// asserted at the unit level for known channels.)
#[tokio::test]
async fn check_flag_is_read_only_when_upgrade_available() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v999.999.999",
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--check",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("A newer pakx is available"));
}

/// With `--yes` the command still succeeds for the Unknown (menu) path
/// without prompting — proves `--yes` is accepted and the no-prompt path
/// never hangs the suite.
#[tokio::test]
async fn yes_flag_accepted_without_hanging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v999.999.999",
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--yes",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success();
}

/// THE core no-hang guard: a KNOWN channel (forced cargo) + upgrade
/// available + no `--yes` + no TTY (`assert_cmd` gives no TTY) must:
///   - NOT hang waiting on stdin,
///   - NOT spawn `cargo install`,
///   - print the command + a `--yes` hint,
///   - exit 0.
/// `--force-channel cargo` lets us exercise the real prompt path that
/// the Unknown (target/) test-binary path can't reach. We pick `cargo`
/// because even if the guard regressed and it DID spawn, `cargo install
/// pakx-cli` would fail fast / be detectable — but the assertion is that
/// it never gets there.
#[tokio::test]
async fn known_channel_no_tty_no_yes_prints_command_and_exits_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v999.999.999",
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--force-channel",
            "cargo",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "cargo install pakx-cli --force --locked",
        ))
        .stdout(predicate::str::contains("Re-run with"))
        .stdout(predicate::str::contains("--yes"));
}

/// `--check` with a KNOWN channel reports the resolved channel + the
/// command it WOULD run, and never spawns it. Exit 0.
#[tokio::test]
async fn check_with_known_channel_reports_would_run_only() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v999.999.999",
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--check",
            "--force-channel",
            "brew",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("detected install channel: brew"))
        .stdout(predicate::str::contains("would run: brew upgrade pakx"))
        .stdout(predicate::str::contains("not running"));
}

/// windows+script forced: must print the fresh-shell command (the
/// `irm | iex` form) and a note about Windows locking the live exe,
/// never spawn. Channel + plan are platform-agnostic in code, so this
/// holds on the CI runner regardless of host OS.
#[tokio::test]
async fn forced_script_channel_on_unix_runs_install_sh() {
    // On a unix CI runner, forced script → the sh/install.sh Run plan;
    // no-TTY guard then prints the command + hint and exits 0.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v999.999.999",
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    let assert = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--force-channel",
            "script",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success();

    // On unix the no-TTY guard prints the install.sh command; on windows
    // the windows+script branch prints the irm|iex command. Either way a
    // pakx.dev install URL is surfaced and nothing is spawned.
    if cfg!(windows) {
        assert.stdout(predicate::str::contains(
            "irm https://pakx.dev/install.ps1 | iex",
        ));
    } else {
        assert.stdout(predicate::str::contains(
            "curl -fsSL https://pakx.dev/install.sh | sh",
        ));
    }
}

#[tokio::test]
async fn reports_dev_build_when_local_is_newer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v0.0.1",
            "html_url": "https://example.invalid",
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Running a dev build?"));
}

#[tokio::test]
async fn surfaces_http_error_when_releases_api_is_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "upgrade",
            "--releases-url",
            &format!("{}/releases/latest", server.uri()),
        ])
        .assert()
        .failure();
}

/// `--releases-url` is a hidden test override but still user-supplied:
/// route it through `validate_base_url` so a plaintext `http://evil.com`
/// override cannot quietly exfiltrate the upgrade probe over the wire.
#[test]
fn upgrade_rejects_plaintext_http_releases_url() {
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["upgrade", "--releases-url", "http://evil.com/latest"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to use registry base URL",
        ));
}
