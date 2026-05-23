//! Real-user round-trip tests: `pakx add` → `pakx install` for each
//! of the six package kinds.
//!
//! These cover the exact gap the 2026-05-23 bug fell through: the user
//! ran `pakx add arwenizEr/hello-world` (NO `-t` flag), saw it land in
//! `mcp:` by default, then ran `pakx install` and got a stack of HTTP
//! errors. No existing test ran the full add-then-install pair in one
//! flow — `tests/add.rs` only exercises the manifest mutation,
//! `tests/skills_e2e.rs` / `tests/bundle_e2e.rs` hand-write the manifest
//! and never go through `pakx add`.
//!
//! Each test here:
//!   1. mounts a `wiremock` pakx-registry serving the appropriate
//!      kind-specific tarball,
//!   2. drives `pakx add <kind> <id>` (two-positional form so the test
//!      is hermetic against the kind-inference heuristic — see the
//!      sibling `*_routes_to_correct_kind_without_type_flag` regression
//!      tests for the inference path),
//!   3. drives `pakx install` against the same mock,
//!   4. asserts the file landed under `<claude_home>/<subdir>/<owner>-<name>/`
//!      and the lockfile entry carries the right `type` discriminator.
//!
//! The `mcp` round-trip uses the official-mcp wire shape instead of a
//! tarball — MCP servers don't ship as tarballs, they get translated
//! into `.mcp.json` entries.

use assert_cmd::Command;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path as wm_path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = "pakx";

// ---------------------------------------------------------------------------
// Tarball + mock helpers (duplicated from bundle_e2e.rs because Cargo's
// integration-test layout treats each tests/*.rs as its own crate; a
// shared `mod common` is more friction than the duplication here).
// ---------------------------------------------------------------------------

fn build_tarball(entries: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let mut buf = Vec::new();
    {
        let mut encoder = GzEncoder::new(&mut buf, Compression::default());
        let mut tar_builder = tar::Builder::new(&mut encoder);
        for (p, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_path(p).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar_builder.append(&header, *contents).unwrap();
        }
        tar_builder.finish().unwrap();
    }
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

/// Mount the package detail + per-version + blob endpoints for one
/// pakx-registry package. Returns the mock server so the caller can
/// pass `--pakx-base-url` and `--mcp-base-url` (we point both at the
/// same wiremock; the MCP endpoints aren't mounted so MCP resolution
/// for unrelated deps simply 404s — fine in a single-dep test).
async fn mock_pakx_for_kind(
    id: &str,
    version: &str,
    sha256_hex: &str,
    tarball_bytes: Vec<u8>,
) -> MockServer {
    let server = MockServer::start().await;
    let blob_path = format!("/blob/{id}/{version}");
    let signed_url = format!("{}{}?download=1&sig=ABC", server.uri(), blob_path);

    let (owner, name) = id.split_once('/').expect("id has /");

    let detail_body = json!({
        "id": id,
        "kind": "skills",
        "description": "round-trip fixture",
        "latestVersion": version,
        "versions": [
            { "version": version, "sha256": sha256_hex, "sizeBytes": tarball_bytes.len() }
        ]
    });
    Mock::given(method("GET"))
        .and(wm_path(format!("/api/v1/packages/{owner}/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(detail_body))
        .mount(&server)
        .await;

    let version_body = json!({
        "id": id,
        "version": version,
        "sha256": sha256_hex,
        "sizeBytes": tarball_bytes.len(),
        "publishedAt": "2026-05-22T00:00:00Z",
        "deprecatedAt": null,
        "tarballUrl": signed_url,
    });
    Mock::given(method("GET"))
        .and(wm_path(format!(
            "/api/v1/packages/{owner}/{name}/{version}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(version_body))
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

/// Drive the add-then-install round-trip for one bundle kind.
async fn assert_add_then_install(yaml_section: &str, on_disk_subdir: &str) {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let owner = "alice";
    let name = format!("{yaml_section}-roundtrip");
    let id = format!("{owner}/{name}");
    let version = "0.1.0";

    let (tarball, sha) = build_tarball(&[(
        "SKILL.md",
        format!("---\nname: {name}\nversion: {version}\n---\n# hi\n").as_bytes(),
    )]);
    let server = mock_pakx_for_kind(&id, version, &sha, tarball).await;

    // 1) `pakx add <kind> <id>` (two-positional form; --no-validate
    // skips the official-MCP probe because we're not resolving MCP).
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", yaml_section, &id, "--no-validate"])
        .assert()
        .success();

    // 2) Sanity: the manifest got the dep under the right section.
    let manifest_body = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    let manifest = pakx_core::parse_manifest(&manifest_body, None).unwrap();
    let section_populated = match yaml_section {
        "skills" => manifest.dependencies.skills.is_some(),
        "subagents" => manifest.dependencies.subagents.is_some(),
        "prompts" => manifest.dependencies.prompts.is_some(),
        "commands" => manifest.dependencies.commands.is_some(),
        "hooks" => manifest.dependencies.hooks.is_some(),
        other => panic!("unexpected kind {other}"),
    };
    assert!(
        section_populated,
        "section {yaml_section} should be populated after add\n{manifest_body}"
    );

    // 3) `pakx install` against the same registry mock.
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

    // 4) Tarball must land under the kind-specific subdir.
    let leaf = format!("{owner}-{name}");
    let extracted = claude_home.path().join(on_disk_subdir).join(&leaf);
    assert!(
        extracted.join("SKILL.md").is_file(),
        "{yaml_section}: SKILL.md should extract to {on_disk_subdir}/{leaf}/"
    );

    // 5) Lockfile records the right kind + canonical (unsigned) URL.
    let lock_body = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let lock: Value = serde_json::from_str(&lock_body).unwrap();
    let key = format!("{yaml_section}/{id}@{version}");
    let entry = &lock["entries"][&key];
    assert_eq!(entry["type"], yaml_section, "{yaml_section}: type");
    assert_eq!(entry["registry"], "pakx", "{yaml_section}: registry");
    let resolved_from = entry["resolvedFrom"].as_str().unwrap();
    assert!(
        !resolved_from.contains('?'),
        "{yaml_section}: signed query must be stripped: {resolved_from}"
    );
}

#[tokio::test]
async fn add_then_install_skills_roundtrip() {
    assert_add_then_install("skills", "skills").await;
}

#[tokio::test]
async fn add_then_install_subagents_roundtrip() {
    // YAML key `subagents:` maps to Claude Code's `agents/` subdir.
    assert_add_then_install("subagents", "agents").await;
}

#[tokio::test]
async fn add_then_install_prompts_roundtrip() {
    assert_add_then_install("prompts", "prompts").await;
}

#[tokio::test]
async fn add_then_install_commands_roundtrip() {
    assert_add_then_install("commands", "commands").await;
}

#[tokio::test]
async fn add_then_install_hooks_roundtrip() {
    assert_add_then_install("hooks", "hooks").await;
}

// ---------------------------------------------------------------------------
// MCP round-trip — the kind that doesn't ship as a tarball but as a
// `.mcp.json` entry. This is the kind the 2026-05-23 user-reported bug
// hit by default.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn add_then_install_mcp_roundtrip() {
    let project = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let id = "io.github.acme/cool";
    let version = "1.2.3";

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v0/servers/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": id,
            "description": "round-trip mcp fixture",
            "version_detail": { "version": version },
            "packages": [
                {
                    "registry_name": "npm",
                    "name": "@acme/mcp",
                    "version": version,
                    "package_arguments": [],
                    "environment_variables": []
                }
            ]
        })))
        .mount(&server)
        .await;

    // `pakx add mcp <id>` — two-positional form, explicit kind. We
    // route through the same server for the add-time validation
    // (would otherwise call the production MCP registry).
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", "mcp", id, "--mcp-base-url", &server.uri()])
        .assert()
        .success();

    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args([
            "install",
            "--mcp-base-url",
            &server.uri(),
            "--no-smithery",
            "--no-pakx-registry",
            "--claude-home",
            claude_home.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    // `.mcp.json` must be written into the project root. MCP installs
    // never write a per-package tree under `<claude_home>/mcp/` — the
    // merge target is the project's `.mcp.json`.
    let mcp_body = std::fs::read_to_string(project.path().join(".mcp.json")).unwrap();
    let mcp: Value = serde_json::from_str(&mcp_body).unwrap();
    assert_eq!(mcp["mcpServers"]["cool"]["command"], "npx");

    // Lockfile carries `type: mcp` + the official-mcp source tag.
    let lock_body = std::fs::read_to_string(project.path().join("agents.lock")).unwrap();
    let lock: Value = serde_json::from_str(&lock_body).unwrap();
    let key = format!("mcp/{id}@{version}");
    let entry = &lock["entries"][&key];
    assert_eq!(entry["type"], "mcp");
    assert_eq!(entry["registry"], "official-mcp");
}

// ---------------------------------------------------------------------------
// Inference round-trip: `<owner>/skills/<name>` shape must route to
// skills WITHOUT an explicit `-t` flag — pre-existing behaviour the
// dual-positional form leaves untouched.
// ---------------------------------------------------------------------------

#[test]
fn add_with_slash_skills_shape_routes_to_skills_section_no_type_flag() {
    // `<owner>/skills/<name>` triggers the `infer_kind` heuristic
    // path: the id contains `/skills/`, so `pakx add` defaults to
    // skills with no `-t` flag at all. This is the single inference
    // path that works on `main` today; broader probe-based inference
    // for plain `<owner>/<name>` shapes is in flight on a parallel
    // branch. We only assert the manifest mutation (not the install
    // round-trip) because the install path needs `<owner>/<name>`
    // (one slash) and the `/skills/` form trips that asymmetry —
    // covered separately by the federated_source_matrix tests.
    let project = TempDir::new().unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .current_dir(project.path())
        .args(["add", "anthropics/skills/pdf", "--no-validate"])
        .assert()
        .success();

    let manifest_body = std::fs::read_to_string(project.path().join("agents.yml")).unwrap();
    let manifest = pakx_core::parse_manifest(&manifest_body, None).unwrap();
    assert!(
        manifest.dependencies.skills.is_some(),
        "infer_kind must route `/skills/` shape to skills section, got:\n{manifest_body}"
    );
    assert!(
        manifest.dependencies.mcp.is_none(),
        "infer_kind must NOT default to mcp on `/skills/` shape, got:\n{manifest_body}"
    );
}
