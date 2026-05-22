//! Crash-safe write helper used by `agents.lock`, `agents.yml`, and the
//! federated-registry response cache.
//!
//! The pattern is: write the body to `<path>.tmp` and `rename` it into
//! place. POSIX `rename(2)` is atomic within the same filesystem, and on
//! Windows `std::fs::rename` lowers to `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`
//! which is also atomic for files. Either way: a crash mid-write leaves
//! the destination either untouched or fully written, never half.
//!
//! Why bother. The previous `fs::write(path, body)` path could leave a
//! corrupt `agents.lock` on disk if the process died after `open` but
//! before the last byte hit. A corrupt lockfile fails the *next* `pakx
//! install` / `pakx test` hard rather than self-healing — exactly the
//! scenario the user least wants to debug.
//!
//! Permission bits are NOT set here. The `~/.pakx/credentials.json`
//! writer needs `0600` and handles that itself via `OpenOptions::mode`
//! at the `open` call (see `credentials::Credentials::write_to`) —
//! mode-at-open is the only atomic way to get sensitive bits onto disk.
//! For everything else (lockfile, manifest, cache) the default umask is
//! the right call: cache entries are public registry responses, the
//! lockfile is meant to be committed to source control, and manifests
//! are user-authored config.

use std::path::{Path, PathBuf};

/// Write `bytes` to `path` atomically.
///
/// Implementation: write to `<path>.tmp`, then `rename` into place. If
/// the rename fails the orphan `.tmp` is unlinked so we don't leak temp
/// files across crashes. The caller is responsible for ensuring the
/// parent directory exists.
///
/// # Errors
///
/// Returns the underlying `std::io::Error` from either the temp-file
/// write or the final rename. On rename failure the temp file is removed
/// best-effort before returning — the original error is surfaced
/// regardless, since the cleanup outcome is rarely actionable.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_path_for(path);

    // Write the body. On failure, leave the tmp file alone — `fs::write`
    // already removed any partial bytes on close, and the tmp path is
    // deterministic so the next successful run reuses it.
    std::fs::write(&tmp, bytes)?;

    // Rename into place. If the rename fails (cross-device, permission,
    // etc.), unlink the orphan tmp so we don't litter the filesystem
    // with stale `.tmp` files across failed runs. Ignore cleanup errors;
    // surfacing the original rename failure is more useful.
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Compute the temp path used by [`atomic_write`]. Splitting this out
/// lets callers reason about the rename target shape (and unit-test the
/// orphan-cleanup path) without going through the filesystem.
#[must_use]
pub fn tmp_path_for(path: &Path) -> PathBuf {
    // `path.with_extension("...tmp")` strips any existing extension,
    // which would collapse `agents.lock` → `agents.tmp` and collide
    // with `agents.yml`'s tmp. Appending `.tmp` to the OS string keeps
    // every original byte intact: `agents.lock` → `agents.lock.tmp`.
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_bytes_to_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn no_tmp_file_lingers_after_successful_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        atomic_write(&path, b"hello").unwrap();

        let tmp = tmp_path_for(&path);
        assert!(
            !tmp.exists(),
            "tmp file {} should not exist after successful rename",
            tmp.display(),
        );
    }

    #[test]
    fn overwrites_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
    }

    #[test]
    fn tmp_path_preserves_full_filename() {
        // `agents.lock` must become `agents.lock.tmp`, not `agents.tmp`
        // — collapsing extensions would let `agents.yml`'s tmp collide
        // with `agents.lock`'s tmp in the same directory.
        let p = Path::new("/some/dir/agents.lock");
        assert_eq!(tmp_path_for(p), Path::new("/some/dir/agents.lock.tmp"));
        let p = Path::new("/some/dir/agents.yml");
        assert_eq!(tmp_path_for(p), Path::new("/some/dir/agents.yml.tmp"));
    }

    #[test]
    fn tmp_path_handles_no_extension() {
        let p = Path::new("/some/dir/file");
        assert_eq!(tmp_path_for(p), Path::new("/some/dir/file.tmp"));
    }

    /// Unix-only: a read-only parent dir makes the temp-file create
    /// (and any subsequent rename) fail. Verify the orphan `.tmp` is
    /// cleaned up — or never created — after the failure so we don't
    /// leak temp files. Windows ACLs make this hard to set up the same
    /// way; skipping there keeps the test portable.
    ///
    /// The test silently passes through as a no-op when run as `root`
    /// (root bypasses unix DAC and would make the writes succeed). The
    /// `nix::geteuid` check would pull in another dep just for this
    /// path; we approximate by attempting the write and returning early
    /// if it unexpectedly succeeds — which only happens under root.
    #[cfg(unix)]
    #[test]
    fn cleans_up_tmp_on_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("ro");
        std::fs::create_dir(&subdir).unwrap();
        let path = subdir.join("data.bin");

        // Drop write perms on the parent dir → both the `fs::write` of
        // `<path>.tmp` and any subsequent `rename` fail with EACCES.
        let mut perms = std::fs::metadata(&subdir).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&subdir, perms).unwrap();

        let outcome = atomic_write(&path, b"new");

        // Restore perms before any assertion so a failing assertion
        // doesn't leak a non-deletable tempdir.
        let mut perms = std::fs::metadata(&subdir).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&subdir, perms).unwrap();

        // Running as root bypasses DAC; the write succeeds. Skip the
        // assertion in that case rather than failing CI in
        // root-containers.
        if outcome.is_ok() {
            return;
        }

        let tmp = tmp_path_for(&path);
        assert!(
            !tmp.exists(),
            "tmp file {} should not linger after failed write",
            tmp.display(),
        );
    }
}
