//! Filesystem helpers for `agents.yml`.

use std::path::Path;

use crate::errors::ManifestError;

use super::parse::parse_manifest;
use super::schema::Manifest;
use super::write::write_manifest;

/// Read + parse `agents.yml` from disk. Errors carry the path for
/// diagnostic output.
pub fn read_from(path: &Path) -> Result<Manifest, ManifestError> {
    let source = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        source,
        path: Some(path.to_path_buf()),
    })?;
    parse_manifest(&source, Some(path))
}

/// Write a manifest to disk via [`write_manifest`].
pub fn write_to(path: &Path, manifest: &Manifest) -> Result<(), ManifestError> {
    let body = write_manifest(manifest);
    std::fs::write(path, body).map_err(|source| ManifestError::Io {
        source,
        path: Some(path.to_path_buf()),
    })
}
