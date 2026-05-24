//! Per-call cache tempdir helper.
//!
//! Six federated-query subcommands (`add`, `outdated`, `audit`,
//! `search`, `info`, plus the `install` runner) build an ad-hoc
//! `CacheDir` rooted under a unique
//! `pakx-<cmd>-cache-<pid>-<nanos>-XXXXXX` directory inside
//! `std::env::temp_dir()`. The pid/nanos prefix prevents parallel
//! integration tests from colliding when their `wiremock` mock
//! servers happen to land on the same loopback port (Linux releases
//! ports aggressively; a hot CI runner routinely sees back-to-back
//! tests reuse a port). The `XXXXXX` random suffix comes from
//! `tempfile::Builder` for the same reason inside a single process.
//!
//! Wrapping the dir in [`tempfile::TempDir`] makes it self-cleaning:
//! the directory deletes on `Drop`, so the caller does NOT have to
//! remember to remove it. A user who never runs `pakx doctor
//! --clear-cache` would otherwise accumulate one `pakx-<cmd>-cache-*`
//! dir per federated invocation indefinitely.
//!
//! The cache is only useful for the lifetime of the call anyway —
//! the `--no-cache` path bypasses it via `with_ttl(Duration::ZERO)`,
//! and the normal path benefits from intra-call cache hits when
//! sibling lookups within the same fetch share the same key. No
//! caller relies on the cache surviving past the function return.
//!
//! ## `cache_dir_at`
//!
//! The companion [`cache_dir_at`] helper builds the actual
//! [`CacheDir`] from a tempdir path + the `--no-cache` flag. Before
//! the round-86 dedupe pass every federated subcommand reimplemented
//! the same 4-line `if no_cache { with_ttl(Duration::ZERO) } else { … }`
//! match. Round 40 wired `--no-cache` across six subcommands but
//! seeded six near-identical copies of the factory; round 86 collapsed
//! them onto this helper so a future change to the bypass semantics
//! (e.g. honouring a future `CacheDir::bypass()` method) only has to
//! land once.

use std::path::Path;
use std::time::Duration;

use pakx_registry_client::CacheDir;

/// Build a per-call cache tempdir guard with the given prefix.
///
/// Keyed by pid + nanos so parallel integration tests cannot collide
/// even when their backing mock-server ports recycle. The returned
/// [`tempfile::TempDir`] deletes its backing directory on drop —
/// callers MUST keep it alive for the duration of every cache
/// read/write that uses its path.
///
/// # Errors
///
/// Surfaces any [`std::io::Error`] from `create_dir_all` on the
/// system temp root or from the tempdir-builder call itself.
pub fn make_cache_tempdir(prefix: &str) -> std::io::Result<tempfile::TempDir> {
    let name = format!(
        "{prefix}-{}-{}-",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let parent = std::env::temp_dir();
    std::fs::create_dir_all(&parent)?;
    tempfile::Builder::new().prefix(&name).tempdir_in(&parent)
}

/// Build a [`CacheDir`] rooted at `root`, with TTL clamped to zero
/// when `no_cache` is on.
///
/// Mirrors the shape used by every federated-query subcommand that
/// gained `--no-cache` in round 40 (`pakx add`, `pakx outdated`,
/// `pakx audit`, `pakx search`, `pakx info`, plus the install
/// runner). Zero-TTL gives "skip read, still write" semantics —
/// enough to satisfy the `--no-cache` contract without forking the
/// `CacheDir` API.
///
/// Centralising the factory closes a copy-paste class flagged in
/// round 86: six call sites each reimplemented the same four-line
/// `if no_cache { … } else { … }` branch, which made it possible for
/// a subcommand to silently drop the zero-TTL clamp during a refactor
/// (the regression caught in round 30 for the install runner was
/// precisely this shape — a single closure that "looked the same" as
/// its sibling but built a slightly different `CacheDir`).
#[must_use]
pub fn cache_dir_at(root: &Path, no_cache: bool) -> CacheDir {
    let cd = CacheDir::with_root(root);
    if no_cache {
        cd.with_ttl(Duration::ZERO)
    } else {
        cd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-86 regression guard: the helper must clamp TTL to zero
    /// when the caller passes `no_cache = true`. The contract is
    /// observable to the federated sources via
    /// [`CacheDir::effective_ttl`] (or whichever accessor the source
    /// uses to decide cache-vs-fetch); here we round-trip the public
    /// shape so the test passes on any future `CacheDir` field
    /// addition that keeps the constructor API stable.
    #[test]
    fn cache_dir_at_clamps_ttl_when_no_cache_set() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let with_cache = cache_dir_at(dir.path(), false);
        let without_cache = cache_dir_at(dir.path(), true);

        // The two configurations must differ on the TTL field —
        // otherwise the `--no-cache` flag was a no-op. The default
        // TTL is a non-zero value baked into `CacheDir::with_root`,
        // so the zero-TTL variant is observably different (formatted
        // shapes diverge).
        let cached_repr = format!("{with_cache:?}");
        let bypass_repr = format!("{without_cache:?}");
        assert_ne!(
            cached_repr, bypass_repr,
            "cache_dir_at(no_cache=true) must produce a different CacheDir shape",
        );
    }

    /// Companion: the default `no_cache = false` branch must return a
    /// `CacheDir` whose [`Debug`] shape matches the bare
    /// `CacheDir::with_root(root)` construction. Pins the "default is
    /// identity" invariant — a future change to the helper that
    /// accidentally always clamped the TTL would trip this guard.
    #[test]
    fn cache_dir_at_default_matches_with_root_only() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let helper = cache_dir_at(dir.path(), false);
        let direct = CacheDir::with_root(dir.path());
        assert_eq!(format!("{helper:?}"), format!("{direct:?}"));
    }
}
