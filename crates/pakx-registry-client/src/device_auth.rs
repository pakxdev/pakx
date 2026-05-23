//! Device authorization grant client for `pakx login --device`.
//!
//! Two endpoints (registry-side contract is frozen — see
//! `pakx-registry/app/api/v1/auth/device/*`):
//!
//!   POST /api/v1/auth/device                  → initiate
//!   POST /api/v1/auth/device/poll             → poll for completion
//!
//! Wire shape mirrors RFC 8628 (OAuth 2.0 Device Authorization Grant),
//! though the registry serves it as a first-party pakx flow rather than
//! as a generic OAuth provider. The CLI never persists `device_code`
//! beyond the in-process poll loop; the only artifact written to disk
//! is the `pakx_v1_…` token returned by a `success` poll, and that goes
//! straight through [`pakx_core::Credentials`].
//!
//! Security caveats honoured by the CLI consumer (`pakx login --device`):
//! - Interval timing uses [`std::time::Instant`] (monotonic). Wall-clock
//!   drift from NTP adjustments must not collapse a long-running poll
//!   loop into a busy-spin.
//! - `slow_down` bumps the local interval by **at least** 5 seconds
//!   (RFC 8628 §3.5 minimum). If the server returns a fresh `interval`
//!   value alongside `slow_down`, the CLI takes the maximum of
//!   `previous + 5` and `server_interval` so an aggressive server can
//!   still wind us down further.
//! - The `token` field never logs at `tracing` level above `debug` and
//!   never prints to stdout/stderr — it goes from the HTTP response
//!   straight into the credentials file.

use pakx_core::http_client;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeviceAuthError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("registry rejected device-auth initiation ({status}): {body}")]
    Initiate { status: u16, body: String },
    #[error("registry rejected device-auth poll ({status}): {body}")]
    Poll { status: u16, body: String },
    #[error("registry returned malformed device-auth payload: {0}")]
    Malformed(String),
}

/// Request body for `POST /api/v1/auth/device`.
///
/// Both fields optional, each capped at 80 chars by the registry —
/// the CLI truncates locally to keep the wire shape inside the
/// documented limit even if a hostname / OS string is unusually long.
#[derive(Debug, Clone, Serialize, Default)]
pub struct InitiateRequest {
    #[serde(rename = "clientHostname", skip_serializing_if = "Option::is_none")]
    pub client_hostname: Option<String>,
    #[serde(rename = "clientOs", skip_serializing_if = "Option::is_none")]
    pub client_os: Option<String>,
}

/// Response to `POST /api/v1/auth/device`.
#[derive(Debug, Clone, Deserialize)]
pub struct InitiateResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    /// Total window the device code is valid for, in seconds.
    pub expires_in: u64,
    /// Minimum poll interval, in seconds.
    pub interval: u64,
}

/// Status discriminator returned by `POST /api/v1/auth/device/poll`.
/// `Success` is the only variant that carries a `token`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollStatus {
    Pending,
    SlowDown,
    Denied,
    Expired,
    Success,
}

/// Response to `POST /api/v1/auth/device/poll`. `interval` is optional
/// — the registry MAY return a fresh interval on `slow_down`; if absent
/// the CLI bumps its local interval by 5s per RFC 8628 §3.5.
#[derive(Debug, Clone, Deserialize)]
pub struct PollResponse {
    pub status: PollStatus,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub interval: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct PollRequest<'a> {
    device_code: &'a str,
}

#[derive(Debug, Clone)]
pub struct DeviceAuthClient {
    http: Client,
    base_url: String,
}

impl DeviceAuthClient {
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        Self::with_client(http_client(), base_url)
    }

    #[must_use]
    pub fn with_client(http: Client, base_url: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    pub async fn initiate(
        &self,
        req: &InitiateRequest,
    ) -> Result<InitiateResponse, DeviceAuthError> {
        let res = self
            .http
            .post(format!("{}/api/v1/auth/device", self.base_url))
            .json(req)
            .send()
            .await?;
        let status = res.status();
        if status != StatusCode::OK {
            return Err(DeviceAuthError::Initiate {
                status: status.as_u16(),
                body: res.text().await.unwrap_or_default(),
            });
        }
        let body = res.json::<InitiateResponse>().await.map_err(|e| {
            DeviceAuthError::Malformed(format!("could not decode initiate payload: {e}"))
        })?;
        if body.device_code.is_empty() || body.user_code.is_empty() {
            return Err(DeviceAuthError::Malformed(
                "initiate payload missing device_code or user_code".to_owned(),
            ));
        }
        Ok(body)
    }

    pub async fn poll(&self, device_code: &str) -> Result<PollResponse, DeviceAuthError> {
        let res = self
            .http
            .post(format!("{}/api/v1/auth/device/poll", self.base_url))
            .json(&PollRequest { device_code })
            .send()
            .await?;
        let status = res.status();
        if status != StatusCode::OK {
            return Err(DeviceAuthError::Poll {
                status: status.as_u16(),
                body: res.text().await.unwrap_or_default(),
            });
        }
        let body = res.json::<PollResponse>().await.map_err(|e| {
            DeviceAuthError::Malformed(format!("could not decode poll payload: {e}"))
        })?;
        if matches!(body.status, PollStatus::Success) && body.token.is_none() {
            return Err(DeviceAuthError::Malformed(
                "poll status=success but token field is missing".to_owned(),
            ));
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceAuthClient, InitiateRequest, PollStatus};

    #[test]
    fn initiate_request_skips_none_fields() {
        let req = InitiateRequest {
            client_hostname: Some("host.local".to_owned()),
            client_os: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"clientHostname\":\"host.local\""));
        assert!(
            !json.contains("clientOs"),
            "absent clientOs must not serialize: {json}",
        );
    }

    #[test]
    fn poll_status_serde_uses_snake_case() {
        let pending: PollStatus = serde_json::from_str("\"pending\"").unwrap();
        assert_eq!(pending, PollStatus::Pending);
        let slow: PollStatus = serde_json::from_str("\"slow_down\"").unwrap();
        assert_eq!(slow, PollStatus::SlowDown);
        let denied: PollStatus = serde_json::from_str("\"denied\"").unwrap();
        assert_eq!(denied, PollStatus::Denied);
        let expired: PollStatus = serde_json::from_str("\"expired\"").unwrap();
        assert_eq!(expired, PollStatus::Expired);
        let success: PollStatus = serde_json::from_str("\"success\"").unwrap();
        assert_eq!(success, PollStatus::Success);
    }

    #[test]
    fn client_trims_trailing_base_slash() {
        // Future-proof: the URL builders use `format!` against
        // `self.base_url`. If a caller passes a trailing slash the
        // resulting URL would be `…//api/v1/…` which most routers
        // accept but is ugly + asymmetric with `PakxBackend`.
        let c = DeviceAuthClient::new("https://registry.pakx.dev/");
        // We cannot reach the private field; assert via Debug instead.
        let s = format!("{c:?}");
        assert!(
            s.contains("base_url: \"https://registry.pakx.dev\""),
            "trailing slash should be trimmed: {s}",
        );
    }
}
