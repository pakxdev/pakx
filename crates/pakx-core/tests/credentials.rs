//! Unit tests for the credentials store.

use pakx_core::{CredentialEntry, Credentials};
use tempfile::TempDir;

fn entry(token: &str, login: &str) -> CredentialEntry {
    CredentialEntry {
        token: token.into(),
        login: Some(login.into()),
        created_at: Some("epoch:0".into()),
    }
}

#[test]
fn read_missing_returns_empty() {
    let temp = TempDir::new().unwrap();
    let creds = Credentials::read_from(&temp.path().join("missing.json")).unwrap();
    assert!(creds.registries.is_empty());
}

#[test]
fn round_trip_set_get() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("c.json");
    let mut creds = Credentials::default();
    creds.set("https://example.com", entry("pakx_v1_aaa", "alice"));
    creds.write_to(&path).unwrap();

    let loaded = Credentials::read_from(&path).unwrap();
    let got = loaded.get("https://example.com").unwrap();
    assert_eq!(got.token, "pakx_v1_aaa");
    assert_eq!(got.login.as_deref(), Some("alice"));
}

#[test]
fn url_normalisation_strips_trailing_slash_and_lowercases() {
    let mut creds = Credentials::default();
    creds.set("https://Example.com/", entry("t", "a"));
    assert!(creds.get("https://example.com").is_some());
    assert!(creds.get("https://example.com/").is_some());
}

#[test]
fn remove_returns_previous() {
    let mut creds = Credentials::default();
    creds.set("https://x.test", entry("t", "a"));
    let prev = creds.remove("https://x.test").unwrap();
    assert_eq!(prev.token, "t");
    assert!(creds.get("https://x.test").is_none());
}

/// `Credentials::Entry` is `deny_unknown_fields`. A typo'd key (or a
/// future field we don't know about) must surface as a parse error
/// instead of being silently dropped — the token is the only field we
/// cannot afford to lose on round-trip.
#[test]
fn entry_rejects_unknown_fields() {
    use pakx_core::CredentialsError;

    let body = r#"{
        "registries": {
            "https://example.com": {
                "token": "pakx_v1_aaa",
                "login": "alice",
                "unexpected_field": "oops"
            }
        }
    }"#;
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("c.json");
    std::fs::write(&path, body).unwrap();

    let err = Credentials::read_from(&path).unwrap_err();
    assert!(
        matches!(err, CredentialsError::Parse { .. }),
        "expected Parse error, got {err:?}"
    );
}

/// Regression: the previous `std::fs::write` then `set_permissions`
/// flow exposed the token at the default umask between the two calls.
/// `OpenOptions::mode(0o600)` removes that window. Verify the file is
/// `0o600` after write on unix.
#[cfg(unix)]
#[test]
fn write_to_sets_mode_0600_on_unix() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("c.json");
    let mut creds = Credentials::default();
    creds.set("https://example.com", entry("pakx_v1_aaa", "alice"));
    creds.write_to(&path).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    // Mask the file-type bits — only the low 9 bits are perms.
    assert_eq!(
        mode & 0o777,
        0o600,
        "credentials.json must be 0600 on unix, got {:o}",
        mode & 0o777,
    );
}

/// The tmp file written under `.tmp` must be cleaned up by `rename`.
/// Verify that after a successful write the only artefact on disk is
/// the final file — no stale `credentials.json.tmp` lying around.
#[test]
fn write_to_leaves_no_tmp_artifact_on_success() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("c.json");
    let mut creds = Credentials::default();
    creds.set("https://example.com", entry("t", "a"));
    creds.write_to(&path).unwrap();
    assert!(path.is_file());
    assert!(
        !temp.path().join("c.json.tmp").exists(),
        "tmp must be renamed away on success"
    );
}

/// Round-47 regression: the retry-once-on-`AlreadyExists` path used to
/// reopen the same predictable `<path>.tmp`. That left a race window
/// where a co-process could slip a tmp file in between our unlink and
/// reopen, and we'd write our token bytes into a file the racer owned
/// — then `rename` it into place as `credentials.json`. The fix uses
/// a `<path>.tmp.<pid>.<nanos>` retry suffix the racer cannot predict.
///
/// Pin the new shape: after a successful retry (predictable tmp
/// pre-planted, unlinked-and-replaced under a random suffix, then
/// renamed away), there must be **no** `<path>.tmp.*` artefact left on
/// disk — the rename clears it, and the random portion guarantees the
/// retry path was distinct from the predictable one we pre-planted.
#[test]
fn write_to_retry_uses_unpredictable_tmp_shape() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("c.json");
    let tmp_predictable = temp.path().join("c.json.tmp");

    // Plant a stale predictable tmp — the retry path fires.
    std::fs::write(&tmp_predictable, b"stale").unwrap();
    assert!(tmp_predictable.exists());

    let mut creds = Credentials::default();
    creds.set("https://example.com", entry("pakx_v1_aaa", "alice"));
    creds.write_to(&path).unwrap();

    // Predictable tmp is gone (unlinked by the AlreadyExists branch).
    assert!(
        !tmp_predictable.exists(),
        "predictable .tmp must be unlinked when the retry path fires",
    );
    // No randomised `<path>.tmp.<pid>.<nanos>` artefacts either: the
    // retry rename swept them. Scan the dir for anything starting
    // with `c.json.tmp` and assert zero hits.
    let leftover: Vec<_> = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .filter(|n| {
            n.to_string_lossy().starts_with(&format!(
                "{}.tmp",
                path.file_name().unwrap().to_string_lossy()
            ))
        })
        .collect();
    assert!(
        leftover.is_empty(),
        "no tmp artefacts must survive a successful retry, got: {leftover:?}",
    );
    assert!(path.is_file());
}

/// Regression: `OpenOptions::mode(0o600)` is **ignored on existing
/// files**. If a stale `credentials.json.tmp` was left behind by a prior
/// crash (or pre-planted by a co-process) at the default umask, the old
/// `.create(true).truncate(true)` path reused that mode and `rename`
/// installed `credentials.json` at the wrong permission bits. The fix
/// is `.create_new(true)` + unlink-and-retry-once on `AlreadyExists`,
/// which guarantees the file is created fresh under our requested mode.
///
/// Pre-create `<path>.tmp` at `0o644`, write, and assert the post-write
/// `credentials.json` is `0o600` on unix.
#[cfg(unix)]
#[test]
fn write_to_overwrites_pre_planted_tmp_at_wrong_mode() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("c.json");
    let tmp_path = temp.path().join("c.json.tmp");

    // Plant a stale `.tmp` at the wrong (group/world-readable) mode —
    // simulating a prior crash or a hostile co-process.
    std::fs::write(&tmp_path, b"stale garbage from a prior crash").unwrap();
    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        std::fs::metadata(&tmp_path).unwrap().permissions().mode() & 0o777,
        0o644,
        "test setup: stale tmp must start at 0o644",
    );

    let mut creds = Credentials::default();
    creds.set("https://example.com", entry("pakx_v1_aaa", "alice"));
    creds.write_to(&path).unwrap();

    // The stale tmp must have been unlinked and replaced. The final
    // file is created fresh, so the mode is the one we requested.
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "credentials.json must be 0600 even when a stale tmp pre-existed at 0o644, got {:o}",
        mode & 0o777,
    );
    // No tmp leftover after a successful rename.
    assert!(
        !tmp_path.exists(),
        "stale tmp must be cleared after success"
    );
}
