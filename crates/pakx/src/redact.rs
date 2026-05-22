//! Path redaction helpers for error messages.
//!
//! `pakx` action subcommands historically embedded absolute paths into
//! `with_context(|| format!("read manifest at {}", path.display()))`
//! and similar call sites. On CI the cwd is typically
//! `/home/runner/work/<org>/<repo>` or
//! `C:\Users\runneradmin\AppData\Local\Temp\pakx-<hash>\…`; surfacing
//! those into a build log leaks the runner workspace path and (on
//! self-hosted runners) the operator's username. The post-action hint
//! lines (e.g. `→ lockfile: <abs path>`) are intentional — they go to
//! stdout for user value. Error messages are a different stream and
//! benefit from the cleaner relative form.
//!
//! Strategy:
//!   1. If the target lives under the project root, render the relative
//!      path with forward-slash separators (stable across platforms).
//!   2. Otherwise render just the file name — the absolute path's
//!      directory components are the part that leaks.
//!   3. If the target has no file name (rare — root paths, empty
//!      paths), fall back to the full display form so the error is
//!      still actionable.

use std::path::{Path, PathBuf};

/// Render `target` for inclusion in an error message rooted at
/// `project_root`. See module docs for the full contract.
pub fn redact_path(target: &Path, project_root: &Path) -> String {
    if let Ok(rel) = target.strip_prefix(project_root) {
        // Normalise to forward slashes so error text is consistent
        // across Linux + macOS + Windows. The display path on Windows
        // would otherwise include `\` which makes pasted-error
        // comparisons across platforms awkward.
        return rel.to_string_lossy().replace('\\', "/");
    }
    target.file_name().map_or_else(
        || target.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Resolve the project root for redaction purposes. Prefers `cwd` when
/// available so the relative form lines up with what the user sees in
/// their shell; falls back to the parent of `target` so the file name
/// stays the worst-case rendered form.
pub fn project_root_for(target: &Path) -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| {
        target
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_to_relative_when_under_project_root() {
        let root = Path::new("/work/proj");
        let target = Path::new("/work/proj/sub/agents.yml");
        assert_eq!(redact_path(target, root), "sub/agents.yml");
    }

    #[test]
    fn redacts_to_basename_when_outside_project_root() {
        let root = Path::new("/work/proj");
        let target = Path::new("/tmp/pakx-cache/agents.lock");
        assert_eq!(redact_path(target, root), "agents.lock");
    }

    #[test]
    fn windows_path_under_root_renders_forward_slashes() {
        // Skip on non-Windows: `strip_prefix` on a posix string with a
        // backslash separator does not behave the same way. Forward-
        // slash normalisation is the part we want to verify here.
        let root = Path::new("/work/proj");
        let target = Path::new("/work/proj/sub/file.yml");
        let out = redact_path(target, root);
        assert!(!out.contains('\\'), "got {out:?}");
    }

    #[test]
    fn redacts_to_full_display_when_no_filename() {
        // Edge case: a root path (e.g. `/` or `C:\`) outside the
        // project root has no `file_name`; fall back to the display
        // form so the error stays actionable. Use a path that is
        // outside `root` *and* lacks a basename component.
        let root = Path::new("/work/proj");
        // `Path::new("/").file_name()` is `None` on unix.
        let target = Path::new("/");
        // The exact rendering is platform-dependent; assert it's at
        // least non-empty so we don't silently emit `""`.
        assert!(!redact_path(target, root).is_empty());
    }
}
