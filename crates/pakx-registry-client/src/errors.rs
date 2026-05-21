//! Errors raised by the federated registry client.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistryError {
    /// HTTP layer failure: connection, DNS, TLS, non-2xx status.
    #[error("http error from {source_tag}: {source}")]
    Http {
        source_tag: &'static str,
        #[source]
        source: reqwest::Error,
    },

    /// Server returned a body we could not decode against the schema.
    #[error("decode error from {source_tag}: {message}")]
    Decode {
        source_tag: &'static str,
        message: String,
    },

    /// The requested package id does not exist in this source.
    #[error("package {id:?} not found in {source_tag}")]
    NotFound {
        source_tag: &'static str,
        id: String,
    },

    /// Local cache I/O failure.
    #[error("cache io error{path}: {source}", path = fmt_path(.path.as_ref()))]
    Cache {
        #[source]
        source: std::io::Error,
        path: Option<PathBuf>,
    },

    /// Structural validation failure (malformed source URL, etc.).
    #[error("invalid input for {source_tag}: {reason}")]
    Invalid {
        source_tag: &'static str,
        reason: String,
    },
}

fn fmt_path(p: Option<&PathBuf>) -> String {
    p.map_or_else(String::new, |path| format!(" at {}", path.display()))
}
