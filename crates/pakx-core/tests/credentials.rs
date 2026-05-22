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
