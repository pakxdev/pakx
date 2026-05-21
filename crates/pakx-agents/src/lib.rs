//! Per-agent adapters for installing pakx packages.
//!
//! Each supported agent (Claude Code, Cursor, Codex, Copilot, Windsurf, ...)
//! implements a uniform trait for detection, install, uninstall, and list.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Re-exports the core crate version this adapter set targets.
pub const SUPPORTED_CORE: &str = pakx_core::VERSION;
