//! Map a [`RegistryError`] to a short, actionable, user-facing hint.
//!
//! Several read-only subcommands (`pakx audit`, `pakx outdated`) surface
//! per-entry registry errors in a table + on stderr. Rendering the raw
//! `RegistryError::Display` there leaks transport jargon ("http error
//! from pakx: error sending request for url …") that an adopter can't
//! act on. This helper collapses each variant onto a single sentence
//! that tells the user *what to do*: retry (transient / offline), check
//! the id (not found), or that the registry is degraded.
//!
//! The raw error is still available behind `tracing` (the call sites log
//! it at debug) so the lossless form survives for diagnosis.

use pakx_registry_client::RegistryError;

/// Collapse a [`RegistryError`] onto a one-line actionable hint.
///
/// - `NotFound` → "not found" with the id (the user likely typo'd it or
///   it was unpublished).
/// - `Http` → transient / offline — almost always a 5xx, a rate-limit,
///   or no network; the right move is to retry shortly.
/// - `Decode` → the registry answered with a body we couldn't parse —
///   a server-side problem, retry / report.
/// - `Cache` → a *local* I/O problem (disk full, permissions); distinct
///   from a registry fault.
/// - `Invalid` → a structural problem with the input (malformed id /
///   URL) — surface the reason so the user can correct it.
#[must_use]
pub fn registry_error_hint(e: &RegistryError) -> String {
    match e {
        RegistryError::NotFound { id, .. } => {
            format!("not found: {id} (check the id, or it may have been unpublished)")
        }
        RegistryError::Http { .. } => {
            "registry unreachable — offline, rate-limited, or a transient 5xx; retry shortly"
                .to_owned()
        }
        RegistryError::Decode { .. } => {
            "registry returned an unreadable response — likely a transient server-side error; retry shortly"
                .to_owned()
        }
        RegistryError::Cache { .. } => {
            "local cache I/O error — check disk space / permissions, or pass --no-cache".to_owned()
        }
        RegistryError::Invalid { reason, .. } => format!("invalid request: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::registry_error_hint;
    use pakx_registry_client::RegistryError;

    #[test]
    fn not_found_names_the_id_and_suggests_a_cause() {
        let e = RegistryError::NotFound {
            source_tag: "pakx",
            id: "alice/widget".to_owned(),
        };
        let hint = registry_error_hint(&e);
        assert!(hint.contains("alice/widget"));
        assert!(hint.contains("not found"));
    }

    #[test]
    fn decode_maps_to_transient_server_hint() {
        let e = RegistryError::Decode {
            source_tag: "pakx",
            message: "missing field `version`".to_owned(),
        };
        let hint = registry_error_hint(&e);
        assert!(hint.contains("retry"));
        // The raw decode message must NOT leak into the hint.
        assert!(!hint.contains("missing field"));
    }

    #[test]
    fn invalid_surfaces_the_reason() {
        let e = RegistryError::Invalid {
            source_tag: "pakx",
            reason: "owner segment empty".to_owned(),
        };
        let hint = registry_error_hint(&e);
        assert!(hint.contains("owner segment empty"));
    }
}
