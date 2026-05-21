//! Integration tests for `ClaudeCodeAdapter` against a temp config tree.

use pakx_agents::{Adapter, AdapterError, ClaudeCodeAdapter};
use pakx_core::install::compute_integrity;
use pakx_core::{Integrity, Skill, SkillFile};
use tempfile::TempDir;

fn make_skill(owner: &str, name: &str, version: &str, files: Vec<(&str, &[u8])>) -> Skill {
    let files: Vec<SkillFile> = files
        .into_iter()
        .map(|(p, c)| SkillFile {
            relative_path: p.to_string(),
            contents: c.to_vec(),
        })
        .collect();
    let integrity = compute_integrity(&files);
    Skill {
        owner: owner.into(),
        name: name.into(),
        version: version.into(),
        files,
        integrity,
    }
}

fn adapter_in(dir: &TempDir) -> ClaudeCodeAdapter {
    ClaudeCodeAdapter::with_config_dir(dir.path().join(".claude"))
}

#[tokio::test]
async fn detect_negative_when_config_dir_missing() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    assert!(!adapter.detect().await, "no ~/.claude → detect=false");
}

#[tokio::test]
async fn detect_positive_after_mkdir() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    std::fs::create_dir_all(adapter.config_dir()).unwrap();
    assert!(adapter.detect().await);
}

#[tokio::test]
async fn install_skill_writes_files() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let skill = make_skill(
        "anthropics",
        "pdf",
        "1.0.0",
        vec![
            ("SKILL.md", b"---\nname: pdf\nversion: 1.0.0\n---\n# PDF\n"),
            ("reference/usage.md", b"# Usage\n"),
        ],
    );

    let installed = adapter.install_skill(&skill).await.unwrap();
    assert_eq!(installed.id, "anthropics/pdf");
    assert_eq!(installed.version, "1.0.0");

    let root = adapter
        .config_dir()
        .join("skills")
        .join("anthropics")
        .join("pdf");
    assert!(root.join("SKILL.md").is_file());
    assert!(root.join("reference").join("usage.md").is_file());
}

#[tokio::test]
async fn install_skill_idempotent_second_run_returns_already_installed() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let skill = make_skill("a", "b", "1.0.0", vec![("SKILL.md", b"x")]);

    adapter.install_skill(&skill).await.unwrap();
    let err = adapter.install_skill(&skill).await.unwrap_err();
    assert!(matches!(err, AdapterError::AlreadyInstalled { .. }));
}

#[tokio::test]
async fn install_skill_replaces_drifted_install() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let v1 = make_skill("a", "b", "1.0.0", vec![("SKILL.md", b"v1")]);
    let v2 = make_skill("a", "b", "2.0.0", vec![("SKILL.md", b"v2")]);

    adapter.install_skill(&v1).await.unwrap();
    adapter.install_skill(&v2).await.unwrap();

    let on_disk = std::fs::read(adapter.config_dir().join("skills/a/b/SKILL.md")).unwrap();
    assert_eq!(on_disk, b"v2");
}

#[tokio::test]
async fn install_skill_integrity_mismatch_aborts() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let mut skill = make_skill("a", "b", "1.0.0", vec![("SKILL.md", b"hello")]);
    // Tamper with the declared integrity so it no longer matches the files.
    skill.integrity = Integrity::parse(format!("sha256-{}=", "Z".repeat(43))).unwrap();

    let err = adapter.install_skill(&skill).await.unwrap_err();
    assert!(matches!(err, AdapterError::IntegrityMismatch { .. }));
    assert!(
        !adapter.config_dir().join("skills/a/b").exists(),
        "nothing written"
    );
}

#[tokio::test]
async fn install_skill_rejects_path_traversal() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let skill = make_skill("a", "b", "1.0.0", vec![("../escape.md", b"x")]);
    let err = adapter.install_skill(&skill).await.unwrap_err();
    assert!(matches!(err, AdapterError::Invalid { .. }));
}

#[tokio::test]
async fn install_skill_rejects_empty_files() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let skill = make_skill("a", "b", "1.0.0", vec![]);
    let err = adapter.install_skill(&skill).await.unwrap_err();
    assert!(matches!(err, AdapterError::Invalid { .. }));
}

#[tokio::test]
async fn list_returns_empty_when_no_skills_root() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let listed = adapter.list().await.unwrap();
    assert!(listed.is_empty());
}

#[tokio::test]
async fn list_after_install_includes_skill() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let skill = make_skill(
        "anthropics",
        "pdf",
        "1.2.3",
        vec![("SKILL.md", b"name: pdf\nversion: 1.2.3\n")],
    );
    adapter.install_skill(&skill).await.unwrap();

    let listed = adapter.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "anthropics/pdf");
    assert_eq!(listed[0].version, "1.2.3");
}

#[tokio::test]
async fn list_is_sorted_by_id() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    adapter
        .install_skill(&make_skill("z", "y", "1.0.0", vec![("SKILL.md", b"a")]))
        .await
        .unwrap();
    adapter
        .install_skill(&make_skill("a", "b", "1.0.0", vec![("SKILL.md", b"b")]))
        .await
        .unwrap();
    let listed = adapter.list().await.unwrap();
    let ids: Vec<&str> = listed.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec!["a/b", "z/y"]);
}

#[tokio::test]
async fn uninstall_removes_skill_dir() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    adapter
        .install_skill(&make_skill("a", "b", "1.0.0", vec![("SKILL.md", b"x")]))
        .await
        .unwrap();
    adapter.uninstall("a/b").await.unwrap();
    assert!(!adapter.config_dir().join("skills/a/b").exists());
}

#[tokio::test]
async fn uninstall_returns_not_installed_when_missing() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let err = adapter.uninstall("ghost/skill").await.unwrap_err();
    assert!(matches!(err, AdapterError::NotInstalled { .. }));
}

#[tokio::test]
async fn uninstall_rejects_malformed_id() {
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let err = adapter.uninstall("bareid").await.unwrap_err();
    assert!(matches!(err, AdapterError::Invalid { .. }));
}

#[tokio::test]
async fn install_returns_unsupported_for_unimplemented_primitives() {
    use pakx_core::install::{Command, Hook, Prompt, Subagent};
    let temp = TempDir::new().unwrap();
    let adapter = adapter_in(&temp);
    let sa = Subagent { id: "x/y".into() };
    let pr = Prompt { id: "x/y".into() };
    let cm = Command { id: "x/y".into() };
    let hk = Hook { id: "x/y".into() };
    assert!(matches!(
        adapter.install_subagent(&sa).await.unwrap_err(),
        AdapterError::Unsupported { .. }
    ));
    assert!(matches!(
        adapter.install_prompt(&pr).await.unwrap_err(),
        AdapterError::Unsupported { .. }
    ));
    assert!(matches!(
        adapter.install_command(&cm).await.unwrap_err(),
        AdapterError::Unsupported { .. }
    ));
    assert!(matches!(
        adapter.install_hook(&hk).await.unwrap_err(),
        AdapterError::Unsupported { .. }
    ));
}
