//! Detect + Unsupported coverage for the four non-Claude-Code adapters.
//!
//! These adapters ship in v0.1 so the installer can detect every agent
//! the user has installed; concrete `install_*` impls land in later steps.

use pakx_agents::{
    Adapter, AdapterError, CodexAdapter, CopilotAdapter, CursorAdapter, WindsurfAdapter,
};
use pakx_core::install::compute_integrity;
use pakx_core::{Skill, SkillFile};
use tempfile::TempDir;

fn sample_skill() -> Skill {
    let files = vec![SkillFile {
        relative_path: "SKILL.md".into(),
        contents: b"name: x\nversion: 1.0.0\n".to_vec(),
    }];
    let integrity = compute_integrity(&files);
    Skill {
        owner: "a".into(),
        name: "b".into(),
        version: "1.0.0".into(),
        files,
        integrity,
    }
}

macro_rules! adapter_contract_tests {
    ($mod:ident, $adapter:ty, $expected_id:expr) => {
        mod $mod {
            use super::*;

            #[tokio::test]
            async fn id_is_canonical() {
                let temp = TempDir::new().unwrap();
                let adapter = <$adapter>::with_config_dir(temp.path());
                assert_eq!(adapter.id(), $expected_id);
            }

            #[tokio::test]
            async fn detect_negative_when_config_dir_missing() {
                let temp = TempDir::new().unwrap();
                // Point at a child that does NOT exist.
                let adapter = <$adapter>::with_config_dir(temp.path().join("missing"));
                assert!(!adapter.detect().await);
            }

            #[tokio::test]
            async fn detect_positive_after_mkdir() {
                let temp = TempDir::new().unwrap();
                let dir = temp.path().join("cfg");
                std::fs::create_dir_all(&dir).unwrap();
                let adapter = <$adapter>::with_config_dir(&dir);
                assert!(adapter.detect().await);
            }

            #[tokio::test]
            async fn install_skill_returns_unsupported() {
                let temp = TempDir::new().unwrap();
                let adapter = <$adapter>::with_config_dir(temp.path());
                let err = adapter.install_skill(&sample_skill()).await.unwrap_err();
                match err {
                    AdapterError::Unsupported { adapter: id, .. } => {
                        assert_eq!(id, $expected_id);
                    }
                    other => panic!("expected Unsupported, got {other:?}"),
                }
            }

            #[tokio::test]
            async fn list_is_empty_by_default() {
                let temp = TempDir::new().unwrap();
                let adapter = <$adapter>::with_config_dir(temp.path());
                let listed = adapter.list().await.unwrap();
                assert!(listed.is_empty());
            }

            #[tokio::test]
            async fn uninstall_returns_not_installed_by_default() {
                let temp = TempDir::new().unwrap();
                let adapter = <$adapter>::with_config_dir(temp.path());
                let err = adapter.uninstall("a/b").await.unwrap_err();
                assert!(matches!(err, AdapterError::NotInstalled { .. }));
            }
        }
    };
}

adapter_contract_tests!(cursor_tests, CursorAdapter, "cursor");
adapter_contract_tests!(codex_tests, CodexAdapter, "codex");
adapter_contract_tests!(copilot_tests, CopilotAdapter, "copilot");
adapter_contract_tests!(windsurf_tests, WindsurfAdapter, "windsurf");
