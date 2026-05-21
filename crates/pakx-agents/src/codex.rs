//! Codex CLI adapter (detect-only at v0.1).
//!
//! `OpenAI`'s Codex CLI keeps per-user config under `~/.codex`. It has no
//! native "skills" primitive (the closest convention is `AGENTS.md` at
//! the project level), so `install_skill` returns `Unsupported`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use pakx_core::manifest::PackageType;
use pakx_core::Skill;

use crate::adapter::{Adapter, Installed};
use crate::error::AdapterError;

#[derive(Debug, Clone)]
pub struct CodexAdapter {
    config_dir: PathBuf,
}

impl CodexAdapter {
    pub const ID: &'static str = "codex";

    #[must_use]
    pub fn new() -> Option<Self> {
        dirs::home_dir().map(|h| Self {
            config_dir: h.join(".codex"),
        })
    }

    #[must_use]
    pub fn with_config_dir(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
        }
    }
}

#[async_trait]
impl Adapter for CodexAdapter {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    async fn install_skill(&self, _skill: &Skill) -> Result<Installed, AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: Self::ID,
            primitive: PackageType::Skills,
        })
    }
}
