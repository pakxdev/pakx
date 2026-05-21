//! Filesystem helpers for `agents.lock`.

use std::path::Path;

use crate::errors::LockfileError;

use super::parse::parse_lockfile;
use super::schema::Lockfile;
use super::write::write_lockfile;

/// Read + parse `agents.lock` from disk.
///
/// Returns `Ok(None)` when the file is absent so callers can distinguish
/// "no lockfile yet" from "broken lockfile". Other I/O errors are surfaced
/// via [`LockfileError::Schema`] with the underlying message embedded.
pub fn read_from(path: &Path) -> Result<Option<Lockfile>, LockfileError> {
    match std::fs::read_to_string(path) {
        Ok(source) => parse_lockfile(&source, Some(path)).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LockfileError::Schema {
            message: format!("io error: {e}"),
            path: Some(path.to_path_buf()),
        }),
    }
}

pub fn write_to(path: &Path, lockfile: &Lockfile) -> Result<(), LockfileError> {
    let body = write_lockfile(lockfile);
    std::fs::write(path, body).map_err(|e| LockfileError::Schema {
        message: format!("io error: {e}"),
        path: Some(path.to_path_buf()),
    })
}
