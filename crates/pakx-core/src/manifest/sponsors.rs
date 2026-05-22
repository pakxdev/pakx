//! Sponsor-link validation for `SKILL.md` frontmatter.
//!
//! Source of truth: `pakx-registry/SPONSOR_LINKS_SPEC.md`. The CLI is the
//! first line of defence (rejects malformed entries at `pakx pack` time);
//! the registry re-validates server-side. The regexes here must stay in
//! sync with the registry's Zod schema — drift is the single highest risk
//! flagged in §6 of the spec.
//!
//! Validation rules per locked spec:
//!
//! | kind   | regex (full match)                                                              |
//! | ------ | ------------------------------------------------------------------------------- |
//! | github | `^https://github\.com/sponsors/[A-Za-z0-9](?:[A-Za-z0-9-]{0,38}[A-Za-z0-9])?$`  |
//! | polar  | `^https://polar\.sh/[A-Za-z0-9_-]+/?$`                                          |
//! | kofi   | `^https://ko-fi\.com/[A-Za-z0-9_-]+/?$`                                         |
//! | url    | parses as `url::Url`, scheme == `https`, host non-empty, length ≤ 256           |
//!
//! Max sponsor entries: 5. Empty list is fine.

use std::sync::LazyLock;

use regex::Regex;
use thiserror::Error;

use super::schema::{Sponsor, SponsorKind};

/// Maximum sponsor entries permitted per package, mirrored from the
/// cross-repo spec (`pakx-registry/SPONSOR_LINKS_SPEC.md` §1).
pub const MAX_SPONSORS: usize = 5;

/// Hard cap on the `url` kind length (chars). Aligned with the registry's
/// `z.string().url().max(256)` constraint.
const URL_KIND_MAX_LEN: usize = 256;

static GITHUB_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://github\.com/sponsors/[A-Za-z0-9](?:[A-Za-z0-9-]{0,38}[A-Za-z0-9])?$")
        .expect("static regex compiles")
});
static POLAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://polar\.sh/[A-Za-z0-9_-]+/?$").expect("static regex compiles")
});
static KOFI_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://ko-fi\.com/[A-Za-z0-9_-]+/?$").expect("static regex compiles")
});

/// Validation failures for the `sponsors:` block. The `index` field is the
/// zero-based position in the source array so error messages can point at
/// the offending entry (`sponsors[2].url: malformed ...`).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SponsorError {
    #[error("sponsors: too many entries ({count}); max {max}")]
    TooMany { count: usize, max: usize },
    #[error("sponsors[{index}].url: does not match the {kind} URL shape: {url:?}")]
    BadUrl {
        index: usize,
        kind: &'static str,
        url: String,
    },
    #[error("sponsors[{index}].url: malformed URL {url:?}: {reason}")]
    Malformed {
        index: usize,
        url: String,
        reason: &'static str,
    },
    #[error("sponsors[{index}].url: must use https scheme, got {scheme:?}")]
    NotHttps { index: usize, scheme: String },
    #[error("sponsors[{index}].url: exceeds {max}-char limit ({len} chars)")]
    TooLong {
        index: usize,
        len: usize,
        max: usize,
    },
}

/// Validate the `sponsors:` block at pack-time. Returns the first
/// violation; iterating to surface every error is intentionally not done
/// here — the CLI's UX is "fix one thing at a time".
pub fn validate_sponsors(sponsors: &[Sponsor]) -> Result<(), SponsorError> {
    if sponsors.len() > MAX_SPONSORS {
        return Err(SponsorError::TooMany {
            count: sponsors.len(),
            max: MAX_SPONSORS,
        });
    }
    for (index, s) in sponsors.iter().enumerate() {
        validate_one(index, s)?;
    }
    Ok(())
}

fn validate_one(index: usize, s: &Sponsor) -> Result<(), SponsorError> {
    match s.kind {
        SponsorKind::Github => check_regex(index, &s.url, &GITHUB_RE, "github"),
        SponsorKind::Polar => check_regex(index, &s.url, &POLAR_RE, "polar"),
        SponsorKind::Kofi => check_regex(index, &s.url, &KOFI_RE, "kofi"),
        SponsorKind::Url => check_url_escape_hatch(index, &s.url),
    }
}

fn check_regex(
    index: usize,
    url: &str,
    re: &Regex,
    kind: &'static str,
) -> Result<(), SponsorError> {
    if re.is_match(url) {
        Ok(())
    } else {
        Err(SponsorError::BadUrl {
            index,
            kind,
            url: url.to_owned(),
        })
    }
}

fn check_url_escape_hatch(index: usize, raw: &str) -> Result<(), SponsorError> {
    if raw.len() > URL_KIND_MAX_LEN {
        return Err(SponsorError::TooLong {
            index,
            len: raw.len(),
            max: URL_KIND_MAX_LEN,
        });
    }
    let parsed = url::Url::parse(raw).map_err(|_| SponsorError::Malformed {
        index,
        url: raw.to_owned(),
        reason: "could not parse as URL",
    })?;
    if parsed.scheme() != "https" {
        return Err(SponsorError::NotHttps {
            index,
            scheme: parsed.scheme().to_owned(),
        });
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(SponsorError::Malformed {
            index,
            url: raw.to_owned(),
            reason: "host must be non-empty",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(kind: SponsorKind, url: &str) -> Sponsor {
        Sponsor {
            kind,
            url: url.to_owned(),
        }
    }

    #[test]
    fn empty_list_is_ok() {
        validate_sponsors(&[]).unwrap();
    }

    #[test]
    fn accepts_one_valid_per_kind() {
        validate_sponsors(&[
            s(SponsorKind::Github, "https://github.com/sponsors/octocat"),
            s(SponsorKind::Polar, "https://polar.sh/octocat"),
            s(SponsorKind::Kofi, "https://ko-fi.com/octocat"),
            s(SponsorKind::Url, "https://opencollective.com/octocat"),
        ])
        .unwrap();
    }

    #[test]
    fn github_with_trailing_slash_rejected() {
        // The locked regex disallows trailing `/` on github sponsors
        // (the URL shape is canonical, no slash).
        let err = validate_sponsors(&[s(
            SponsorKind::Github,
            "https://github.com/sponsors/octocat/",
        )])
        .unwrap_err();
        assert!(matches!(err, SponsorError::BadUrl { kind: "github", .. }));
    }

    #[test]
    fn github_host_mismatch_rejected() {
        let err = validate_sponsors(&[s(
            SponsorKind::Github,
            "https://gitlab.com/sponsors/octocat",
        )])
        .unwrap_err();
        assert!(matches!(err, SponsorError::BadUrl { kind: "github", .. }));
    }

    #[test]
    fn github_http_scheme_rejected() {
        let err =
            validate_sponsors(&[s(SponsorKind::Github, "http://github.com/sponsors/octocat")])
                .unwrap_err();
        assert!(matches!(err, SponsorError::BadUrl { kind: "github", .. }));
    }

    #[test]
    fn github_trailing_hyphen_rejected() {
        // The locked regex requires the final char to be alphanumeric.
        let err = validate_sponsors(&[s(
            SponsorKind::Github,
            "https://github.com/sponsors/octocat-",
        )])
        .unwrap_err();
        assert!(matches!(err, SponsorError::BadUrl { kind: "github", .. }));
    }

    #[test]
    fn polar_accepts_with_trailing_slash() {
        validate_sponsors(&[s(SponsorKind::Polar, "https://polar.sh/octo_cat-1/")]).unwrap();
    }

    #[test]
    fn polar_rejects_wrong_host() {
        let err =
            validate_sponsors(&[s(SponsorKind::Polar, "https://polr.sh/octocat")]).unwrap_err();
        assert!(matches!(err, SponsorError::BadUrl { kind: "polar", .. }));
    }

    #[test]
    fn kofi_accepts_with_underscore_and_hyphen() {
        validate_sponsors(&[s(SponsorKind::Kofi, "https://ko-fi.com/oct_o-cat")]).unwrap();
    }

    #[test]
    fn kofi_rejects_extra_path_segment() {
        let err = validate_sponsors(&[s(SponsorKind::Kofi, "https://ko-fi.com/octocat/extra")])
            .unwrap_err();
        assert!(matches!(err, SponsorError::BadUrl { kind: "kofi", .. }));
    }

    #[test]
    fn url_escape_hatch_accepts_arbitrary_https_path() {
        validate_sponsors(&[s(
            SponsorKind::Url,
            "https://opencollective.com/foo/donate?amount=5",
        )])
        .unwrap();
    }

    #[test]
    fn url_kind_rejects_http_scheme() {
        let err =
            validate_sponsors(&[s(SponsorKind::Url, "http://example.com/donate")]).unwrap_err();
        assert!(matches!(err, SponsorError::NotHttps { .. }));
    }

    #[test]
    fn url_kind_rejects_malformed_input() {
        let err = validate_sponsors(&[s(SponsorKind::Url, "not a url")]).unwrap_err();
        assert!(matches!(err, SponsorError::Malformed { .. }));
    }

    #[test]
    fn url_kind_rejects_when_over_256_chars() {
        let long_path = "a".repeat(260);
        let raw = format!("https://example.com/{long_path}");
        let err = validate_sponsors(&[s(SponsorKind::Url, &raw)]).unwrap_err();
        assert!(matches!(err, SponsorError::TooLong { max: 256, .. }));
    }

    #[test]
    fn too_many_rejected_at_six() {
        let many: Vec<_> = (0..6)
            .map(|i| {
                s(
                    SponsorKind::Github,
                    &format!("https://github.com/sponsors/u{i}"),
                )
            })
            .collect();
        let err = validate_sponsors(&many).unwrap_err();
        assert!(matches!(err, SponsorError::TooMany { count: 6, max: 5 }));
    }

    #[test]
    fn exactly_five_is_ok() {
        let five: Vec<_> = (0..5)
            .map(|i| {
                s(
                    SponsorKind::Github,
                    &format!("https://github.com/sponsors/u{i}"),
                )
            })
            .collect();
        validate_sponsors(&five).unwrap();
    }

    #[test]
    fn kind_url_mismatch_rejected() {
        // `kind: github` with a non-github URL should fail.
        let err =
            validate_sponsors(&[s(SponsorKind::Github, "https://polar.sh/octocat")]).unwrap_err();
        assert!(matches!(err, SponsorError::BadUrl { kind: "github", .. }));
    }

    #[test]
    fn error_carries_offending_index() {
        let bad = vec![
            s(SponsorKind::Github, "https://github.com/sponsors/octocat"),
            s(SponsorKind::Github, "https://gitlab.com/sponsors/octocat"),
        ];
        let err = validate_sponsors(&bad).unwrap_err();
        match err {
            SponsorError::BadUrl { index, .. } => assert_eq!(index, 1),
            other => panic!("expected BadUrl at index 1, got {other:?}"),
        }
    }
}
