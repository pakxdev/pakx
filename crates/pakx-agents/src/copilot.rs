//! GitHub Copilot adapter (detect-only at v0.1).
//!
//! Copilot's per-user config dir is platform-specific:
//! `%APPDATA%\github-copilot` on Windows, `$XDG_CONFIG_HOME/github-copilot`
//! (defaulting to `~/.config/github-copilot`) on Linux/macOS. There is no
//! native "skills" primitive — custom instructions live in the project's
//! `.github/copilot-instructions.md` — so `install_skill` returns
//! `Unsupported`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use pakx_core::manifest::PackageType;
use pakx_core::Skill;

use crate::adapter::{Adapter, Installed};
use crate::error::AdapterError;

#[derive(Debug, Clone)]
pub struct CopilotAdapter {
    config_dir: PathBuf,
}

impl CopilotAdapter {
    pub const ID: &'static str = "copilot";

    /// Resolve the default per-user config dir for GitHub Copilot.
    /// Returns `None` if no usable directory can be located.
    #[must_use]
    pub fn new() -> Option<Self> {
        let base = if cfg!(windows) {
            dirs::data_dir()?
        } else {
            dirs::config_dir()?
        };
        Some(Self {
            config_dir: base.join("github-copilot"),
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
impl Adapter for CopilotAdapter {
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
