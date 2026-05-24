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
    /// HTTP 411 from the version PUT path. Emitted when the tarball
    /// upload request lacks a parseable `Content-Length`, or when the
    /// client sent `Transfer-Encoding: chunked`. The CLI always sends a
    /// declared `Content-Length`, so a 411 here points at a build-tool
    /// or proxy stripping the header — not a publisher-fixable input.
    #[error("length required (411): registry rejected the upload without a Content-Length header")]
    LengthRequired,
    /// HTTP 413 from the version PUT path. Emitted when the tarball
    /// exceeds the registry's hard cap (50 MiB as of v0.1). Carries the
    /// cap parsed from the response body (`maxBytes` field) when the
    /// shape is recognisable, so the CLI hint can quote the exact
    /// ceiling instead of guessing.
    #[error("payload too large (413): tarball exceeds registry cap")]
    TooLarge { max_bytes: Option<u64> },
    /// HTTP 409 specifically meaning "this version was already
    /// published". Distinguished from the generic `Conflict` variant
    /// (which the upload path mapped before this refactor) so the CLI
    /// hint can call out the exact next-step ("bump the version") and
    /// the `--json` payload can pin `errorKind: "version-exists"`.
    #[error("version already published (409)")]
    VersionExists,
    /// HTTP 409 from the POST upsert path when a republish under a
    /// different `kind` is attempted. The registry refuses kind changes
    /// in-place because the kind picks the install destination. Carries
    /// the stored + received kinds from the JSON body when parseable so
    /// the CLI hint can quote both sides.
    #[error("kind mismatch (409): stored={stored:?} received={received:?}")]
    KindMismatch {
        stored: Option<String>,
        received: Option<String>,
    },
    /// HTTP 400 with a registry-side validation reason. Emitted by the
    /// POST upsert when the manifest fails the zod schema (name shape,
    /// description length, sponsors / keywords / readme schema) and by
    /// the version PUT for `empty`, `invalid-id`, and oversize-README
    /// inputs. Carries the `detail` field from the response body when
    /// present (a string in some shapes, a structured zod-flatten
    /// object in others — stringified verbatim for the CLI hint).
    #[error("invalid request (400){}", detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default())]
    Invalid { detail: Option<String> },
    /// HTTP 429 — caller hit the per-user (publish bucket) or per-IP
    /// (search bucket) rate limit. `retry_after_secs` is parsed from
    /// the `Retry-After` response header set by
    /// `lib/rate-limit.ts::withRateLimitHeaders`.
    #[error("rate limited (429)")]
    RateLimited { retry_after_secs: Option<u64> },
    /// HTTP 500 — uncaught registry-side throw. The response body is
    /// redacted to `{ error: "internal" }` in production by the
    /// `lib/api-errors.ts::internalError` helper; the operator-facing
    /// detail lives only in the registry's server log.
    #[error("registry internal error (500)")]
    Internal,
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
            // 429 is bucketed per-user on the POST upsert (the
            // `PUBLISH_RATE` bucket on
            // `lib/rate-limit.ts::withRateLimitHeaders`). The
            // `Retry-After` header is set when the limiter trips —
            // surface it so the CLI hint can quote a wait time instead
            // of guessing.
            StatusCode::TOO_MANY_REQUESTS => Err(BackendError::RateLimited {
                retry_after_secs: parse_retry_after(&res),
            }),
            // 409 on the POST upsert path is exclusively
            // `kind-mismatch` (see `app/api/v1/packages/route.ts`). The
            // version-collision 409 lives on the PUT version path. We
            // lift the stored + received kinds out of the JSON body so
            // the CLI hint can quote both sides; the body has the
            // shape `{ error, stored, received, hint }`.
            StatusCode::CONFLICT => {
                let body = res.text().await.unwrap_or_default();
                let parsed = parse_error_body(&body);
                let stored = parsed.as_ref().and_then(|v| {
                    v.get("stored")
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                });
                let received = parsed.as_ref().and_then(|v| {
                    v.get("received")
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                });
                Err(BackendError::KindMismatch { stored, received })
            }
            // 400 on the POST upsert path is the zod-refusal branch.
            // The body shape is `{ error: "invalid", detail: <string |
            // object> }`; we lift the detail verbatim so the CLI hint
            // can echo the registry's own complaint without inventing
            // wording.
            StatusCode::BAD_REQUEST => {
                let body = res.text().await.unwrap_or_default();
                Err(BackendError::Invalid {
                    detail: parse_error_body(&body).as_ref().and_then(detail_from_value),
                })
            }
            StatusCode::INTERNAL_SERVER_ERROR => Err(BackendError::Internal),
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
        // The `<owner>` half of the path is subject to the same threat
        // model — a malicious manifest with `owner: ".."` percent-encodes
        // to `PUT /api/v1/packages/../<name>/<version>` which a
        // normalising CDN collapses upward. The bearer-token /
        // package-ownership check on the registry side stops the
        // cross-namespace publish, but defense-in-depth says the CLI
        // shouldn't be a willing accomplice to traversal probes.
        validate_package_name(owner)?;
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
            // 409 on the version PUT path is exclusively
            // `version-exists` (see `app/api/v1/packages/[owner]/[name]/
            // [version]/route.ts`). The kind-mismatch 409 lives on the
            // POST upsert path. We surface the dedicated variant so the
            // CLI hint can be specific ("bump the version in
            // agents.yml") and `--json` callers can branch on
            // `errorKind: "version-exists"`.
            StatusCode::CONFLICT => Err(BackendError::VersionExists),
            // 411 surfaces when the PUT lacked a parseable
            // Content-Length header (or sent chunked encoding). The
            // CLI always sends a numeric Content-Length, so the only
            // way a 411 reaches us is via an intermediary stripping
            // the header — bubble a typed variant so the hint can
            // call out the proxy-side likely cause.
            StatusCode::LENGTH_REQUIRED => Err(BackendError::LengthRequired),
            // 413 with `{ error: "too-large", maxBytes: <n> }`. Lift
            // `maxBytes` so the CLI can quote the registry's exact
            // ceiling instead of guessing.
            StatusCode::PAYLOAD_TOO_LARGE => {
                let body = res.text().await.unwrap_or_default();
                Err(BackendError::TooLarge {
                    max_bytes: parse_error_body(&body)
                        .and_then(|v| v.get("maxBytes").and_then(serde_json::Value::as_u64)),
                })
            }
            // 400 on the version PUT covers `empty`, `invalid-id`, and
            // oversize-README inputs (see `readReadmeHeader`). Lift the
            // optional `detail` for the CLI hint, same as POST.
            StatusCode::BAD_REQUEST => {
                let body = res.text().await.unwrap_or_default();
                Err(BackendError::Invalid {
                    detail: parse_error_body(&body).as_ref().and_then(detail_from_value),
                })
            }
            StatusCode::TOO_MANY_REQUESTS => Err(BackendError::RateLimited {
                retry_after_secs: parse_retry_after(&res),
            }),
            StatusCode::INTERNAL_SERVER_ERROR => Err(BackendError::Internal),
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
        // `upload_version`; see that method for the rationale (covers
        // the `<owner>` traversal probe too).
        validate_package_name(owner)?;
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

/// Parse a registry JSON error body into a `serde_json::Value` if it
/// looks like one. We deliberately swallow parse failures (returning
/// `None`) so a non-JSON 4xx body never panics the caller — the typed
/// variant just falls back to a `None` field and the CLI hint uses the
/// generic copy. Capped at 64 KiB so a runaway HTML page from a CDN
/// error response can't push megabytes through `serde_json`.
fn parse_error_body(body: &str) -> Option<serde_json::Value> {
    if body.is_empty() || body.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_str(body).ok()
}

/// Pull the `detail` field out of a parsed registry error body.
///
/// The registry emits `detail` in two shapes:
///   - A plain string (most paths): `{ error: "invalid", detail: "readme too large (max 256 KiB)" }`
///   - A zod `flatten()` object (POST schema-refusal): `{ error: "invalid", detail: { formErrors: [...], fieldErrors: {...} } }`
///
/// We stringify the object shape with `to_string` so the CLI hint can
/// echo it verbatim. Non-string, non-object values (or absent `detail`)
/// collapse to `None`.
fn detail_from_value(v: &serde_json::Value) -> Option<String> {
    match v.get("detail") {
        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(other @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) => {
            Some(other.to_string())
        }
        _ => None,
    }
}

/// Read the `Retry-After` response header as a non-negative integer
/// number of seconds. The HTTP spec also permits an HTTP-date but the
/// registry's `withRateLimitHeaders` always emits seconds, so we only
/// parse the integer form. Returns `None` on absent / non-integer
/// values; the CLI hint falls back to a generic wait time.
fn parse_retry_after(res: &reqwest::Response) -> Option<u64> {
    res.headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
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

    // ---------------------------------------------------------------
    // Owner-half guard — defense-in-depth against a manifest with a
    // hostile `owner:` field. The encoder leaves `.` unreserved so a
    // literal `..` segment would otherwise reach the wire and a
    // normalising CDN would collapse it upward. The bearer-token
    // ownership check on the registry stops the cross-namespace publish
    // attempt; we refuse to be the willing accomplice on the CLI side.
    // No mock server is stood up — a fall-through to the HTTP layer
    // would hang on TCP and fail the test by timeout, making
    // "validator fired pre-network" the only passing path.
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn upload_version_rejects_hostile_owner_double_dot() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .upload_version("tok", "..", "demo", "1.0.0", vec![], None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, BackendError::InvalidName { ref name, .. } if name == ".."),
            "expected InvalidName for owner `..`, got {err:?}",
        );
    }

    #[tokio::test]
    async fn upload_version_rejects_hostile_owner_embedded_traversal() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .upload_version("tok", "../escape", "demo", "1.0.0", vec![], None)
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidName { .. }));
    }

    #[tokio::test]
    async fn upload_version_rejects_hostile_owner_leading_dot() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .upload_version("tok", ".hidden", "demo", "1.0.0", vec![], None)
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidName { .. }));
    }

    #[tokio::test]
    async fn upload_version_rejects_hostile_owner_slash() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .upload_version("tok", "a/b", "demo", "1.0.0", vec![], None)
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidName { .. }));
    }

    #[tokio::test]
    async fn upload_version_rejects_hostile_owner_backslash() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .upload_version("tok", "a\\b", "demo", "1.0.0", vec![], None)
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidName { .. }));
    }

    #[tokio::test]
    async fn upload_version_rejects_hostile_owner_control_char() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .upload_version("tok", "a\nb", "demo", "1.0.0", vec![], None)
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidName { .. }));
    }

    #[tokio::test]
    async fn unpublish_rejects_hostile_owner_double_dot() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b.unpublish("tok", "..", "demo", "1.0.0").await.unwrap_err();
        assert!(
            matches!(err, BackendError::InvalidName { ref name, .. } if name == ".."),
            "expected InvalidName for owner `..`, got {err:?}",
        );
    }

    #[tokio::test]
    async fn unpublish_rejects_hostile_owner_embedded_traversal() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .unpublish("tok", "../escape", "demo", "1.0.0")
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidName { .. }));
    }

    #[tokio::test]
    async fn unpublish_rejects_hostile_owner_slash() {
        let b = PakxBackend::new("https://example.invalid");
        let err = b
            .unpublish("tok", "a/b", "demo", "1.0.0")
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidName { .. }));
    }
}
