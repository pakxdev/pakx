//! Per-call cache tempdir helper.
//!
//! Five federated-query subcommands (`add`, `outdated`, `audit`,
//! `search`) build an ad-hoc `CacheDir` rooted under a unique
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
