//! Filesystem helpers for `agents.lock`.

use std::path::Path;

use crate::atomic_write::atomic_write;
use crate::errors::LockfileError;

use super::parse::parse_lockfile;
use super::schema::Lockfile;
use super::write::write_lockfile;

/// Read + parse `agents.lock` from disk.
///
/// Returns `Ok(None)` when the file is absent so callers can distinguish
/// "no lockfile yet" from "broken lockfile". Other I/O errors are
/// surfaced via [`LockfileError::Io`] so the user sees an honest
/// "read/write agents.lock: <reason>" — not "schema validation
/// failed," which the previous code emitted for permission-denied and
/// disk-full.
pub fn read_from(path: &Path) -> Result<Option<Lockfile>, LockfileError> {
    match std::fs::read_to_string(path) {
        Ok(source) => parse_lockfile(&source, Some(path)).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(LockfileError::Io {
            source,
            path: Some(path.to_path_buf()),
        }),
    }
}

/// Write `agents.lock` to disk via the crash-safe `atomic_write` helper.
///
/// A crash mid-write leaves the prior `agents.lock` untouched — the
/// previous `std::fs::write` flow could leave a half-written body on
/// disk, which fails the next `pakx install` / `pakx test` hard rather
/// than self-healing.
pub fn write_to(path: &Path, lockfile: &Lockfile) -> Result<(), LockfileError> {
    let body = write_lockfile(lockfile);
    atomic_write(path, body.as_bytes()).map_err(|source| LockfileError::Io {
        source,
        path: Some(path.to_path_buf()),
    })
}
