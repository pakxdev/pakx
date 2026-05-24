//! Shape guards for untrusted strings that ride in URL path segments
//! against the pakx-registry backend.
//!
//! The registry's RFC 3986 minimal percent-encoder
//! (`urlencoding_minimal` in the registry client + `commands/info.rs`)
//! leaves `.` in the unreserved set per the spec — which means a string
//! of literally `..` produces a URL with a literal `..` segment that a
//! normalizing reverse proxy (CDN, ALB, nginx with `merge_slashes off`)
//! collapses upward, silently re-routing the call to the wrong
//! endpoint. The encoder is doing the right thing; we need a separate
//! shape guard on every input that lands inside a URL path segment
//! before encoding.
//!
//! Two guards live here:
//!
//! - [`validate_package_name`] — for `<name>` segments (and reused via
//!   the registry client's own copy of the same logic).
//! - [`validate_version`] — for `<version>` segments. Stricter than the
//!   name guard because semver versions have a well-defined character
//!   set (`[a-zA-Z0-9._+-]{1,64}` covers exact pins, build metadata,
//!   and pre-release tags).
//!
//! Both share the same error type so callers can route either through
//! a single `match` arm in the CLI's error rendering.

use std::fmt;

/// Shape-guard failure for a string destined for a URL path segment.
///
/// Carries the offending input + the reason so the CLI can surface
/// both — the input alone wouldn't tell the user *why* it was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// The string that failed the guard. Echoed in the rendered error
    /// so the user sees exactly which input was refused.
    pub input: String,
    /// Human-friendly explanation of the failure.
    pub reason: &'static str,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid {input:?}: {reason}",
            input = self.input,
            reason = self.reason
        )
    }
}

impl std::error::Error for ValidationError {}

/// Reject hostile package names before they reach the URL builder.
///
/// `urlencoding_minimal` follows RFC 3986 §2.3 and leaves `.` in the
/// unreserved set, so a name like `..` produces a URL segment with a
/// literal `..` — which most HTTP routers (and any normalizing
/// reverse-proxy in front of the registry) collapse, silently
/// re-routing the request to an unintended endpoint. The encoder is
/// doing the right thing for `.`; we need a shape guard on the input.
///
/// Rejection rules:
/// - empty
/// - exactly `.` or `..`
/// - starts with `.` (hidden-file convention; nothing legit needs it)
/// - contains the substring `..` anywhere
/// - contains `/`, `\`, or any ASCII control char
///
/// Used by the publish / unpublish path-segment builders; the registry
/// client's `pakx_backend` module wraps this guard in a backend-
/// specific error variant.
pub fn validate_package_name(name: &str) -> Result<(), ValidationError> {
    let reject = |reason: &'static str| ValidationError {
        input: name.to_owned(),
        reason,
    };
    if name.is_empty() {
        return Err(reject("name must not be empty"));
    }
    if name == "." || name == ".." {
        return Err(reject("name must not be `.` or `..`"));
    }
    if name.starts_with('.') {
        return Err(reject("name must not start with `.`"));
    }
    if name.contains("..") {
        return Err(reject("name must not contain `..`"));
    }
    for c in name.chars() {
        if c == '/' || c == '\\' {
            return Err(reject("name must not contain `/` or `\\`"));
        }
        if c.is_control() {
            return Err(reject("name must not contain control characters"));
        }
    }
    Ok(())
}

/// Maximum number of characters in a validated version segment.
///
/// The semver spec doesn't impose a cap (it allows arbitrarily long
/// pre-release and build metadata tags) but the URL path segment
/// realistically lives well under 64 chars — anything beyond is either
/// a typo or an injection probe and rejecting it loud is safer than
/// silently routing megabyte-long paths to the registry.
pub const MAX_VERSION_LEN: usize = 64;

/// Reject hostile version pins before they reach the URL builder.
///
/// Same threat model as [`validate_package_name`]: an unencoded `..`
/// segment normalises away under a CDN. The version's allowed
/// character set is well-defined (it's the union of what semver
/// accepts: alphanumerics, dot, dash, plus, underscore — see
/// <https://semver.org>) so we can apply a positive whitelist on top
/// of the `..`-traversal rejection that names get.
///
/// Rejection rules:
/// - empty
/// - longer than [`MAX_VERSION_LEN`] characters
/// - exactly `.` or `..`
/// - starts with `.` (the empty-segment-then-traversal trick)
/// - starts with `-` (would land in `clap`-style argument parsing on
///   any shell tooling that consumes the version downstream)
/// - contains the substring `..` anywhere
/// - any character outside `[A-Za-z0-9._+-]`
///
/// Notably permits `+` (semver build metadata, e.g. `1.0.0+build.7`),
/// `~` is **not** permitted (would let a `~user/...` traversal slip
/// through if anyone ever concatenated this segment into a path on the
/// CLI side).
pub fn validate_version(version: &str) -> Result<(), ValidationError> {
    let reject = |reason: &'static str| ValidationError {
        input: version.to_owned(),
        reason,
    };
    if version.is_empty() {
        return Err(reject("version must not be empty"));
    }
    if version.len() > MAX_VERSION_LEN {
        return Err(reject("version exceeds 64-character limit"));
    }
    if version == "." || version == ".." {
        return Err(reject("version must not be `.` or `..`"));
    }
    if version.starts_with('.') {
        return Err(reject("version must not start with `.`"));
    }
    if version.starts_with('-') {
        return Err(reject("version must not start with `-`"));
    }
    if version.contains("..") {
        return Err(reject("version must not contain `..`"));
    }
    for c in version.chars() {
        let ok = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-');
        if !ok {
            return Err(reject(
                "version must match [A-Za-z0-9._+-] (semver-friendly set)",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_accepts_plain() {
        assert!(validate_package_name("foo").is_ok());
        assert!(validate_package_name("foo-bar_baz.qux").is_ok());
    }

    #[test]
    fn name_rejects_double_dot() {
        assert_eq!(
            validate_package_name("..").unwrap_err().reason,
            "name must not be `.` or `..`"
        );
    }

    #[test]
    fn name_rejects_embedded_traversal() {
        assert!(validate_package_name("foo..bar").is_err());
        assert!(validate_package_name("foo/../bar").is_err());
    }

    #[test]
    fn name_rejects_leading_dot_and_slash() {
        assert!(validate_package_name(".hidden").is_err());
        assert!(validate_package_name("a/b").is_err());
        assert!(validate_package_name("a\\b").is_err());
    }

    #[test]
    fn version_accepts_exact_semver() {
        assert!(validate_version("0.1.0").is_ok());
        assert!(validate_version("1.0.0-rc.1").is_ok());
        assert!(validate_version("1.0.0+build.7").is_ok());
        assert!(validate_version("v1.0.0").is_ok());
        // Lowercased alphanumeric pre-release identifiers.
        assert!(validate_version("0.1.0-alpha.2").is_ok());
    }

    #[test]
    fn version_rejects_empty_and_double_dot() {
        assert_eq!(
            validate_version("").unwrap_err().reason,
            "version must not be empty"
        );
        assert_eq!(
            validate_version("..").unwrap_err().reason,
            "version must not be `.` or `..`"
        );
        assert!(validate_version("../etc").is_err());
        assert!(validate_version("1..0").is_err());
    }

    #[test]
    fn version_rejects_leading_dot_or_dash() {
        assert!(validate_version(".5").is_err());
        // A leading `-` would be picked up as a flag by any downstream
        // shell-arg tooling that pastes the version into a command.
        assert!(validate_version("-1.0.0").is_err());
    }

    #[test]
    fn version_rejects_disallowed_chars() {
        assert!(validate_version("1.0.0 ").is_err());
        assert!(validate_version("1.0.0/x").is_err());
        assert!(validate_version("1.0.0%2F").is_err());
        // `~` is deliberately NOT permitted — it's a path-tilde-expansion
        // marker in any shell that walks the version into a filesystem
        // path downstream. (`urlencoding_minimal` leaves it unencoded
        // per RFC 3986, so it would reach the wire as `~`.)
        assert!(validate_version("1.0.0~rc").is_err());
        // Single-byte control char.
        assert!(validate_version("1.0.0\n").is_err());
    }

    #[test]
    fn version_rejects_overlong_input() {
        let too_long: String = "1".repeat(MAX_VERSION_LEN + 1);
        assert_eq!(
            validate_version(&too_long).unwrap_err().reason,
            "version exceeds 64-character limit"
        );
        // Exactly at the cap is OK.
        let at_cap: String = "1".repeat(MAX_VERSION_LEN);
        assert!(validate_version(&at_cap).is_ok());
    }
}
