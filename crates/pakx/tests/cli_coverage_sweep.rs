//! Coverage-sweep integration tests for CLI surfaces shipped in
//! rounds 32–44. Each test fills a gap noted during the
//! `test/cli-coverage-sweep-rounds-32-44` audit: a public CLI
//! subcommand surface that had fewer than 2–3 happy/edge tests, an
//! edge-case the existing tests didn't pin, or a behavioural contract
//! that only had a "flag parses" test rather than a behavioural
//! assertion.
//!
//! Discipline:
//!   - Every test that spins HTTP uses `wiremock` (no live registry).
//!   - Every test that touches disk uses `tempfile::TempDir` (no shared
//!     state).
//!   - The cache-root pattern from round 30 (`pakx-<cmd>-cache-<pid>-<nanos>`)
//!     is the CLI's discipline; tests here assert their own tempdir
//!     scope only.
//!   - One happy / one edge / one error per surface where possible.

use std::collections::BTreeMap;
use std::io::Write;

use assert_cmd::Command;
use flate2::write::GzEncoder;
use flate2::Compression;
use predicates::prelude::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

// ---------------------------------------------------------------------------
// Tarball helpers — local copy of the helper used in skills_e2e /
// bundle_e2e / export. The cargo integration-test layout makes each
// file its own compilation unit, so a `mod common;` shared module
// would drag in unrelated symbols.
// ---------------------------------------------------------------------------

fn build_tarball(entries: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let mut buf = Vec::new();
    {
        let encoder = GzEncoder::new(&mut buf, Compression::default());
        let mut tar_builder = tar::Builder::new(encoder);
        for (p, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_path(p).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar_builder.append(&header, *contents).unwrap();
        }
        let encoder = tar_builder.into_inner().unwrap();
        encoder.finish().unwrap();
    }
    let _ = std::io::sink().flush();
    let sha = bytes_to_hex(&Sha256::digest(&buf));
    (buf, sha)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

async fn mock_pakx_skill_registry(
    id: &str,
    version: &str,
    sha256_hex: &str,
    tarball_bytes: Vec<u8>,
) -> MockServer {
    let server = MockServer::start().await;
    let blob_path = format!("/blob/{id}/{version}");
    let signed_url = format!("{}{}?download=1&sig=ABC", server.uri(), blob_path);
    let (owner, name) = id.split_once('/').expect("id has /");

    Mock::given(method("GET"))
        .and(wm_path(format!("/api/v1/packages/{owner}/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id,
            "kind": "skill",
            "description": "test",
            "latestVersion": version,
            "versions": [
                { "version": version, "sha256": sha256_hex, "sizeBytes": tarball_bytes.len() }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path(format!(
            "/api/v1/packages/{owner}/{name}/{version}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id,
            "version": version,
            "sha256": sha256_hex,
            "sizeBytes": tarball_bytes.len(),
            "publishedAt": "2026-05-23T00:00:00Z",
            "deprecatedAt": null,
            "tarballUrl": signed_url,
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path(blob_path))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(tarball_bytes)
                .insert_header("content-type", "application/gzip"),
        )
        .mount(&server)
        .await;
    server
}

fn seed_skill_manifest(project: &TempDir, id: &str, version: &str) {
    std::fs::write(
        project.path().join("agents.yml"),
        format!("name: demo\nversion: 0.0.0\ndependencies:\n  skills:\n    - {id}@{version}\n"),
    )
    .unwrap();
}

// ===========================================================================
// `pakx info <id> <field>` — additional shape gaps.
// ===========================================================================

/// Empty field segment — `pakx info <id> ""` — must error at parse time
/// (the manifest path module returns `PathError::Empty`). The user gets
/// a clean diagnostic, not a cryptic `serde` panic, so the path module's
/// `Empty` error must reach the CLI surface for the field-query flag too.
#[tokio::test]
async fn info_field_query_empty_field_errors_cleanly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/alice/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "description": "hi",
            "versions": []
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args(["info", "alice/hello", "", "--registry", &server.uri()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must not be empty"));
}

/// `--no-cache` on `pakx info --version` must parse + execute. The
/// existing happy-path coverage exercises `--version` without
/// `--no-cache`; this pins the flag composes with the per-version path
/// (which uses its own tempdir cache + zero-TTL clamp).
#[tokio::test]
async fn info_with_version_accepts_no_cache_flag() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/alice/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "kind": "skills",
            "versions": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/alice/hello/0.1.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "version": "0.1.0",
            "sha256": "abc",
            "sizeBytes": 100,
            "publishedAt": "2026-05-22T00:00:00Z",
            "deprecatedAt": null,
            "tarballUrl": "https://example.com/t.tgz"
        })))
        .mount(&server)
        .await;

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "info",
            "alice/hello",
            "--version",
            "0.1.0",
            "--no-cache",
            "--registry",
            &server.uri(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0.1.0"));
}

/// Field-query against the per-version endpoint composing with `--json`
/// — pin the JSON shape of `pakx info <id> <field> --version <v> --json`
/// returns a JSON scalar. Existing tests cover the same composition in
/// human mode; this seals the `--json` branch.
#[tokio::test]
async fn info_field_query_version_with_json_emits_json_scalar() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/alice/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "kind": "skills",
            "versions": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/alice/hello/0.1.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "version": "0.1.0",
            "sha256": "abcdef12",
            "sizeBytes": 100,
            "publishedAt": "2026-05-22T00:00:00Z",
            "deprecatedAt": null,
            "tarballUrl": "https://example.com/t.tgz"
        })))
        .mount(&server)
        .await;

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "info",
            "alice/hello",
            "sha256",
            "--version",
            "0.1.0",
            "--json",
            "--registry",
            &server.uri(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Must parse as JSON (quoted string), not as a bare scalar.
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON, got `{stdout}`: {e}"));
    assert_eq!(parsed.as_str(), Some("abcdef12"));
}

/// `pakx info <id> sponsors` returns the (possibly empty) sponsors
/// array — pin the always-emit-array contract from spec §2. The
/// field-query layer must reach into the body and surface the
/// stable empty array, not `null`.
#[tokio::test]
async fn info_field_query_sponsors_emits_array_even_when_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/alice/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "alice/hello",
            "kind": "skills",
            "description": "no sponsors here",
            "versions": []
            // sponsors deliberately omitted from upstream — CLI must
            // still emit [] (Default::default() on the Vec<Sponsor>).
        })))
        .mount(&server)
        .await;

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "info",
            "alice/hello",
            "sponsors",
            "--registry",
            &server.uri(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON array, got `{stdout}`: {e}"));
    assert!(parsed.is_array(), "expected array, got {parsed}");
    assert_eq!(parsed.as_array().unwrap().len(), 0);
}

// ===========================================================================
// `pakx pack` — additional shape gaps.
// ===========================================================================

/// `pakx pack -o <dir>` — the `-o` short alias is documented in
/// `commands/pack.rs::PackArgs` and must keep working alongside the
/// long forms `--output` / `--out`.
#[test]
fn pack_accepts_o_short_form() {
    let src = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    std::fs::write(
        src.path().join("SKILL.md"),
        "---\nname: demo\nversion: 0.1.0\n---\n# hi\n",
    )
    .unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            src.path().to_str().unwrap(),
            "-o",
            out.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(out.path().join("demo-0.1.0.tgz").is_file());
}

/// `pakx pack <missing-dir>` — non-existent source path must surface a
/// clean error referencing the SKILL.md the packer was trying to read,
/// not a `serde_yaml` cascade or a raw stack trace. Maps the missing
/// dir to a `read SKILL.md: ...` context layer so the user knows the
/// pack source was the problem.
#[test]
fn pack_missing_source_dir_errors_cleanly() {
    let ghost = TempDir::new().unwrap();
    let phantom = ghost.path().join("does-not-exist");
    let out = TempDir::new().unwrap();
    let assertion = Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "pack",
            phantom.to_str().unwrap(),
            "--output",
            out.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    // The CLI surfaces the packer's `read SKILL.md: <io error>` chain
    // — which is the right diagnostic, since SKILL.md is what the
    // packer was trying to open inside the missing dir. We accept any
    // of the canonical OS-error phrases since their text differs across
    // Windows / macOS / Linux but the SKILL.md context line is portable.
    assert!(
        stderr.contains("SKILL.md"),
        "missing-source error must surface the SKILL.md context line; got: {stderr}"
    );
    // And the tarball must not have been written despite the failure.
    assert!(
        !out.path().join("demo-0.1.0.tgz").exists(),
        "no tarball should be written on a missing-source failure"
    );
}

/// `--dry-run` without `--json` and without `--output` — both flags are
/// optional. A dry-run with neither flag must succeed (no `.tgz`
/// anywhere) — pins that `--output` is genuinely optional and not
/// silently required even in dry-run mode.
#[test]
fn pack_dry_run_without_output_succeeds() {
    let src = TempDir::new().unwrap();
    std::fs::write(
        src.path().join("SKILL.md"),
        "---\nname: demo\nversion: 0.1.0\ndescription: tidy.\n---\n# hi\n",
    )
    .unwrap();
    // No --output flag at all.
    Command::cargo_bin(BIN)
        .unwrap()
        .args(["pack", src.path().to_str().unwrap(), "--dry-run"])
        .assert()
        .success();
}

// ===========================================================================
// `pakx export` — JSON shape with --force flag composition.
// ===========================================================================

/// Walk a directory tree and collect every file's relative path → byte
/// contents.
fn snapshot_tree(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let ft = entry.file_type().unwrap();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

/// `pakx export --json --force` — composing `--force` (overwrite an
/// existing destination) with `--json` must still emit a single
/// newline-terminated JSON object on stdout. Pins that the `--force`
/// branch doesn't accidentally short-circuit the JSON emit path.
#[tokio::test]
async fn export_json_with_force_still_emits_payload() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/force-json";
    let version = "0.1.0";

    let (tarball, sha) = build_tarball(&[(
        "SKILL.md",
        b"---\nname: force-json\nversion: 0.1.0\n---\n# hi\n" as &[u8],
    )]);
    let server = mock_pakx_skill_registry(id, version, &sha, tarball).await;
    seed_skill_manifest(&project, id, version);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    // Plant a prior export tree, then export with --force --json.
    let export_root = TempDir::new().unwrap();
    let dest = export_root.path().join("force-json");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("STALE.txt"), b"old").unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "export",
            id,
            "--output",
            dest.to_str().unwrap(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
            "--force",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8(out.stdout).unwrap();
    let body = stdout.trim_end_matches('\n');
    assert!(!body.contains('\n'), "json output must be single-line");
    let v: Value = serde_json::from_str(body).expect("json parses");
    assert_eq!(v["files"].as_u64().unwrap(), 1, "1 file in export");
    // Stale file must be gone (force wipes the prior tree).
    assert!(!dest.join("STALE.txt").exists());
    let snap = snapshot_tree(&dest);
    assert!(snap.contains_key("SKILL.md"));
}

/// `pakx export <id-not-in-lockfile>` — exit 1 with an error mentioning
/// the missing id. Pins the "not found in lockfile" branch — distinct
/// from the "no lockfile at all" error path the existing test covers.
#[tokio::test]
async fn export_errors_when_id_not_in_lockfile() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/installed";
    let version = "0.1.0";

    let (tarball, sha) = build_tarball(&[(
        "SKILL.md",
        b"---\nname: installed\nversion: 0.1.0\n---\n# hi\n" as &[u8],
    )]);
    let server = mock_pakx_skill_registry(id, version, &sha, tarball).await;
    seed_skill_manifest(&project, id, version);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    // Try to export a different id — lockfile has alice/installed but
    // we ask for ghost/missing.
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "export",
            "ghost/missing",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ghost/missing"));
}

// ===========================================================================
// `pakx audit` — additional gaps.
// ===========================================================================

const AUDIT_MANIFEST_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const AUDIT_ENTRY_INTEGRITY: &str = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

/// `pakx audit --json` on an entirely-deprecated multi-entry lockfile.
/// Every row must surface `status: "deprecated"` and the command must
/// exit 1. Pins that the deprecated-detection loop doesn't short-circuit
/// after the first hit.
#[tokio::test]
async fn audit_json_marks_every_deprecated_row() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    for (name, ver) in &[("alpha", "0.1.0"), ("beta", "0.1.0")] {
        Mock::given(method("GET"))
            .and(wm_path(format!("/api/v1/packages/team/{name}/{ver}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": format!("team/{name}"),
                "version": ver,
                "sha256": "0".repeat(64),
                "sizeBytes": 1024,
                "publishedAt": "2026-04-01T00:00:00Z",
                "deprecatedAt": "2026-04-12T08:00:00Z",
                "tarballUrl": "https://blob.example.com/sig",
            })))
            .mount(&pakx_registry)
            .await;
    }
    let lock = format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{AUDIT_MANIFEST_HASH}","entries":{{
  "skills/team/alpha@0.1.0":{{"name":"team/alpha","type":"skills","version":"0.1.0","resolvedFrom":"https://registry.pakx.dev/api/v1/packages/team/alpha/0.1.0","registry":"pakx","integrity":"{AUDIT_ENTRY_INTEGRITY}","agents":["claude-code"],"dependencies":[]}},
  "skills/team/beta@0.1.0":{{"name":"team/beta","type":"skills","version":"0.1.0","resolvedFrom":"https://registry.pakx.dev/api/v1/packages/team/beta/0.1.0","registry":"pakx","integrity":"{AUDIT_ENTRY_INTEGRITY}","agents":["claude-code"],"dependencies":[]}}
}}}}
"#
    );
    std::fs::write(project.path().join("agents.lock"), lock).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["audit", "--pakx-base-url", &pakx_registry.uri(), "--json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Value = serde_json::from_str(stdout.trim()).expect("json parses");
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(
            row["status"], "deprecated",
            "every row must be flagged; got {row:?}"
        );
    }
}

// ===========================================================================
// `pakx tree` — additional gaps.
// ===========================================================================

/// `pakx tree --json` on a 3-entry mixed-kind lockfile groups every
/// entry under the right `<kind>.<registry>` bucket. Existing tests
/// cover 1-skill + 1-mcp and the empty-group filter; this 3-entry test
/// pins multi-entry handling at the bucket level (no entries dropped,
/// no entries duplicated across buckets).
#[test]
fn tree_json_with_three_mixed_kind_entries_buckets_correctly() {
    let project = TempDir::new().unwrap();
    let lock = format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{AUDIT_MANIFEST_HASH}","entries":{{
  "skills/team/alpha@0.1.0":{{"name":"team/alpha","type":"skills","version":"0.1.0","resolvedFrom":"pakx:team/alpha","registry":"pakx","integrity":"{AUDIT_ENTRY_INTEGRITY}","agents":["claude-code"],"dependencies":[]}},
  "skills/team/beta@0.2.0":{{"name":"team/beta","type":"skills","version":"0.2.0","resolvedFrom":"pakx:team/beta","registry":"pakx","integrity":"{AUDIT_ENTRY_INTEGRITY}","agents":["claude-code"],"dependencies":[]}},
  "mcp/team/server@1.0.0":{{"name":"team/server","type":"mcp","version":"1.0.0","resolvedFrom":"official-mcp:team/server","registry":"official-mcp","integrity":"{AUDIT_ENTRY_INTEGRITY}","agents":["claude-code"],"dependencies":[]}}
}}}}
"#
    );
    std::fs::write(project.path().join("agents.lock"), lock).unwrap();
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["tree", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let skills_pakx = parsed["kinds"]["skills"]["pakx"].as_array().unwrap();
    assert_eq!(skills_pakx.len(), 2, "both skills bucketed under pakx");
    let mcp_official = parsed["kinds"]["mcp"]["official-mcp"].as_array().unwrap();
    assert_eq!(
        mcp_official.len(),
        1,
        "single mcp bucketed under official-mcp"
    );
    // No spurious cross-bucket bleeding — pakx bucket must not list
    // the mcp entry and vice versa.
    assert!(parsed["kinds"]["mcp"].get("pakx").is_none());
    assert!(parsed["kinds"]["skills"].get("official-mcp").is_none());
}

// ===========================================================================
// `pakx why` — additional gaps.
// ===========================================================================

/// `pakx why` with a version-suffixed id that doesn't match the locked
/// version must still resolve via the id-only branch (the locked
/// version is what the row reports, not what the user typed). Pins the
/// "trailing `@<ver>` is informational" semantics.
#[test]
fn why_with_mismatched_version_suffix_still_resolves_by_id() {
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("agents.yml"),
        "name: smoke\nversion: 0.0.0\ndependencies:\n  skills:\n    - team/pkg@0.1.0\n",
    )
    .unwrap();
    let lock = format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{AUDIT_MANIFEST_HASH}","entries":{{
  "skills/team/pkg@0.1.0":{{"name":"team/pkg","type":"skills","version":"0.1.0","resolvedFrom":"pakx:team/pkg","registry":"pakx","integrity":"{AUDIT_ENTRY_INTEGRITY}","agents":["claude-code"],"dependencies":[]}}
}}}}
"#
    );
    std::fs::write(project.path().join("agents.lock"), lock).unwrap();

    // User typed `@0.9.0` but lockfile has 0.1.0 — should still find
    // by id, report the *locked* version (0.1.0).
    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["why", "team/pkg@0.9.0", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&out).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1, "id-only match must succeed");
    assert_eq!(
        arr[0]["lockedVersion"], "0.1.0",
        "reported version is what the lockfile says, not what the user typed"
    );
}

// ===========================================================================
// `pakx manifest` — additional dot-path edge cases.
// ===========================================================================

const MANIFEST_FIXTURE: &str =
    "name: demo\nversion: 0.1.0\ndescription: demo\ndependencies:\n  skills:\n    - alice/bob@0.1.0\n    - carol/dave\n";

/// `pakx manifest get` on a malformed path (leading dot) must error at
/// parse time with the canonical "invalid path segment" diagnostic.
/// Path-parser error surface is well-covered at the unit level
/// (`manifest::path::parse_path`); this pins the CLI surface threads
/// that error through unchanged.
#[test]
fn manifest_get_rejects_leading_dot_path() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("agents.yml");
    std::fs::write(&manifest_path, MANIFEST_FIXTURE).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "get",
            ".description",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid path segment"));
}

/// `pakx manifest set --json` writing an object value at a deep path.
/// Pins that the JSON value layer (`set --json`) accepts an object,
/// not just primitives + arrays.
#[test]
fn manifest_set_json_accepts_object_value() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("agents.yml");
    std::fs::write(&manifest_path, MANIFEST_FIXTURE).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "set",
            "--json",
            "metadata",
            r#"{"repo":"git@example.com:demo.git","tags":["a","b"]}"#,
        ])
        .assert()
        .success();

    let body = std::fs::read_to_string(&manifest_path).unwrap();
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&body).unwrap();
    let repo = v
        .as_mapping()
        .and_then(|m| m.get(serde_yaml_ng::Value::String("metadata".into())))
        .and_then(|m| m.as_mapping())
        .and_then(|m| m.get(serde_yaml_ng::Value::String("repo".into())))
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(repo, "git@example.com:demo.git");
    let tags = v
        .as_mapping()
        .and_then(|m| m.get(serde_yaml_ng::Value::String("metadata".into())))
        .and_then(|m| m.as_mapping())
        .and_then(|m| m.get(serde_yaml_ng::Value::String("tags".into())))
        .and_then(|v| v.as_sequence())
        .unwrap();
    let labels: Vec<&str> = tags.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(labels, vec!["a", "b"]);
}

/// `pakx manifest set` on a sequence index past `len + 1` must surface
/// the `IndexOutOfBounds` error from the path module — the user typed
/// a gap, not a push.
#[test]
fn manifest_set_rejects_index_past_sequence_end() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("agents.yml");
    std::fs::write(&manifest_path, MANIFEST_FIXTURE).unwrap();
    let before = std::fs::read_to_string(&manifest_path).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "set",
            "dependencies.skills[99]",
            "eve/frank@0.1.0",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("out of bounds"));

    // File untouched — the atomic write never fired.
    let after = std::fs::read_to_string(&manifest_path).unwrap();
    assert_eq!(before, after);
}

/// `pakx manifest delete` on a sequence index past `len` is a soft
/// no-op (idempotent) per the contract — same exit-0 + stderr-warning
/// shape as `delete` on a missing key.
#[test]
fn manifest_delete_out_of_bounds_index_is_idempotent_warning() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("agents.yml");
    std::fs::write(&manifest_path, MANIFEST_FIXTURE).unwrap();
    let before = std::fs::read_to_string(&manifest_path).unwrap();

    Command::cargo_bin(BIN)
        .unwrap()
        .args([
            "manifest",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "delete",
            "dependencies.skills[99]",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("not present"));

    let after = std::fs::read_to_string(&manifest_path).unwrap();
    assert_eq!(before, after, "file untouched on the idempotent no-op");
}

// ===========================================================================
// `pakx install` — JSON shape gaps.
// ===========================================================================

/// `pakx install --json --no-cache` — both flags compose. Pins the
/// `--no-cache` flag doesn't accidentally suppress the JSON emit.
#[tokio::test]
async fn install_json_with_no_cache_still_emits_payload() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "alice/cache-json";
    let version = "0.1.0";
    let (tarball, sha) = build_tarball(&[(
        "SKILL.md",
        b"---\nname: cache-json\nversion: 0.1.0\n---\n# hi\n" as &[u8],
    )]);
    let server = mock_pakx_skill_registry(id, version, &sha, tarball).await;
    seed_skill_manifest(&project, id, version);

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--pakx-base-url",
            &server.uri(),
            "--no-smithery",
            "--mcp-base-url",
            &server.uri(),
            "--claude-home",
            claude_home.path().to_str().unwrap(),
            "--json",
            "--no-cache",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let body = stdout.trim_end_matches('\n');
    let v: Value = serde_json::from_str(body).expect("stdout is valid json");
    let arr = v.as_array().expect("top level is an array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["status"], "ok");
    assert_eq!(arr[0]["kind"], "skills");
}

// ===========================================================================
// `pakx outdated` — registry-source gap.
// ===========================================================================

/// `pakx outdated` on a lockfile whose only entry is from a source
/// without an outdated-check implementation (e.g. `glama`) must surface
/// every row as `skip`, exit 0. Pins the skip-on-unsupported-source
/// branch in `outdated::check_entry`.
#[tokio::test]
async fn outdated_marks_unsupported_source_as_skip() {
    let project = TempDir::new().unwrap();
    let lock = format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{AUDIT_MANIFEST_HASH}","entries":{{
  "mcp/team/server@1.0.0":{{"name":"team/server","type":"mcp","version":"1.0.0","resolvedFrom":"glama:team/server","registry":"glama","integrity":"{AUDIT_ENTRY_INTEGRITY}","agents":["claude-code"],"dependencies":[]}}
}}}}
"#
    );
    std::fs::write(project.path().join("agents.lock"), lock).unwrap();

    let out = Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated", "--json"])
        .assert()
        .success() // glama skip → no actionable rows → exit 0
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let body = stdout.trim_end_matches('\n');
    // Per the `pakx outdated --json` contract: up-to-date / skip rows
    // are excluded from the array (only `upgrade` / `drift` / `error`
    // rows ship). A lockfile of skips therefore lands as `[]`.
    assert_eq!(body, "[]");
}

// ===========================================================================
// Cache-root collision pin: per-call dirs must NOT share a name.
// ===========================================================================

/// Sanity check on the round-30 cache-root pattern. Run `pakx outdated`
/// against a wiremock-backed registry; each invocation builds a fresh
/// `pakx-outdated-cache-<pid>-<nanos>` directory under the system
/// temp dir. The pid changes per child process, the nanos changes
/// every call — so even on a hot CI runner two invocations cannot
/// collide. We assert by snapshotting the count of matching dirs
/// before + after the call; the after-count must be larger by at
/// least one. The wiremock returns up-to-date so the run is fast and
/// `build_clients` fires (it short-circuits on empty lockfiles).
#[tokio::test]
async fn outdated_creates_per_call_cache_root() {
    let project = TempDir::new().unwrap();
    let pakx_registry = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/v1/packages/team/cachepin"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "team/cachepin",
            "kind": "skills",
            "description": "cache root pin",
            "versions": [
                {
                    "version": "0.1.0",
                    "sha256": "0".repeat(64),
                    "sizeBytes": 1024,
                    "publishedAt": "2026-05-22T00:00:00Z",
                    "deprecatedAt": null,
                }
            ],
        })))
        .mount(&pakx_registry)
        .await;
    let lock = format!(
        r#"{{"lockfileVersion":1,"manifestHash":"{AUDIT_MANIFEST_HASH}","entries":{{
  "skills/team/cachepin@0.1.0":{{"name":"team/cachepin","type":"skills","version":"0.1.0","resolvedFrom":"pakx:team/cachepin","registry":"pakx","integrity":"{AUDIT_ENTRY_INTEGRITY}","agents":["claude-code"],"dependencies":[]}}
}}}}
"#
    );
    std::fs::write(project.path().join("agents.lock"), lock).unwrap();

    let tmp = std::env::temp_dir();
    let prefix = "pakx-outdated-cache-";
    let count_with_prefix = |dir: &std::path::Path| -> usize {
        std::fs::read_dir(dir).map_or(0, |it| {
            it.filter_map(Result::ok)
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|s| s.starts_with(prefix))
                })
                .count()
        })
    };
    let count_before = count_with_prefix(&tmp);

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["outdated", "--pakx-base-url", &pakx_registry.uri()])
        .assert()
        .success();

    let count_after = count_with_prefix(&tmp);

    assert!(
        count_after > count_before,
        "pakx outdated must create a fresh per-call cache dir; before={count_before}, after={count_after}"
    );
}
