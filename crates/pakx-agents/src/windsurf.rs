//! Windsurf adapter (detect-only at v0.1).
//!
//! Codeium's Windsurf IDE stores per-user config under
//! `~/.codeium/windsurf`. No native "skills" primitive yet, so
//! `install_skill` returns `Unsupported`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use pakx_core::manifest::PackageType;
use pakx_core::Skill;

use crate::adapter::{Adapter, Installed};
use crate::error::AdapterError;

#[derive(Debug, Clone)]
pub struct WindsurfAdapter {
    config_dir: PathBuf,
}

impl WindsurfAdapter {
    pub const ID: &'static str = "windsurf";

    #[must_use]
    pub fn new() -> Option<Self> {
        dirs::home_dir().map(|h| Self {
            config_dir: h.join(".codeium").join("windsurf"),
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
impl Adapter for WindsurfAdapter {
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
