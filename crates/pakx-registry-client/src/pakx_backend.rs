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
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        Self::with_client(Client::new(), base_url)
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
    ) -> Result<UploadVersionResponse, BackendError> {
        // Percent-encode every path segment. Without this, a package
        // `name` containing `/` or `..` silently routes the PUT to the
        // wrong endpoint — `PakxSource::fetch` already encodes these
        // segments and we mirror the same shape here.
        let url = self.package_version_url(owner, name, version);
        let res = self
            .http
            .put(url)
            .bearer_auth(token)
            .header("content-type", "application/gzip")
            .body(tarball)
            .send()
            .await?;
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
        // Same percent-encoding contract as `upload_version`.
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
    use super::PakxBackend;

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
        // `..` is unreserved (only dots) so it isn't percent-encoded,
        // but the trailing `/` of `../something` is — which alone
        // breaks the traversal. Document the behaviour with a test.
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
}
