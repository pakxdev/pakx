//! Integration tests for the file-backed cache.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pakx_registry_client::{CacheDir, RegistryError};
use tempfile::TempDir;

#[tokio::test]
async fn first_call_invokes_fetcher_second_is_cached() {
    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path());
    let calls = Arc::new(AtomicUsize::new(0));

    for _ in 0..3 {
        let calls = calls.clone();
        let value: String = cache
            .get_or_fetch("k", move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, RegistryError>("hello".to_string())
            })
            .await
            .unwrap();
        assert_eq!(value, "hello");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ttl_expiry_refetches() {
    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path()).with_ttl(Duration::from_millis(40));
    let calls = Arc::new(AtomicUsize::new(0));

    for _ in 0..2 {
        let calls = calls.clone();
        let _: String = cache
            .get_or_fetch("k", move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, RegistryError>("v".to_string())
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn different_keys_get_different_files() {
    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path());

    let a: String = cache
        .get_or_fetch("a", || async { Ok::<_, RegistryError>("A".to_string()) })
        .await
        .unwrap();
    let b: String = cache
        .get_or_fetch("b", || async { Ok::<_, RegistryError>("B".to_string()) })
        .await
        .unwrap();
    assert_eq!(a, "A");
    assert_eq!(b, "B");

    // Cache root contains two files.
    let n = std::fs::read_dir(cache.root()).unwrap().count();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn invalidate_forces_refetch() {
    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path());
    let calls = Arc::new(AtomicUsize::new(0));

    let calls1 = calls.clone();
    let _: String = cache
        .get_or_fetch("k", move || async move {
            calls1.fetch_add(1, Ordering::SeqCst);
            Ok::<_, RegistryError>("v".to_string())
        })
        .await
        .unwrap();

    cache.invalidate("k").await.unwrap();

    let calls2 = calls.clone();
    let _: String = cache
        .get_or_fetch("k", move || async move {
            calls2.fetch_add(1, Ordering::SeqCst);
            Ok::<_, RegistryError>("v".to_string())
        })
        .await
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn corrupt_cache_file_triggers_refetch() {
    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path());

    // Populate.
    let _: String = cache
        .get_or_fetch("k", || async { Ok::<_, RegistryError>("v".to_string()) })
        .await
        .unwrap();

    // Corrupt every cache file.
    for entry in std::fs::read_dir(cache.root()).unwrap() {
        let path = entry.unwrap().path();
        std::fs::write(&path, b"not json").unwrap();
    }

    // Should re-fetch and overwrite, not error.
    let v: String = cache
        .get_or_fetch("k", || async { Ok::<_, RegistryError>("v2".to_string()) })
        .await
        .unwrap();
    assert_eq!(v, "v2");
}

/// `--no-cache` propagation lands here: every CLI subcommand that
/// honours `--no-cache` builds its `CacheDir` with
/// `with_ttl(Duration::ZERO)`. The CLI surface tests can only assert
/// that the flag parses and the command succeeds; the **behavioural
/// contract** ("zero TTL means the next call always refetches") has to
/// be pinned at this layer. Without this test, a future refactor that
/// changed `>` to `>=` on the TTL comparison (or replaced `ZERO` with
/// a tiny duration) would silently degrade every `--no-cache` flag
/// across the CLI without tripping a single integration test.
#[tokio::test]
async fn ttl_zero_always_refetches() {
    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path()).with_ttl(Duration::ZERO);
    let calls = Arc::new(AtomicUsize::new(0));

    // Three back-to-back hits on the same key. With TTL=0 every hit
    // must invoke the fetcher — the read-side gate considers any
    // age > 0 expired, and SystemTime::now() advances between calls.
    for _ in 0..3 {
        let calls = calls.clone();
        let value: String = cache
            .get_or_fetch("hot-key", move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, RegistryError>("fresh".to_string())
            })
            .await
            .unwrap();
        assert_eq!(value, "fresh");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "with TTL=0 every read must refetch"
    );
}

/// Symmetric to `ttl_zero_always_refetches`: confirm the default TTL
/// (1 hour) keeps a freshly-written entry for at least one back-to-back
/// read. Pins the contract that `--no-cache` is the *only* lever that
/// changes the read path; absent the flag, callers see cached bytes.
#[tokio::test]
async fn default_ttl_keeps_entry_for_back_to_back_read() {
    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path()); // default 1h TTL
    let calls = Arc::new(AtomicUsize::new(0));

    for _ in 0..5 {
        let calls = calls.clone();
        let _: String = cache
            .get_or_fetch("k", move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, RegistryError>("v".to_string())
            })
            .await
            .unwrap();
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "default TTL must hold a hot entry across back-to-back reads"
    );
}

/// `invalidate()` on a missing key must be a clean no-op, not a hard
/// error. Cache-eviction callers (`pakx doctor --reset-cache` etc.)
/// rely on being able to invalidate eagerly without first checking
/// existence. `NotFound` at the FS layer is part of the happy path here.
#[tokio::test]
async fn invalidate_missing_key_is_silent_noop() {
    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path());
    // Cache empty — nothing under this key has ever been written.
    cache
        .invalidate("ghost")
        .await
        .expect("invalidate must tolerate missing entries");
    // And idempotently: a second invalidate on the same missing key
    // still returns Ok.
    cache
        .invalidate("ghost")
        .await
        .expect("invalidate must be idempotent on missing entries");
}

/// Round-trip a serializable struct, not just a bare `String`, to pin
/// the `Serialize + DeserializeOwned` bound covers real registry shapes
/// (the source modules cache `Package`, `PackageVersion`, etc.).
/// Regression guard against a future change that tightened the bounds
/// to `Display + FromStr` or similar — the source-side callers would
/// then silently lose their cache reads.
#[tokio::test]
async fn struct_value_round_trips_through_cache() {
    use serde::{Deserialize, Serialize};
    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Pkg {
        id: String,
        versions: Vec<String>,
    }
    let temp = TempDir::new().unwrap();
    let cache = CacheDir::with_root(temp.path());
    let stored = Pkg {
        id: "alice/hello".into(),
        versions: vec!["0.1.0".into(), "0.1.1".into()],
    };
    let _ = cache
        .get_or_fetch::<Pkg, _, _>("pkg", || async {
            Ok::<_, RegistryError>(Pkg {
                id: "alice/hello".into(),
                versions: vec!["0.1.0".into(), "0.1.1".into()],
            })
        })
        .await
        .unwrap();
    // Second call must read from disk — proves the round-trip works.
    let read: Pkg = cache
        .get_or_fetch::<Pkg, _, _>("pkg", || async {
            panic!("must not refetch — second call should hit cache")
        })
        .await
        .unwrap();
    assert_eq!(read, stored);
}
