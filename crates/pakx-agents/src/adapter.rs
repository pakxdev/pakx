//! Per-agent adapter trait.
//!
//! Each agent that pakx supports (Claude Code, Cursor, Codex, Copilot,
//! Windsurf, ...) implements this trait. The trait is `dyn`-safe via
//! `async-trait`: the installer holds a `Vec<Box<dyn Adapter>>` and
//! dispatches at runtime against detected adapters.

use std::path::PathBuf;

use async_trait::async_trait;
use pakx_core::manifest::PackageType;
use pakx_core::{Command, Hook, McpServer, Prompt, Skill, Subagent};

use crate::error::AdapterError;

/// Metadata about an installed primitive, returned by [`Adapter::list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// `<owner>/<name>` for the installed package.
    pub id: String,
    pub kind: PackageType,
    pub version: String,
    /// On-disk location of the install (skill dir, mcp config entry, ...).
    pub path: PathBuf,
}

/// One agent adapter. Default `install_*` impls return [`AdapterError::Unsupported`]
/// so concrete adapters only override the primitives they actually handle.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Stable lowercase kebab-case id, e.g. `"claude-code"`.
    fn id(&self) -> &'static str;

    /// Filesystem root where this adapter stores its config.
    fn config_dir(&self) -> &std::path::Path;

    /// Returns true iff this agent is installed on the local machine.
    /// Default impl: `config_dir().is_dir()`. Adapters with more specific
    /// detection logic (registry keys, binary on PATH, etc.) override.
    async fn detect(&self) -> bool {
        tokio::fs::try_exists(self.config_dir())
            .await
            .unwrap_or(false)
    }

    // ---- install_* -------------------------------------------------------

    async fn install_skill(&self, _skill: &Skill) -> Result<Installed, AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.id(),
            primitive: PackageType::Skills,
        })
    }

    async fn install_mcp(&self, _mcp: &McpServer) -> Result<Installed, AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.id(),
            primitive: PackageType::Mcp,
        })
    }

    async fn install_subagent(&self, _sa: &Subagent) -> Result<Installed, AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.id(),
            primitive: PackageType::Subagents,
        })
    }

    async fn install_prompt(&self, _p: &Prompt) -> Result<Installed, AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.id(),
            primitive: PackageType::Prompts,
        })
    }

    async fn install_command(&self, _c: &Command) -> Result<Installed, AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.id(),
            primitive: PackageType::Commands,
        })
    }

    async fn install_hook(&self, _h: &Hook) -> Result<Installed, AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.id(),
            primitive: PackageType::Hooks,
        })
    }

    // ---- uninstall + list -----------------------------------------------

    /// Remove a previously installed package by its `<owner>/<name>` id.
    /// Returns [`AdapterError::NotInstalled`] if absent.
    async fn uninstall(&self, id: &str) -> Result<(), AdapterError>;

    /// List every primitive currently installed by this adapter, across
    /// all package types it supports.
    async fn list(&self) -> Result<Vec<Installed>, AdapterError>;
}
