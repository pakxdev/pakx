//! Parse `agents.yml` source text into a validated [`Manifest`].

use std::path::Path;

use crate::errors::ManifestError;

use super::schema::Manifest;

/// Parse a YAML manifest. If `path` is supplied, it is attached to any
/// returned error for diagnostic display.
pub fn parse_manifest(source: &str, path: Option<&Path>) -> Result<Manifest, ManifestError> {
    // First check the source actually decodes to a mapping; serde_yml
    // would otherwise produce a less specific schema error on inputs
    // that are valid YAML but the wrong shape (sequence, scalar).
    let value: serde_yml::Value =
        serde_yml::from_str(source).map_err(|source| ManifestError::ParseYaml {
            source,
            path: path.map(Path::to_path_buf),
        })?;

    if !value.is_mapping() {
        return Err(ManifestError::Schema {
            message: "top level must be a YAML mapping".into(),
            path: path.map(Path::to_path_buf),
        });
    }

    serde_yml::from_value::<Manifest>(value).map_err(|source| ManifestError::Schema {
        message: source.to_string(),
        path: path.map(Path::to_path_buf),
    })
}
