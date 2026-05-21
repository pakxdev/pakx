//! Serialise a [`Manifest`] back to YAML with stable field order for
//! diff-friendly output.

use super::schema::Manifest;

/// Render a manifest to YAML.
///
/// Key order is fixed by the field order in [`Manifest`] (name → version →
/// agents → dependencies). Empty/`None` collections are skipped via
/// `skip_serializing_if`. Output ends with a single trailing newline.
pub fn write_manifest(manifest: &Manifest) -> String {
    let mut out = serde_yml::to_string(manifest).expect("Manifest serializes infallibly");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}
