//! Federated MCP dependency resolution.
//!
//! Shared by `pakx test` and `pakx install`. The headline contract: a
//! manifest entry resolves through every registered source, not just
//! the official MCP Registry. README + CHANGELOG sell federated
//! resolution as the marquee feature for both commands, and the
//! `--no-smithery` / `--no-pakx-registry` flags need to actually do
//! something.
//!
//! Strategy:
//!   1. Try `OfficialMcpSource::fetch` first. The MCP Registry is the
//!      canonical source for upstream MCP servers; if a direct fetch
//!      hits, that's the right answer with the right version pin.
//!   2. On `NotFound`, fan a `client.search(&id)` across every other
//!      source and pick the package whose canonical `id` equals the
//!      query. Smithery and pakx-registry both expose search but
//!      surface canonical ids the same way `pakx search` already
//!      consumes. The first exact match wins.
//!   3. Anything else (network error, decode failure) is surfaced
//!      verbatim — those aren't "not in federated registry," they're
//!      bugs we want loud.
//!
//! This intentionally does NOT do parallel federated fetches against
//! every source — that would multiply the number of requests per dep
//! by the source count, and the search-based fallback gets us the
//! same result with one extra round-trip in the worst case.

use pakx_core::RegistrySource;
use pakx_registry_client::{Package, RegistryClient, RegistryError};

/// Result of a federated resolution attempt for one MCP id.
#[derive(Debug)]
pub enum Resolved {
    /// Found via direct fetch against the official MCP Registry.
    OfficialMcp(Package),
    /// Found via a federated search exact-name match. The
    /// [`RegistrySource`] tag tells the caller which source claimed
    /// the package so it can be recorded in `agents.lock`.
    Federated(Package),
    /// No source returned a match. Includes the underlying error
    /// from the official-MCP fetch (typically `NotFound`) for
    /// diagnostic purposes.
    NotFound,
}

impl Resolved {
    /// The package payload regardless of which path produced it.
    /// Used by tests; callers match on the variant directly in
    /// production code so they can also access the source tag.
    #[cfg(test)]
    pub const fn package(&self) -> Option<&Package> {
        match self {
            Self::OfficialMcp(p) | Self::Federated(p) => Some(p),
            Self::NotFound => None,
        }
    }

    /// Which registry produced this match. `None` for `NotFound`.
    #[cfg(test)]
    pub fn source(&self) -> Option<RegistrySource> {
        self.package().map(|p| p.source)
    }
}

/// Resolve `id` against every registered source. See module docs for
/// the strategy.
///
/// Non-`NotFound` errors from the initial `OfficialMcp` fetch (HTTP,
/// decode, etc.) are returned to the caller — they aren't a federated
/// fallback signal.
pub async fn resolve_federated(
    client: &RegistryClient,
    id: &str,
) -> Result<Resolved, RegistryError> {
    match client.fetch(RegistrySource::OfficialMcp, id).await {
        Ok(pkg) => Ok(Resolved::OfficialMcp(pkg)),
        Err(RegistryError::NotFound { .. }) => {
            // Federated search across every registered source **except**
            // `OfficialMcp` — the direct fetch above already disagreed,
            // and the search index can lag the canonical fetch route, so
            // trusting a stale search hit would re-introduce the search-
            // lag race. Filtering the fan-out also saves one HTTP
            // round-trip per resolved dep, which adds up at install
            // time on manifests with many entries.
            //
            // `RegistryClient::search_excluding` swallows per-source
            // failures and logs them, so partial results win — the
            // caller still gets a non-None match if any other source
            // surfaced the id.
            let hits = client
                .search_excluding(id, Some(RegistrySource::OfficialMcp))
                .await;
            for pkg in hits {
                if pkg.id == id || pkg.name == id {
                    return Ok(Resolved::Federated(pkg));
                }
            }
            Ok(Resolved::NotFound)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pakx_registry_client::Source;
    use serde_json::json;

    /// Test double: returns whatever package the test seeds.
    struct StubSource {
        tag: RegistrySource,
        fetch_result: Option<Package>,
        search_result: Vec<Package>,
    }

    #[async_trait]
    impl Source for StubSource {
        fn tag(&self) -> RegistrySource {
            self.tag
        }
        async fn search(&self, _query: &str) -> Result<Vec<Package>, RegistryError> {
            Ok(self.search_result.clone())
        }
        async fn fetch(&self, id: &str) -> Result<Package, RegistryError> {
            self.fetch_result
                .clone()
                .filter(|p| p.id == id)
                .ok_or(RegistryError::NotFound {
                    source_tag: "stub",
                    id: id.to_owned(),
                })
        }
    }

    fn pkg(id: &str, source: RegistrySource, version: &str) -> Package {
        Package {
            id: id.to_owned(),
            source,
            name: id.to_owned(),
            version: version.to_owned(),
            description: None,
            kind: None,
            install_hints: json!({}),
        }
    }

    #[tokio::test]
    async fn returns_official_when_fetch_hits() {
        let official = StubSource {
            tag: RegistrySource::OfficialMcp,
            fetch_result: Some(pkg("a/b", RegistrySource::OfficialMcp, "1.0.0")),
            search_result: vec![],
        };
        let smithery = StubSource {
            tag: RegistrySource::Smithery,
            fetch_result: None,
            search_result: vec![pkg("a/b", RegistrySource::Smithery, "latest")],
        };
        let client = RegistryClient::new()
            .with_source(Box::new(official))
            .with_source(Box::new(smithery));
        let r = resolve_federated(&client, "a/b").await.unwrap();
        assert!(matches!(r, Resolved::OfficialMcp(_)));
        assert_eq!(r.source(), Some(RegistrySource::OfficialMcp));
    }

    #[tokio::test]
    async fn falls_back_to_smithery_on_official_not_found() {
        let official = StubSource {
            tag: RegistrySource::OfficialMcp,
            fetch_result: None,
            search_result: vec![],
        };
        let smithery = StubSource {
            tag: RegistrySource::Smithery,
            fetch_result: None,
            search_result: vec![pkg("a/b", RegistrySource::Smithery, "latest")],
        };
        let client = RegistryClient::new()
            .with_source(Box::new(official))
            .with_source(Box::new(smithery));
        let r = resolve_federated(&client, "a/b").await.unwrap();
        assert!(matches!(r, Resolved::Federated(_)));
        assert_eq!(r.source(), Some(RegistrySource::Smithery));
    }

    #[tokio::test]
    async fn falls_back_to_pakx_when_smithery_misses() {
        let official = StubSource {
            tag: RegistrySource::OfficialMcp,
            fetch_result: None,
            search_result: vec![],
        };
        let smithery = StubSource {
            tag: RegistrySource::Smithery,
            fetch_result: None,
            search_result: vec![pkg("other/server", RegistrySource::Smithery, "latest")],
        };
        let pakx = StubSource {
            tag: RegistrySource::Pakx,
            fetch_result: None,
            search_result: vec![pkg("a/b", RegistrySource::Pakx, "1.0.0")],
        };
        let client = RegistryClient::new()
            .with_source(Box::new(official))
            .with_source(Box::new(smithery))
            .with_source(Box::new(pakx));
        let r = resolve_federated(&client, "a/b").await.unwrap();
        assert_eq!(r.source(), Some(RegistrySource::Pakx));
    }

    #[tokio::test]
    async fn not_found_when_no_source_returns_match() {
        let official = StubSource {
            tag: RegistrySource::OfficialMcp,
            fetch_result: None,
            search_result: vec![],
        };
        let smithery = StubSource {
            tag: RegistrySource::Smithery,
            fetch_result: None,
            search_result: vec![],
        };
        let client = RegistryClient::new()
            .with_source(Box::new(official))
            .with_source(Box::new(smithery));
        let r = resolve_federated(&client, "ghost/server").await.unwrap();
        assert!(matches!(r, Resolved::NotFound));
    }

    /// A federated search match against `OfficialMcp` must NOT count —
    /// we already called `fetch` and got `NotFound`. Trusting a
    /// stale search index over the canonical fetch would re-introduce
    /// the "search lag" race.
    #[tokio::test]
    async fn ignores_official_mcp_search_hit_after_fetch_miss() {
        let official = StubSource {
            tag: RegistrySource::OfficialMcp,
            fetch_result: None,
            search_result: vec![pkg("a/b", RegistrySource::OfficialMcp, "0.0.0")],
        };
        let client = RegistryClient::new().with_source(Box::new(official));
        let r = resolve_federated(&client, "a/b").await.unwrap();
        assert!(matches!(r, Resolved::NotFound));
    }
}
