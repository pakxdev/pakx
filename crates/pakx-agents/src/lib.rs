//! Per-agent adapters for installing pakx packages.
//!
//! Each supported agent (Claude Code, Cursor, Codex, Copilot, Windsurf, ...)
//! implements a uniform [`Adapter`] trait for detection, install, uninstall,
//! and list.

pub mod adapter;
pub mod claude_code;
pub mod error;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Re-exports the core crate version this adapter set targets.
pub const SUPPORTED_CORE: &str = pakx_core::VERSION;

pub use adapter::{Adapter, Installed};
pub use claude_code::ClaudeCodeAdapter;
pub use error::AdapterError;
