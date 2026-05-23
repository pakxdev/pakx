//! Shared `reqwest::Client` factory with project-wide timeouts.
//!
//! Every HTTP call in pakx ultimately funnels through `reqwest::Client`.
//! Constructing one via `Client::new()` is convenient but leaves both
//! the request timeout and the connect timeout at reqwest's default of
//! **none** — so a half-open TCP connection to a slow / blackholed
//! registry will hang the CLI indefinitely. The user can't even
//! `Ctrl+C` out of the install loop cleanly because the futures are
//! parked on the network, not on a tokio timer.
//!
//! This module centralises client construction so:
//!
//! - `request_timeout` defaults to 60s — long enough for an ordinary
//!   registry round-trip (federated search + metadata fetch) but short
//!   enough that a hung CI doesn't sit for 30 minutes before the job
//!   scheduler kills it.
//! - `connect_timeout` defaults to 15s — fail fast on DNS / TCP issues
//!   instead of letting the request-timeout absorb the connect cost.
//! - Long-running uploads (`pakx publish`, in particular the tarball
//!   PUT) can opt into a longer request timeout via
//!   [`http_client_with_timeout`].
//!
//! All other call sites that previously used `reqwest::Client::new()`
//! must route through [`http_client`] so the timeout discipline is
//! uniform across `install`, `publish`, `search`, `outdated`, `audit`,
//! `add`, `info`, `upgrade`, and the registry sources. Auditing for
//! drift is then a single `grep Client::new` across the workspace.

use std::time::Duration;

use reqwest::Client;

/// Default request-level timeout applied to every client built via
/// [`http_client`].
///
/// Long enough for the slowest legitimate registry round-trip we've
/// seen in the wild (federated `pakx search` against a cold CDN),
/// short enough that a wedged CI never sits indefinitely.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Default TCP/TLS connect timeout. Separate from the request timeout
/// so DNS / unreachable-host errors surface in seconds rather than
/// being absorbed into the full 60s request budget.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Request timeout for tarball uploads (`pakx publish` PUT). The
/// default 60s is too aggressive for a 50 MiB upload over a slow
/// residential uplink; 5 minutes matches the registry's own server-
/// side limit.
pub const UPLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Build a `reqwest::Client` with the project-wide default timeouts.
///
/// Panics only if `reqwest` itself fails to construct the underlying
/// TLS stack — a programmer error (missing feature flag), not a
/// runtime condition. Every call site in pakx already treats client
/// construction as infallible, so panicking matches the prior
/// `Client::new()` semantics exactly.
#[must_use]
pub fn http_client() -> Client {
    http_client_with_timeout(DEFAULT_REQUEST_TIMEOUT)
}

/// Build a `reqwest::Client` with a caller-supplied request timeout
/// and the project-wide default connect timeout.
///
/// Use this for code paths that exceed the default 60s budget —
/// primarily tarball uploads in `pakx publish`.
#[must_use]
pub fn http_client_with_timeout(request_timeout: Duration) -> Client {
    Client::builder()
        .timeout(request_timeout)
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .build()
        .expect("http client builder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_client_builds() {
        let _c = http_client();
    }

    #[test]
    fn http_client_with_timeout_builds() {
        let _c = http_client_with_timeout(Duration::from_secs(1));
        let _c = http_client_with_timeout(UPLOAD_REQUEST_TIMEOUT);
    }
}
