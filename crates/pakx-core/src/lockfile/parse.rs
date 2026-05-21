//! Parse `agents.lock` source text into a validated [`Lockfile`].

use std::path::Path;

use crate::errors::LockfileError;

use super::schema::{is_valid_entry_key, Lockfile, LOCKFILE_VERSION};

/// Parse a JSON lockfile. If `path` is supplied, it is attached to any
/// returned error for diagnostic display.
pub fn parse_lockfile(source: &str, path: Option<&Path>) -> Result<Lockfile, LockfileError> {
    let lockfile: Lockfile =
        serde_json::from_str(source).map_err(|source| LockfileError::ParseJson {
            source,
            path: path.map(Path::to_path_buf),
        })?;

    if lockfile.lockfile_version != LOCKFILE_VERSION {
        return Err(LockfileError::Schema {
            message: format!(
                "unsupported lockfileVersion {} (this build understands {LOCKFILE_VERSION})",
                lockfile.lockfile_version
            ),
            path: path.map(Path::to_path_buf),
        });
    }

    for key in lockfile.entries.keys() {
        if !is_valid_entry_key(key) {
            return Err(LockfileError::Schema {
                message: format!("invalid entry key {key:?}: must match `<type>/<id>@<version>`"),
                path: path.map(Path::to_path_buf),
            });
        }
    }

    for dep_key in lockfile.entries.values().flat_map(|e| &e.dependencies) {
        if !is_valid_entry_key(dep_key) {
            return Err(LockfileError::Schema {
                message: format!(
                    "invalid transitive dependency key {dep_key:?}: must match `<type>/<id>@<version>`"
                ),
                path: path.map(Path::to_path_buf),
            });
        }
    }

    Ok(lockfile)
}
