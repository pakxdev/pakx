//! Shared validator for registry base-URL overrides.
//!
//! Used by `pakx install` and `pakx test` (and any future surface that
//! accepts `--mcp-base-url` / `--smithery-base-url` / `--pakx-base-url`).
//! Both commands need the same userinfo-bypass guard — the original
//! `starts_with` + `split('/')` parser accepted
//! `http://localhost:8080@evil.com/` because the substring before the
//! path looked loopback-like even though the real host was `evil.com`.
//!
//! Pulling the validator into one module guarantees `install` and
//! `test` cannot drift apart: if a registry URL is good enough for
//! `pakx test`, it has to be good enough for `pakx install`, and vice
//! versa. Implementation uses `url::Url` so the host extraction is
//! actually authoritative — the previous string-split parser is the
//! exact thing the userinfo bypass exploited.

use anyhow::Result;

/// Allow `https://` everywhere; allow `http://` only when the host is
/// the loopback address (`localhost` / `127.0.0.1` / `[::1]`,
/// optionally with port). Any other plaintext URL is rejected — it
/// would silently exfiltrate manifest contents over the wire in CI.
///
/// URLs carrying a username or password are rejected outright. The
/// previous `starts_with` + `split('/')` parser was vulnerable to
/// `http://localhost:8080@evil.com/` — the segment before the path
/// looked loopback-like even though the actual host was `evil.com`.
/// Using `url::Url` puts the authoritative host extraction in the
/// hands of the URL crate, not our ad-hoc string-splitting.
///
/// **Both `install` and `test` must call this on every user-supplied
/// base URL before any HTTP request fires.** A previous regression
/// validated on `test` only, leaving `install` open to the exact
/// userinfo-smuggling bypass that `test` was hardened against.
pub fn validate_base_url(url_str: &str) -> Result<()> {
    let parsed = url::Url::parse(url_str).map_err(|e| {
        anyhow::anyhow!("refusing to use registry base URL {url_str:?}: not a valid URL ({e})")
    })?;

    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!(
            "refusing to use registry base URL {url_str:?}: URLs with embedded \
             credentials are rejected to prevent userinfo smuggling"
        );
    }

    let host = parsed.host_str().ok_or_else(|| {
        anyhow::anyhow!("refusing to use registry base URL {url_str:?}: no host component")
    })?;

    // `url::Url::host_str` returns IPv6 literals with surrounding
    // brackets on some versions and without on others — match both
    // forms so `http://[::1]:8080/` is accepted either way.
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") => Ok(()),
        _ => anyhow::bail!(
            "refusing to use registry base URL {url_str:?}: only `https://` or \
             `http://localhost` / `http://127.0.0.1` / `http://[::1]` are allowed"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::validate_base_url;

    #[test]
    fn accepts_https() {
        validate_base_url("https://registry.example.com").unwrap();
        validate_base_url("https://registry.example.com:443/").unwrap();
    }

    #[test]
    fn accepts_http_loopback() {
        validate_base_url("http://localhost").unwrap();
        validate_base_url("http://localhost:8080").unwrap();
        validate_base_url("http://127.0.0.1").unwrap();
        validate_base_url("http://127.0.0.1:8080/").unwrap();
        validate_base_url("http://[::1]:8080/").unwrap();
    }

    #[test]
    fn rejects_plaintext_http() {
        assert!(validate_base_url("http://evil.com").is_err());
        assert!(validate_base_url("http://registry.example.com").is_err());
    }

    #[test]
    fn rejects_userinfo_smuggle() {
        // The exact bypass that motivated the shared validator. Document
        // the rejection here so any future re-write cannot regress.
        assert!(validate_base_url("http://localhost:8080@evil.com/").is_err());
        assert!(validate_base_url("http://127.0.0.1@evil.com/").is_err());
        // Even with a real https scheme, embedded credentials are
        // rejected — they don't belong in a registry base URL.
        assert!(validate_base_url("https://user:pass@example.com/").is_err());
    }

    #[test]
    fn rejects_non_http_schemes() {
        assert!(validate_base_url("file:///etc/passwd").is_err());
        assert!(validate_base_url("ftp://example.com").is_err());
        assert!(validate_base_url("").is_err());
        // Garbage strings fail the URL parser entirely.
        assert!(validate_base_url("not a url").is_err());
    }
}
