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
