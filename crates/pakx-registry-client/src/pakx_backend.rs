//! Typed client for the pakx-registry backend (Phase B).
//!
//! Routes consumed (mirrors `pakxdev/pakx-registry/app/api/v1/*`):
//!
//!   GET  /api/v1/whoami                                  (Bearer)
//!   POST /api/v1/packages                                (Bearer)
//!   PUT  /api/v1/packages/<owner>/<name>/<version>       (Bearer)
//!   DELETE /api/v1/packages/<owner>/<name>/<version>     (Bearer)
//!
//! Not a [`Source`] (`SmitherySource` / `OfficialMcpSource`) — this is
//! the publish-side client. The federated search side does not yet
//! query pakx-registry; that lands when we wire `pakx search` to
//! aggregate public packages alongside MCP/Smithery.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use pakx_core::{
    http_client_with_timeout, validate_package_name as core_validate_package_name,
    validate_version as core_validate_version, Sponsor, ValidationError, UPLOAD_REQUEST_TIMEOUT,
};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unauthorized — your pakx_v1_ token is missing, expired, or revoked")]
    Unauthorized,
    #[error("forbidden — you do not own this package")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("conflict: {message}")]
    Conflict { message: String },
    #[error("invalid package name {name:?}: {reason}")]
    InvalidName { name: String, reason: &'static str },
    #[error("invalid version {version:?}: {reason}")]
    InvalidVersion {
        version: String,
        reason: &'static str,
    },
    #[error("registry error ({status}): {body}")]
    Other { status: u16, body: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Whoami {
    pub id: String,
    pub login: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatePackageRequest<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    /// Sponsor links emitted on publish. The registry omits this field
    /// from the upsert payload entirely when the slice is empty
    /// (`Option::None`) — the registry treats *absent* as "no change",
    /// while `[]` would explicitly clear the sponsor list. Keep them
    /// distinct so a `pakx publish` of a manifest without `sponsors:`
    /// never wipes existing sponsors on a republish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsors: Option<&'a [Sponsor]>,
    /// Long-form README markdown captured from the bundle's
    /// `README.md` at pack time. Omitted from the JSON body entirely
    /// when `None` so the registry's omit-vs-explicit semantics fire:
    /// a bundle without a README on republish never clears a
    /// previously-stored README. The CLI never sends `null` here —
    /// clearing a stored README is a PATCH operation, not a publish
    /// one. Capped at 256 KiB at the registry; pack-time truncation
    /// in `pack::load_readme` keeps the wire payload under that limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readme: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePackageResponse {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub kind: String,
    pub created: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UploadVersionResponse {
    pub id: String,
    pub version: String,
    pub sha256: String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(rename = "tarballUrl")]
    pub tarball_url: String,
}

#[derive(Debug, Clone)]
pub struct PakxBackend {
    http: Client,
    base_url: String,
}

impl PakxBackend {
    /// Construct a backend client using the project-wide upload-friendly
    /// request timeout (`UPLOAD_REQUEST_TIMEOUT`, 5 minutes) and the
    /// default 15s connect timeout. The 5-minute request budget covers
    /// `pakx publish`'s tarball PUT; small calls (`whoami`, `create
    /// package`) still fail fast on connect via the same client because
    /// the connect timeout is independent of the request timeout.
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        Self::with_client(http_client_with_timeout(UPLOAD_REQUEST_TIMEOUT), base_url)
    }

    #[must_use]
    pub fn with_client(http: Client, base_url: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    pub async fn whoami(&self, token: &str) -> Result<Whoami, BackendError> {
        let res = self
            .http
            .get(format!("{}/api/v1/whoami", self.base_url))
            .bearer_auth(token)
            .send()
            .await?;
        let status = res.status();
        if status == StatusCode::OK {
            return Ok(res.json::<Whoami>().await?);
        }
        if status == StatusCode::UNAUTHORIZED {
            return Err(BackendError::Unauthorized);
        }
        Err(BackendError::Other {
            status: status.as_u16(),
            body: res.text().await.unwrap_or_default(),
        })
    }

    pub async fn create_package(
        &self,
        token: &str,
        req: &CreatePackageRequest<'_>,
    ) -> Result<CreatePackageResponse, BackendError> {
        let res = self
            .http
            .post(format!("{}/api/v1/packages", self.base_url))
            .bearer_auth(token)
            .json(req)
            .send()
            .await?;
        let status = res.status();
        match status {
            StatusCode::OK | StatusCode::CREATED => Ok(res.json::<CreatePackageResponse>().await?),
            StatusCode::UNAUTHORIZED => Err(BackendError::Unauthorized),
            StatusCode::FORBIDDEN => Err(BackendError::Forbidden),
            _ => Err(BackendError::Other {
                status: status.as_u16(),
                body: res.text().await.unwrap_or_default(),
            }),
        }
    }

    pub async fn upload_version(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        version: &str,
        tarball: Vec<u8>,
        readme: Option<&str>,
    ) -> Result<UploadVersionResponse, BackendError> {
        // Reject hostile shapes (`..`, leading `.`, embedded `..`, `/`,
        // `\`, control chars, empty) **before** any encoding work.
        // `urlencoding_minimal` deliberately leaves `.` unreserved per
        // RFC 3986, so a name of literally `..` produces a URL with a
        // literal `..` path segment that HTTP routers normalize away —
        // silently re-routing the `PUT` to a different endpoint. The
        // encoder is correct; we need a separate shape guard on top.
        validate_package_name(name)?;
        // Same threat model applies to the `<version>` segment — a
        // `version` of `..` percent-encodes to a literal `..` segment
        // that a normalising reverse proxy can collapse upward.
        validate_version(version)?;
        // Percent-encode every path segment. Without this, a package
        // `name` containing `/` or `..` silently routes the PUT to the
        // wrong endpoint — `PakxSource::fetch` already encodes these
        // segments and we mirror the same shape here.
        let url = self.package_version_url(owner, name, version);
        let mut req = self
            .http
            .put(url)
            .bearer_auth(token)
            .header("content-type", "application/gzip");
        // Optional README markdown piggybacked alongside the tarball.
        // The PUT body itself is the raw `.tgz` so the registry can
        // pre-check Content-Length without buffering; the README rides
        // in `x-pakx-readme-b64` (base64 to normalize header byte set —
        // raw markdown with newlines + code fences would tear into
        // multiple header lines). Omitted entirely when `readme` is
        // `None` so the registry's omit-vs-explicit semantics fire.
        // Sized at 256 KiB on the registry side; pack-time truncation
        // in `pack::load_readme` keeps the encoded value comfortably
        // under any header-length ceiling for sensible READMEs.
        if let Some(text) = readme {
            req = req.header("x-pakx-readme-b64", BASE64.encode(text.as_bytes()));
        }
        let res = req.body(tarball).send().await?;
        let status = res.status();
        match status {
            StatusCode::OK | StatusCode::CREATED => Ok(res.json::<UploadVersionResponse>().await?),
            StatusCode::UNAUTHORIZED => Err(BackendError::Unauthorized),
            StatusCode::FORBIDDEN => Err(BackendError::Forbidden),
            StatusCode::NOT_FOUND => Err(BackendError::NotFound),
            StatusCode::CONFLICT => Err(BackendError::Conflict {
                message: res.text().await.unwrap_or_default(),
            }),
            _ => Err(BackendError::Other {
                status: status.as_u16(),
                body: res.text().await.unwrap_or_default(),
            }),
        }
    }

    /// Build the `PUT` / `DELETE` package URL for a given
    /// `(owner, name, version)` triple, with every segment
    /// percent-encoded. Pulled out as a private helper so we can unit
    /// test the encoding without standing up a mock HTTP server.
    fn package_version_url(&self, owner: &str, name: &str, version: &str) -> String {
        format!(
            "{}/api/v1/packages/{}/{}/{}",
            self.base_url,
            urlencoding_minimal(owner),
            urlencoding_minimal(name),
            urlencoding_minimal(version),
        )
    }

    pub async fn unpublish(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        version: &str,
    ) -> Result<(), BackendError> {
        // Same shape guard + percent-encoding contract as
        // `upload_version`; see that method for the rationale.
        validate_package_name(name)?;
        validate_version(version)?;
        let url = self.package_version_url(owner, name, version);
        let res = self.http.delete(url).bearer_auth(token).send().await?;
        let status = res.status();
        match status {
            StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::UNAUTHORIZED => Err(BackendError::Unauthorized),
            StatusCode::FORBIDDEN => Err(BackendError::Forbidden),
            StatusCode::NOT_FOUND => Err(BackendError::NotFound),
            _ => Err(BackendError::Other {
                status: status.as_u16(),
                body: res.text().await.unwrap_or_default(),
            }),
        }
    }
}

/// Thin wrapper around [`pakx_core::validate_package_name`] that lifts
/// the shared `ValidationError` into this crate's `BackendError`
/// variant. The shape rules — empty, `..`, leading `.`, embedded `..`,
/// `/` / `\` / control chars — live in pakx-core so the CLI and any
/// future consumer (e.g. a registry-side typed client) all enforce the
/// same contract. See [`pakx_core::validation`] for the threat model.
fn validate_package_name(name: &str) -> Result<(), BackendError> {
    core_validate_package_name(name).map_err(|ValidationError { input, reason }| {
        BackendError::InvalidName {
            name: input,
            reason,
        }
    })
}

/// Mirror of [`validate_package_name`] for the `<version>` URL path
/// segment. Same threat model — a literal `..` segment after the
/// minimal RFC 3986 encoder is a router-normalisation hazard — but a
/// tighter character whitelist because the semver-friendly set is
/// well-defined (see [`pakx_core::validate_version`]).
fn validate_version(version: &str) -> Result<(), BackendError> {
    core_validate_version(version).map_err(|ValidationError { input, reason }| {
        BackendError::InvalidVersion {
            version: input,
            reason,
        }
    })
}

/// Minimal percent-encoder for URL path segments. Encodes everything
/// outside the unreserved set (RFC 3986 §2.3) — notably `/`, which is
/// the byte that lets a hostile package name escape the routing.
fn urlencoding_minimal(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{validate_package_name, validate_version, BackendError, PakxBackend};

    #[test]
    fn package_version_url_encodes_slash_in_name() {
        // A hostile name `foo/bar` must NOT route to
        // `/api/v1/packages/<owner>/foo/bar/<version>` — the embedded
        // slash would silently re-route the PUT to a different
        // endpoint. Verify the slash gets percent-encoded.
        let b = PakxBackend::new("https://registry.pakx.dev");
        let url = b.package_version_url("alice", "foo/bar", "1.0.0");
        assert_eq!(
            url,
            "https://registry.pakx.dev/api/v1/packages/alice/foo%2Fbar/1.0.0",
        );
    }

    #[test]
    fn package_version_url_encodes_traversal() {
        // The URL builder itself still emits `..` literally because
        // `.` is unreserved per RFC 3986 — that's why we layer a
        // separate `validate_package_name` shape guard in front of
        // `upload_version` / `unpublish`. The builder is verified in
        // isolation here; the guard is verified in the validator tests
        // below. Embedded `/` is still encoded by the encoder itself.
        let b = PakxBackend::new("https://registry.pakx.dev");
        let url = b.package_version_url("alice", "..", "1.0.0");
        assert_eq!(
            url,
            "https://registry.pakx.dev/api/v1/packages/alice/../1.0.0",
        );
        let url2 = b.package_version_url("alice", "../escape", "1.0.0");
        assert_eq!(
            url2,
            "https://registry.pakx.dev/api/v1/packages/alice/..%2Fescape/1.0.0",
        );
    }

    #[test]
    fn validate_package_name_accepts_plain_name() {
        assert!(validate_package_name("foo").is_ok());
        assert!(validate_package_name("foo-bar").is_ok());
        assert!(validate_package_name("foo_bar").is_ok());
        assert!(validate_package_name("foo.bar").is_ok());
        assert!(validate_package_name("a1b2").is_ok());
    }

    #[test]
    fn validate_package_name_rejects_double_dot() {
        let err = validate_package_name("..").unwrap_err();
        assert!(
            matches!(err, BackendError::InvalidName { ref name, .. } if name == ".."),
            "expected InvalidName for `..`, got {err:?}",
        );
    }

    #[test]
    fn validate_package_name_rejects_embedded_traversal() {
        let err = validate_package_name("foo/../bar").unwrap_err();
        assert!(
            matches!(err, BackendError::InvalidName { .. }),
            "expected InvalidName for `foo/../bar`, got {err:?}",
        );
        // Even without slashes, a literal `..` substring is fatal —
        // because the encoder leaves dots alone and HTTP routers
        // normalize `..` segments.
        let err = validate_package_name("foo..bar").unwrap_err();
        assert!(matches!(err, BackendError::InvalidName { .. }));
    }

    #[test]
    fn validate_package_name_rejects_leading_dot() {
        let err = validate_package_name(".hidden").unwrap_err();
        assert!(
            matches!(err, BackendError::InvalidName { ref name, .. } if name == ".hidden"),
            "expected InvalidName for `.hidden`, got {err:?}",
        );
    }

    #[test]
    fn validate_package_name_rejects_backslash() {
        let err = validate_package_name("foo\\bar").unwrap_err();
        assert!(
            matches!(err, BackendError::InvalidName { .. }),
            "expected InvalidName for `foo\\bar`, got {err:?}",
        );
    }

    #[test]
    fn validate_package_name_rejects_slash_and_control_and_empty() {
        assert!(matches!(
            validate_package_name("foo/bar").unwrap_err(),
            BackendError::InvalidName { .. },
        ));
        assert!(matches!(
            validate_package_name("foo\nbar").unwrap_err(),
            BackendError::InvalidName { .. },
        ));
        assert!(matches!(
            validate_package_name("").unwrap_err(),
            BackendError::InvalidName { .. },
        ));
        assert!(matches!(
            validate_package_name(".").unwrap_err(),
            BackendError::InvalidName { .. },
        ));
    }

    #[test]
    fn package_version_url_encodes_version_with_plus() {
        // SemVer build metadata uses `+`, which means "space" in
        // query strings — encode it for safety.
        let b = PakxBackend::new("https://registry.pakx.dev");
        let url = b.package_version_url("alice", "demo", "1.0.0+build");
        assert_eq!(
            url,
            "https://registry.pakx.dev/api/v1/packages/alice/demo/1.0.0%2Bbuild",
        );
    }

    #[test]
    fn package_version_url_trims_trailing_base_slash() {
        let b = PakxBackend::new("https://registry.pakx.dev/");
        let url = b.package_version_url("a", "b", "1.0.0");
        assert_eq!(url, "https://registry.pakx.dev/api/v1/packages/a/b/1.0.0");
    }

    #[test]
    fn validate_version_accepts_semver_shapes() {
        assert!(validate_version("0.1.0").is_ok());
        assert!(validate_version("1.0.0-rc.1").is_ok());
        assert!(validate_version("1.0.0+build.7").is_ok());
    }

    #[test]
    fn validate_version_rejects_traversal_and_empty() {
        // Each of these would percent-encode to a literal `..` segment
        // (or worse) and silently re-route the PUT / DELETE to a
        // different endpoint after a normalising reverse proxy
        // collapsed the path. The encoder is doing the right thing for
        // `.` / `..` per RFC 3986; the shape guard catches the input.
        assert!(matches!(
            validate_version("..").unwrap_err(),
            BackendError::InvalidVersion { .. },
        ));
        assert!(matches!(
            validate_version("../etc").unwrap_err(),
            BackendError::InvalidVersion { .. },
        ));
        assert!(matches!(
            validate_version("").unwrap_err(),
            BackendError::InvalidVersion { .. },
        ));
        assert!(matches!(
            validate_version("1..0").unwrap_err(),
            BackendError::InvalidVersion { .. },
        ));
    }

    #[test]
    fn validate_version_rejects_unsafe_chars_and_leading_dash() {
        assert!(matches!(
            validate_version("1.0.0/x").unwrap_err(),
            BackendError::InvalidVersion { .. },
        ));
        // Leading `-` would land in `clap`-style flag parsing on any
        // shell tooling downstream.
        assert!(matches!(
            validate_version("-1.0.0").unwrap_err(),
            BackendError::InvalidVersion { .. },
        ));
        // Control char.
        assert!(matches!(
            validate_version("1.0.0\n").unwrap_err(),
            BackendError::InvalidVersion { .. },
        ));
    }

    #[tokio::test]
    async fn upload_version_rejects_hostile_version_before_http() {
        // No mock server stood up — if the validator falls through to
        // an HTTP send the test will hang on TCP and fail by timeout,
        // making "validator fired pre-network" the only way the test
        // passes.
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .upload_version("tok", "alice", "demo", "..", vec![], None)
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidVersion { .. }));
    }

    #[tokio::test]
    async fn unpublish_rejects_hostile_version_before_http() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .unpublish("tok", "alice", "demo", "../escape")
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidVersion { .. }));
    }
}
