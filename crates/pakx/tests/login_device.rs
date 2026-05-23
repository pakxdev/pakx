//! Integration tests for `pakx login --device` against a wiremock-
//! backed pakx-registry.
//!
//! Each test sets `--no-open` so the spawned binary never tries to
//! launch a browser on the test host. Polling intervals are forced
//! to a sub-second value via `--poll-interval-secs 0` (clamped to
//! 1s internally) — the device-flow code uses `tokio::time::sleep`
//! which honours this without changing the contract surface.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const BIN: &str = "pakx";
const TEST_TOKEN: &str = "pakx_v1_DEVICE_FLOW_OK";

/// Common initiate-response body. Keeps the `device_code` stable so the
/// test can assert it landed in the poll body too.
fn initiate_body() -> Value {
    json!({
        "device_code": "DEVICE-CODE-ABCDEF-0123456789",
        "user_code": "ABCD-WXYZ",
        "verification_uri": "https://example.test/auth/device",
        "verification_uri_complete": "https://example.test/auth/device?user_code=ABCD-WXYZ",
        "expires_in": 600,
        "interval": 1,
    })
}

/// `Respond` implementation that walks a fixed script of poll
/// responses, returning the next one each time the endpoint is hit
/// and clamping at the final entry for any extra requests (so a
/// late-fire poll after `success` doesn't deadlock the test).
struct ScriptedPoll {
    script: Vec<Value>,
    counter: Arc<AtomicUsize>,
}

impl ScriptedPoll {
    fn new(script: Vec<Value>) -> Self {
        Self {
            script,
            counter: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Respond for ScriptedPoll {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let idx = n.min(self.script.len() - 1);
        ResponseTemplate::new(200).set_body_json(&self.script[idx])
    }
}

async fn mount_initiate(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(initiate_body()))
        .mount(server)
        .await;
}

async fn mount_whoami(server: &MockServer, login: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/whoami"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "id": "u_1", "login": login, "email": null })),
        )
        .mount(server)
        .await;
}

async fn mount_poll(server: &MockServer, script: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/device/poll"))
        .respond_with(ScriptedPoll::new(script))
        .mount(server)
        .await;
}

/// Happy path — initiate, two `pending` polls, then `success` with a
/// token. The token must land in the credentials file and never appear
/// on stdout/stderr.
#[tokio::test]
async fn login_device_happy_path_writes_credentials() {
    let temp = TempDir::new().unwrap();
    let creds = temp.path().join("creds.json");
    let server = MockServer::start().await;
    mount_initiate(&server).await;
    mount_poll(
        &server,
        vec![
            json!({ "status": "pending" }),
            json!({ "status": "pending" }),
            json!({ "status": "success", "token": TEST_TOKEN }),
        ],
    )
    .await;
    mount_whoami(&server, "alice").await;

    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--device",
            "--no-open",
            "--poll-interval-secs",
            "0",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(15))
        .assert()
        .success()
        .get_output()
        .clone();

    let stderr = String::from_utf8(output.stderr).unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Token MUST NOT appear on either stream.
    assert!(
        !stderr.contains(TEST_TOKEN),
        "token leaked to stderr: {stderr}",
    );
    assert!(
        !stdout.contains(TEST_TOKEN),
        "token leaked to stdout: {stdout}",
    );

    // Status / user-code lines belong on stderr only.
    assert!(stderr.contains("ABCD-WXYZ"), "user code missing: {stderr}");
    assert!(
        stderr.contains("signed in") || stderr.contains("logged in"),
        "success line missing: {stderr}",
    );

    // Credentials file got the token.
    let body = std::fs::read_to_string(&creds).unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    let entry = &v["registries"][&server.uri().to_lowercase()];
    assert_eq!(entry["token"], TEST_TOKEN);
    assert_eq!(entry["login"], "alice");
}

/// `slow_down` response bumps the local interval by at least 5s, then
/// the next poll returns `success`. We assert the CLI tolerates the
/// status and surfaces a back-off hint on stderr.
#[tokio::test]
async fn login_device_handles_slow_down_then_success() {
    let temp = TempDir::new().unwrap();
    let creds = temp.path().join("creds.json");
    let server = MockServer::start().await;
    mount_initiate(&server).await;
    mount_poll(
        &server,
        vec![
            json!({ "status": "slow_down" }),
            json!({ "status": "success", "token": TEST_TOKEN }),
        ],
    )
    .await;
    mount_whoami(&server, "alice").await;

    // `--timeout-secs 30` keeps the test fast even with the 5s
    // slow_down bump — the bumped interval becomes 0s + 5s = 5s, and
    // the second poll fires inside the window.
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--device",
            "--no-open",
            "--poll-interval-secs",
            "0",
            "--timeout-secs",
            "30",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .get_output()
        .clone();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("backing off"),
        "expected slow_down hint on stderr: {stderr}",
    );

    let body = std::fs::read_to_string(&creds).unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    let entry = &v["registries"][&server.uri().to_lowercase()];
    assert_eq!(entry["token"], TEST_TOKEN);
}

/// `denied` is terminal — the CLI exits 1 with a "sign-in denied" line
/// on stderr. No credentials file written.
#[tokio::test]
async fn login_device_denied_exits_1() {
    let temp = TempDir::new().unwrap();
    let creds = temp.path().join("creds.json");
    let server = MockServer::start().await;
    mount_initiate(&server).await;
    mount_poll(&server, vec![json!({ "status": "denied" })]).await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--device",
            "--no-open",
            "--poll-interval-secs",
            "0",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(15))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("sign-in denied"));

    assert!(
        !creds.exists(),
        "credentials file must not be written on denial",
    );
}

/// `expired` is terminal — CLI exits 1 with the documented retry hint.
#[tokio::test]
async fn login_device_expired_exits_1() {
    let temp = TempDir::new().unwrap();
    let creds = temp.path().join("creds.json");
    let server = MockServer::start().await;
    mount_initiate(&server).await;
    mount_poll(&server, vec![json!({ "status": "expired" })]).await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--device",
            "--no-open",
            "--poll-interval-secs",
            "0",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(15))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("sign-in window expired"));
}

/// Total-timeout safety net — when the registry only ever returns
/// `pending`, the CLI must abort after the `--timeout-secs` window
/// instead of polling forever. We use a 2-second window so the test
/// runs fast.
#[tokio::test]
async fn login_device_total_timeout_aborts() {
    let temp = TempDir::new().unwrap();
    let creds = temp.path().join("creds.json");
    let server = MockServer::start().await;
    mount_initiate(&server).await;
    mount_poll(&server, vec![json!({ "status": "pending" })]).await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--device",
            "--no-open",
            "--poll-interval-secs",
            "0",
            "--timeout-secs",
            "2",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(15))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("sign-in window expired"));
}

/// Initiate request must carry `clientHostname` and `clientOs` (both
/// strings, capped at 80 chars). The contract is part of the registry
/// API — pin it so future churn surfaces.
#[tokio::test]
async fn login_device_initiate_sends_client_metadata() {
    let temp = TempDir::new().unwrap();
    let creds = temp.path().join("creds.json");
    let server = MockServer::start().await;
    mount_initiate(&server).await;
    mount_poll(
        &server,
        vec![json!({ "status": "success", "token": TEST_TOKEN })],
    )
    .await;
    mount_whoami(&server, "alice").await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "login",
            "--device",
            "--no-open",
            "--poll-interval-secs",
            "0",
            "--registry",
            &server.uri(),
            "--credentials-file",
            creds.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(15))
        .assert()
        .success();

    // Pull every initiate body and assert at least one carried the
    // metadata. clientOs is always present (std::env::consts::OS is
    // never empty). clientHostname is best-effort — on some CI runners
    // gethostname() returns an empty string; tolerate it being absent.
    let initiate_bodies: Vec<Value> = server
        .received_requests()
        .await
        .expect("wiremock recorder enabled")
        .into_iter()
        .filter(|r: &Request| {
            r.method == wiremock::http::Method::POST && r.url.path() == "/api/v1/auth/device"
        })
        .map(|r| serde_json::from_slice::<Value>(&r.body).expect("initiate body is valid json"))
        .collect();

    assert_eq!(initiate_bodies.len(), 1);
    let body = &initiate_bodies[0];
    let os = body["clientOs"].as_str().expect("clientOs string");
    assert!(!os.is_empty(), "clientOs empty: {body}");
    assert!(os.len() <= 80, "clientOs exceeds 80-char cap: {os:?}");
    if let Some(h) = body.get("clientHostname").and_then(Value::as_str) {
        assert!(h.len() <= 80, "clientHostname exceeds 80-char cap: {h:?}");
    }
}

/// Regression — the legacy `--token` flow MUST keep working unchanged.
/// `pakx login --token <pakx_v1_…>` skips the device flow entirely,
/// runs whoami, and writes the credentials file. Same shape as the
/// pre-device path.
#[tokio::test]
async fn login_token_flow_still_works() {
    let temp = TempDir::new().unwrap();
    let creds = temp.path().join("creds.json");
    let server = MockServer::start().await;
    mount_whoami(&server, "alice").await;

    Command::cargo_bin(BIN)
        .unwrap()
        // Important: `env_clear` so a host-set `PAKX_TOKEN` cannot
        // shadow the `--token` we're explicitly passing.
        .env_remove("PAKX_TOKEN")
        .args([
            "login",
            "--registry",
            &server.uri(),
            "--token",
            "pakx_v1_LEGACY_PASTE_PATH",
            "--credentials-file",
            creds.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        .stderr(predicate::str::contains("alice"));

    let body = std::fs::read_to_string(&creds).unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    let entry = &v["registries"][&server.uri().to_lowercase()];
    assert_eq!(entry["token"], "pakx_v1_LEGACY_PASTE_PATH");
    assert_eq!(entry["login"], "alice");
}

/// `--device` and `--token` are mutually exclusive at the clap layer.
/// Passing both is a usage error (exit 2).
#[test]
fn login_device_and_token_are_mutually_exclusive() {
    Command::cargo_bin(BIN)
        .unwrap()
        .env_remove("PAKX_TOKEN")
        .args([
            "login",
            "--device",
            "--token",
            "pakx_v1_anything",
            "--registry",
            "https://example.test",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}
