//! Cursor adapter (detect-only at v0.1).
//!
//! Cursor stores per-user config under `~/.cursor` (rules live in
//! `.cursorrules` per-project, MCP servers under `~/.cursor/mcp.json`).
//! There is no native "skills" primitive in Cursor today, so
//! `install_skill` returns [`AdapterError::Unsupported`] until a future
//! step decides whether to project skills onto `.cursor/rules/*.md`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use pakx_core::manifest::PackageType;
use pakx_core::Skill;

use crate::adapter::{Adapter, Installed};
use crate::error::AdapterError;

#[derive(Debug, Clone)]
pub struct CursorAdapter {
    config_dir: PathBuf,
}

impl CursorAdapter {
    pub const ID: &'static str = "cursor";

    #[must_use]
    pub fn new() -> Option<Self> {
        dirs::home_dir().map(|h| Self {
            config_dir: h.join(".cursor"),
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
impl Adapter for CursorAdapter {
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
