//! Integration tests for `pakx update`.
//!
//! Each scenario writes fixture `agents.yml` + `agents.lock` bodies,
//! mounts a `wiremock` server with the expected federated-registry
//! responses, and drives the real built `pakx` binary through
//! `assert_cmd`. Mirrors the shape of `tests/outdated.rs` (the same
//! lockfile + pakx-registry fixtures are reused).
//!
//! Behaviour asserted:
//!   - `pakx update --yes` rewrites `agents.yml` to the latest version
//!     for every outdated dep and triggers a follow-up install that
//!     re-pins the lockfile to the new version.
//!   - `pakx update --yes --no-install` rewrites the manifest only.
//!   - `pakx update --dry-run` writes nothing on disk.
//!   - `pakx update <id>@<version>` pins to the requested version
//!     verbatim (no registry round-trip), allowing downgrades.
//!   - `pakx update <id>` resolves a single id via the registry.
//!   - Non-shorthand specs (git / registry-object) are rejected with a
//!     diagnostic.
//!   - Exit code 2 when the registry cannot determine a target
//!     version for a single explicitly-requested id.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

const MANIFEST_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const ENTRY_INTEGRITY: &str = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

fn write_manifest(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("agents.yml"), body).expect("write agents.yml");
}

fn write_lockfile(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("agents.lock"), body).expect("write agents.lock");
}

/// Lockfile body with one pakx-registry skill entry pinned at
/// `version`. Same id used by the live-smoke target.
fn pakx_lockfile(version: &str) -> String {
    format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{
  "skills/arwenizEr/hello-world@{version}":{{
    "name":"arwenizEr/hello-world",
    "type":"skills",
    "version":"{version}",
    "resolvedFrom":"https://registry.pakx.dev/api/v1/packages/arwenizEr/hello-world/{version}",
    "registry":"pakx",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
    )
}

/// Mount the package-detail endpoint returning the given (version,
/// deprecated) tuples in highest-to-lowest order — matches the
/// production sort. Sha256s are zeros so the optional install path
/// pre-resolves cleanly without a real tarball download.
async fn mount_pakx_detail(server: &MockServer, versions: &[(&str, bool)]) {
    let versions_json: Vec<Value> = versions
        .iter()
        .map(|(v, deprecated)| {
            json!({
                "version": v,
                "sha256": "0".repeat(64),
                "sizeBytes": 1024,
                "publishedAt": "2026-05-22T00:00:00Z",
                "deprecatedAt": if *deprecated { Some("2026-05-23T00:00:00Z") } else { None },
            })
        })
        .collect();
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/arwenizEr/hello-world"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "arwenizEr/hello-world",
            "kind": "skills",
            "description": "update e2e",
            "versions": versions_json,
        })))
        .mount(server)
        .await;
}

/// Mount the per-version detail endpoint. Returns a fake tarball URL
/// and sha256 (zeros) which the install step will then try to
/// download — covered by `mount_pakx_tarball`.
async fn mount_pakx_version(server: &MockServer, version: &str, sha256: &str, tarball_url: &str) {
    let path = format!("/api/v1/packages/arwenizEr/hello-world/{version}");
    Mock::given(method("GET"))
        .and(wm_path(path))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "arwenizEr/hello-world",
            "version": version,
            "sha256": sha256,
            "sizeBytes": 64,
            "tarballUrl": tarball_url,
        })))
        .mount(server)
        .await;
}

/// Minimal gzipped tar containing a single `SKILL.md` file. The
/// install step downloads + sha-verifies + extracts; we serve a real
/// (tiny) tarball so the extraction step succeeds. Returns the bytes
/// + their hex sha256 so the per-version mount can pin them together.
fn build_skill_tarball() -> (Vec<u8>, String) {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        let body = b"---\nname: hello-world\nversion: 0.1.2\n---\n# hi\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "SKILL.md", &body[..])
            .unwrap();
        builder.finish().unwrap();
    }
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&tar_buf).unwrap();
    let compressed = gz.finish().unwrap();
    let mut hasher = Sha256::new();
    hasher.update(&compressed);
    let digest = hasher.finalize();
    let hex = digest.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    });
    (compressed, hex)
}

/// Mount a tarball download URL that returns the supplied bytes.
async fn mount_pakx_tarball(server: &MockServer, route: &str, body: Vec<u8>) {
    Mock::given(method("GET"))
        .and(wm_path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(server)
        .await;
}

/// Mount an empty MCP Registry: every detail 404s and search returns
/// `[]`. Used so the in-process `pakx install` doesn't hammer
/// production MCP endpoints during the post-update reconciliation.
async fn mount_empty_mcp(server: &MockServer) {
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

#[tokio::test]
async fn update_yes_rewrites_manifest_to_latest_then_installs() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    let mcp = MockServer::start().await;
    mount_empty_mcp(&mcp).await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;

    let (tarball_bytes, sha_hex) = build_skill_tarball();
    let tarball_route = "/tarballs/hello-world-0.1.2.tgz";
    let tarball_url = format!("{}{tarball_route}", pakx_registry.uri());
    mount_pakx_version(&pakx_registry, "0.1.2", &sha_hex, &tarball_url).await;
    mount_pakx_tarball(&pakx_registry, tarball_route, tarball_bytes).await;

    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.0\n",
    );
    write_lockfile(project.path(), &pakx_lockfile("0.1.0"));

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "--yes",
            "--pakx-base-url",
            &pakx_registry.uri(),
            "--mcp-base-url",
            &mcp.uri(),
            "--no-smithery",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("arwenizEr/hello-world"),
        "stdout should list the updated id; got:\n{stdout}"
    );
    assert!(
        stdout.contains("0.1.2"),
        "stdout should reference the new pin; got:\n{stdout}"
    );

    // Manifest must now pin 0.1.2.
    let yml = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    assert!(
        yml.contains("arwenizEr/hello-world@0.1.2"),
        "agents.yml must be rewritten to the new pin; got:\n{yml}"
    );
    assert!(
        !yml.contains("arwenizEr/hello-world@0.1.0"),
        "agents.yml must no longer contain the old pin; got:\n{yml}"
    );

    // Lockfile must now reflect the new pin too (install reconciled).
    let lock: Value =
        serde_json::from_str(&std::fs::read_to_string(project.path().join("agents.lock")).unwrap())
            .unwrap();
    let entries = lock["entries"].as_object().expect("entries object");
    assert!(
        entries.contains_key("skills/arwenizEr/hello-world@0.1.2"),
        "lockfile must reflect the new version; got entries: {entries:?}"
    );
}

#[tokio::test]
async fn update_no_install_rewrites_manifest_only() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;

    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.0\n",
    );
    write_lockfile(project.path(), &pakx_lockfile("0.1.0"));

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "--yes",
            "--no-install",
            "--pakx-base-url",
            &pakx_registry.uri(),
        ])
        .assert()
        .success();

    // Manifest rewritten to new pin.
    let yml = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    assert!(yml.contains("arwenizEr/hello-world@0.1.2"));

    // Lockfile unchanged — `--no-install` means no reconcile.
    let lock = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    assert!(
        lock.contains("\"version\":\"0.1.0\""),
        "lockfile must still show the old pin under --no-install; got:\n{lock}"
    );
}

#[tokio::test]
async fn update_dry_run_does_not_modify_files() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;

    let manifest_body =
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.0\n";
    let lockfile_body = pakx_lockfile("0.1.0");
    write_manifest(project.path(), manifest_body);
    write_lockfile(project.path(), &lockfile_body);

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "--yes",
            "--dry-run",
            "--pakx-base-url",
            &pakx_registry.uri(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("would update"),
        "dry-run must print a `would update` preview line; got:\n{stdout}"
    );

    // Disk state untouched.
    let yml = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    assert_eq!(yml, manifest_body, "agents.yml must be untouched");
    let lock = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    assert_eq!(lock, lockfile_body, "agents.lock must be untouched");
}

#[tokio::test]
async fn update_pinned_form_skips_registry_and_allows_downgrade() {
    // `pakx update <id>@<version>` should pin verbatim — no registry
    // query, so downgrades work even when the registry is empty /
    // unreachable. We deliberately don't mount any pakx-registry
    // response: any HTTP traffic to it would be a bug.
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    // No mounts — any GET would 404, surfacing a bug.

    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.2\n",
    );
    write_lockfile(project.path(), &pakx_lockfile("0.1.2"));

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "arwenizEr/hello-world@0.1.0",
            "--no-install",
            "--pakx-base-url",
            &pakx_registry.uri(),
        ])
        .assert()
        .success();
    let yml = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    assert!(
        yml.contains("arwenizEr/hello-world@0.1.0"),
        "manifest must be downgraded to the requested pin; got:\n{yml}"
    );

    // Confirm no requests landed.
    let recorded = pakx_registry.received_requests().await.unwrap();
    assert!(
        recorded.is_empty(),
        "pinned form must not hit the registry; got {} request(s)",
        recorded.len()
    );
}

#[tokio::test]
async fn update_rejects_non_shorthand_specs() {
    // Manifest contains a git-form spec for the requested id, so
    // `update_shorthand` returns `NonShorthand` — surfaced as a hard
    // error with the spec-mandated diagnostic.
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - git: https://example.test/repo.git\n",
    );
    // Use the git URL itself as the id so the matcher hits a Git
    // spec exactly.
    write_lockfile(
        project.path(),
        &format!(
            r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{
  "mcp/https://example.test/repo.git@0.1.0":{{
    "name":"https://example.test/repo.git",
    "type":"mcp",
    "version":"0.1.0",
    "resolvedFrom":"git:https://example.test/repo.git",
    "registry":"git",
    "integrity":"{ENTRY_INTEGRITY}",
    "agents":["claude-code"],
    "dependencies":[]
  }}
}}}}
"#
        ),
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "https://example.test/repo.git@0.1.2",
            "--no-install",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("git or registry-object spec"));
}

#[tokio::test]
async fn update_no_args_says_up_to_date_when_nothing_outdated() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false)]).await;
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.2\n",
    );
    write_lockfile(project.path(), &pakx_lockfile("0.1.2"));

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["update", "--yes", "--pakx-base-url", &pakx_registry.uri()])
        .assert()
        .success()
        .stderr(predicate::str::contains("up to date"));
}

/// Regression for the non-TTY hang: `pakx update` (no explicit id, no
/// `--yes`) prompts per outdated dep via `inquire::Confirm`. With an
/// outdated dep present and no terminal on stdin that prompt would block
/// forever — it must instead fail fast with the "not a TTY" hint. The
/// manifest pins `0.1.0` while the registry's latest is `0.1.2`, so a
/// plan exists and the prompt path is reached.
#[tokio::test]
async fn update_without_yes_and_no_tty_bails_instead_of_hanging() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.0\n",
    );
    write_lockfile(project.path(), &pakx_lockfile("0.1.0"));

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        // Empty stdin → not a TTY. Guard must fire before the prompt.
        .write_stdin("")
        .args([
            "update",
            "--no-install",
            "--pakx-base-url",
            &pakx_registry.uri(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("stdin is not a TTY"))
        .stderr(predicate::str::contains("--yes"));

    // Manifest must be unchanged — the bail happens before any rewrite.
    let yml = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    assert!(
        yml.contains("arwenizEr/hello-world@0.1.0"),
        "manifest must keep the old pin; got:\n{yml}"
    );
}

#[tokio::test]
async fn update_bare_id_exits_two_when_registry_unreachable() {
    // The lockfile pins the id but no registry mount answers — the
    // outdated row lands as `error`. Per spec, a single-id update
    // with no determinable target version exits 2.
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    // No mounts — every GET 404s.

    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.0\n",
    );
    write_lockfile(project.path(), &pakx_lockfile("0.1.0"));

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "arwenizEr/hello-world",
            "--no-install",
            "--pakx-base-url",
            &pakx_registry.uri(),
        ])
        .assert()
        .code(2);
}

/// `pakx update --kind <type>` disambiguates when an id is declared
/// under more than one section. Without it, the run errors out with
/// "declared under multiple sections"; with it, only the matching
/// section is rewritten and the sibling section is left untouched.
#[tokio::test]
async fn update_kind_flag_resolves_ambiguous_id_to_one_section() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - shared/dep@0.1.0\n  skills:\n    - shared/dep@0.1.0\n",
    );
    // No lockfile needed — the explicit `<id>@<version>` form skips
    // both the registry query and the outdated check.

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "shared/dep@0.2.0",
            "--kind",
            "skills",
            "--no-install",
            "--yes",
        ])
        .assert()
        .success();

    let yml = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    let m = pakx_core::parse_manifest(&yml, None).unwrap();
    // Skills section bumped, mcp untouched.
    let skills = m.dependencies.skills.as_ref().unwrap();
    let mcp = m.dependencies.mcp.as_ref().unwrap();
    let dump = |list: &[pakx_core::DepSpec]| -> Vec<String> {
        list.iter()
            .map(|d| match d {
                pakx_core::DepSpec::String(s) => s.as_str().to_owned(),
                _ => String::new(),
            })
            .collect()
    };
    assert_eq!(dump(skills), vec!["shared/dep@0.2.0".to_string()]);
    assert_eq!(
        dump(mcp),
        vec!["shared/dep@0.1.0".to_string()],
        "mcp section must remain at the old pin",
    );
}

/// Without `--kind`, an ambiguous id errors out cleanly and the
/// rerun-hint mentions every candidate section. Locks in the
/// replacement for the prior `(TODO)` error message.
#[tokio::test]
async fn update_errors_on_ambiguous_id_without_kind_flag() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - shared/dep@0.1.0\n  skills:\n    - shared/dep@0.1.0\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["update", "shared/dep@0.2.0", "--no-install", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("declared under multiple sections"))
        .stderr(predicate::str::contains("--kind"))
        // The prior message included an explicit `(TODO)` — we never
        // ship `--kind` and `(TODO)` in the same string again.
        .stderr(predicate::str::contains("(TODO)").not());
}

/// `--kind` aimed at a kind that doesn't actually hold the requested
/// id surfaces the clean diagnostic instead of silently rewriting a
/// sibling section. Mirrors the `pakx remove --kind` not-declared
/// branch byte-for-byte.
#[tokio::test]
async fn update_kind_flag_errors_when_no_entry_of_that_kind_matches() {
    let project = TempDir::new().unwrap();
    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  mcp:\n    - alpha/dep@0.1.0\n",
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "alpha/dep@0.2.0",
            "--kind",
            "skills",
            "--no-install",
            "--yes",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no `skills` entry named `alpha/dep` in agents.yml",
        ));
}

#[tokio::test]
async fn update_missing_id_in_lockfile_errors_loudly() {
    let project = TempDir::new().unwrap();
    write_manifest(project.path(), "name: demo\nversion: 0.1.0\n");
    write_lockfile(
        project.path(),
        &format!(
            r#"{{"lockfileVersion":1,"manifestHash":"{MANIFEST_HASH}","entries":{{}}}}
"#
        ),
    );

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["update", "missing/dep@1.0.0", "--no-install"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing/dep"));
}

/// Regression for the `--directory` propagation gap in the `--no-install`
/// `→ next:` hint. When the user passes `--directory <subdir>`, the
/// printed hint must include `--directory <subdir>` so the user can
/// copy-paste it verbatim — previously the hint dropped the directory
/// flag, leaving the user to remember to re-thread it.
#[tokio::test]
async fn update_no_install_hint_propagates_directory_arg() {
    let project = TempDir::new().unwrap();
    let subdir = project.path().join("sub");
    std::fs::create_dir_all(&subdir).unwrap();

    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;

    write_manifest(
        &subdir,
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.0\n",
    );
    write_lockfile(&subdir, &pakx_lockfile("0.1.0"));

    let subdir_str = subdir.to_string_lossy().to_string();
    let out = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "update",
            "--yes",
            "--no-install",
            "--directory",
            &subdir_str,
            "--pakx-base-url",
            &pakx_registry.uri(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let expected = format!("\u{2192} next: pakx install --directory {subdir_str}");
    assert!(
        stdout.contains(&expected),
        "→ next hint must propagate --directory; expected {expected:?}, got stdout:\n{stdout}"
    );
}

/// Round-86 wiring guard: `pakx update --no-cache` must parse and
/// the full update + install reconciliation must still succeed.
/// The cache-bypass semantic itself is unit-tested by
/// `commands::cache_tempdir::cache_dir_at_clamps_ttl_when_no_cache_set`
/// — this integration test pins the surface contract (flag accepted,
/// command returns 0, manifest rewritten as usual). Documents the
/// closure of the canonical `pakx publish && pakx update --yes
/// --no-cache` post-publish re-pin loop.
#[tokio::test]
async fn update_yes_no_cache_succeeds_end_to_end() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    let mcp = MockServer::start().await;
    mount_empty_mcp(&mcp).await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;

    let (tarball_bytes, sha_hex) = build_skill_tarball();
    let tarball_route = "/tarballs/hello-world-0.1.2.tgz";
    let tarball_url = format!("{}{tarball_route}", pakx_registry.uri());
    mount_pakx_version(&pakx_registry, "0.1.2", &sha_hex, &tarball_url).await;
    mount_pakx_tarball(&pakx_registry, tarball_route, tarball_bytes).await;

    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.0\n",
    );
    write_lockfile(project.path(), &pakx_lockfile("0.1.0"));

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "--yes",
            "--no-cache",
            "--pakx-base-url",
            &pakx_registry.uri(),
            "--mcp-base-url",
            &mcp.uri(),
            "--no-smithery",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    // Manifest must now pin 0.1.2 — same outcome as the cached
    // happy-path. The `--no-cache` switch is observable upstream
    // (fresh registry queries) but the user-visible result is the
    // same: pinned at the latest available version.
    let yml = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    assert!(
        yml.contains("arwenizEr/hello-world@0.1.2"),
        "agents.yml must be rewritten to the new pin under --no-cache; got:\n{yml}"
    );
}

/// Help-text guard: `pakx update --help` must surface `--no-cache`.
/// Pins the round-86 wiring so a future flag-rename does not silently
/// drop the bypass from the documented surface.
#[test]
fn update_help_lists_no_cache() {
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["update", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--no-cache"));
}

/// Inverse: without `--directory`, the hint stays in its original
/// `→ next: pakx install` shape (no spurious trailing flag).
#[tokio::test]
async fn update_no_install_hint_omits_directory_when_unset() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    mount_pakx_detail(&pakx_registry, &[("0.1.2", false), ("0.1.0", false)]).await;

    write_manifest(
        project.path(),
        "name: demo\nversion: 0.1.0\ndependencies:\n  skills:\n    - arwenizEr/hello-world@0.1.0\n",
    );
    write_lockfile(project.path(), &pakx_lockfile("0.1.0"));

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "update",
            "--yes",
            "--no-install",
            "--pakx-base-url",
            &pakx_registry.uri(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\u{2192} next: pakx install"),
        "→ next hint expected; got stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("--directory"),
        "→ next hint must not carry --directory when unset; got stdout:\n{stdout}"
    );
}
