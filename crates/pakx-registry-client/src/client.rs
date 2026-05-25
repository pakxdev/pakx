//! Top-level [`RegistryClient`] that fans queries out across every
//! registered [`Source`] and merges results.

use futures::future::join_all;
use pakx_core::RegistrySource;
use tracing::warn;

use crate::errors::RegistryError;
use crate::source::Source;
use crate::types::Package;

pub struct RegistryClient {
    sources: Vec<Box<dyn Source>>,
}

impl RegistryClient {
    /// Construct an empty client. Add sources via [`Self::with_source`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// Builder-style: register one source.
    #[must_use]
    pub fn with_source(mut self, source: Box<dyn Source>) -> Self {
        self.sources.push(source);
        self
    }

    /// How many sources are registered.
    #[must_use]
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Fan a free-text search across every registered source in parallel,
    /// merge results, and dedupe by `(source, id)`. Per-source failures
    /// are logged (`tracing::warn`) and dropped — partial results win.
    pub async fn search(&self, query: &str) -> Vec<Package> {
        self.search_excluding(query, None).await
    }

    /// Like [`Self::search`], but constrain results to a single package
    /// `kind` (the canonical plural token: `skills` / `mcp` / `subagents`
    /// / `prompts` / `commands` / `hooks`).
    ///
    /// Two-layer filter so every source behaves correctly under `--kind`:
    ///
    /// - **Server-side** for the first-party pakx-registry: the kind is
    ///   forwarded as `?kind=<kind>` via [`Source::search_kind`], so the
    ///   registry returns only matching packages (round 24 filter).
    /// - **Client-side** for federated sources with no kind discriminator
    ///   (Smithery, official MCP Registry): their hits arrive with
    ///   `Package::kind == None`. Because both upstreams list MCP servers
    ///   exclusively, `--kind mcp` keeps them and any other `--kind`
    ///   value drops them. This is the single honest interpretation that
    ///   never fabricates a kind on the [`Package`] itself.
    ///
    /// `kind == None` is identical to [`Self::search`].
    pub async fn search_kind(&self, query: &str, kind: Option<&str>) -> Vec<Package> {
        let Some(kind) = kind else {
            return self.search(query).await;
        };
        let futures = self.sources.iter().map(|s| async move {
            let tag = s.tag();
            (tag, s.search_kind(query, Some(kind)).await)
        });
        let results: Vec<(RegistrySource, Result<Vec<Package>, RegistryError>)> =
            join_all(futures).await;

        let mut out: Vec<Package> = Vec::new();
        for (tag, res) in results {
            match res {
                Ok(packages) => out.extend(packages),
                Err(e) => {
                    warn!(target: "pakx::registry", source = ?tag, error = %e, "source search_kind failed");
                }
            }
        }
        out.retain(|pkg| kind_matches(pkg, kind));
        dedupe_by_source_id(out)
    }

    /// Same as [`Self::search`], but skip the source matching `exclude`.
    ///
    /// Used by the federated-resolution fallback in `pakx install` /
    /// `pakx test`: when `OfficialMcp::fetch` already returned
    /// `NotFound`, fanning the search to `OfficialMcp` is a wasted
    /// round-trip — the resolver discards its hits anyway because the
    /// canonical fetch route already disagreed. Passing
    /// `Some(RegistrySource::OfficialMcp)` saves one HTTP request per
    /// resolved dep.
    pub async fn search_excluding(
        &self,
        query: &str,
        exclude: Option<RegistrySource>,
    ) -> Vec<Package> {
        let futures = self
            .sources
            .iter()
            .filter(|s| exclude.is_none_or(|tag| s.tag() != tag))
            .map(|s| async move {
                let tag = s.tag();
                (tag, s.search(query).await)
            });
        let results: Vec<(RegistrySource, Result<Vec<Package>, RegistryError>)> =
            join_all(futures).await;

        let mut out: Vec<Package> = Vec::new();
        for (tag, res) in results {
            match res {
                Ok(packages) => out.extend(packages),
                Err(e) => {
                    warn!(target: "pakx::registry", source = ?tag, error = %e, "source search failed");
                }
            }
        }
        dedupe_by_source_id(out)
    }

    /// Fetch a package by `(source, id)`. Returns `NotFound` if no source
    /// matching `tag` is registered, or whatever the source returns.
    pub async fn fetch(&self, tag: RegistrySource, id: &str) -> Result<Package, RegistryError> {
        for source in &self.sources {
            if source.tag() == tag {
                return source.fetch(id).await;
            }
        }
        Err(RegistryError::NotFound {
            source_tag: tag_to_static_str(tag),
            id: id.to_owned(),
        })
    }
}

impl Default for RegistryClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Decide whether `pkg` matches the requested `kind` token.
///
/// `requested` is the canonical plural CLI token (`skills` / `mcp` / ...).
/// The package's own [`Package::kind`] is the raw source string, which the
/// pakx-registry historically emits in the **singular** (`"skill"`), so we
/// normalize both sides through [`normalize_kind`] before comparing.
///
/// A package with `kind == None` (every federated, no-kind-concept source
/// — Smithery, official MCP Registry) matches **only** `mcp`, since both
/// upstreams list MCP servers exclusively. This keeps `--kind mcp` honest
/// without fabricating a kind on the `Package` struct.
fn kind_matches(pkg: &Package, requested: &str) -> bool {
    let want = normalize_kind(requested);
    pkg.kind
        .as_deref()
        .map_or_else(|| want == "mcp", |k| normalize_kind(k) == want)
}

/// Fold a kind token (singular or plural, any case) onto the canonical
/// plural CLI form. Unknown tokens pass through lowercased so an
/// unexpected future kind still compares deterministically rather than
/// silently collapsing onto a known variant.
fn normalize_kind(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "skill" | "skills" => "skills".to_owned(),
        "mcp" => "mcp".to_owned(),
        "subagent" | "subagents" => "subagents".to_owned(),
        "prompt" | "prompts" => "prompts".to_owned(),
        "command" | "commands" => "commands".to_owned(),
        "hook" | "hooks" => "hooks".to_owned(),
        other => other.to_owned(),
    }
}

fn dedupe_by_source_id(mut packages: Vec<Package>) -> Vec<Package> {
    packages.sort_by(|a, b| {
        (a.source, a.id.as_str(), a.version.as_str()).cmp(&(
            b.source,
            b.id.as_str(),
            b.version.as_str(),
        ))
    });
    packages.dedup_by(|a, b| a.source == b.source && a.id == b.id);
    packages
}

const fn tag_to_static_str(tag: RegistrySource) -> &'static str {
    match tag {
        RegistrySource::OfficialMcp => "official-mcp",
        RegistrySource::Smithery => "smithery",
        RegistrySource::Glama => "glama",
        RegistrySource::Github => "github",
        RegistrySource::Git => "git",
        RegistrySource::Pakx => "pakx",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// In-memory `Source` that returns a fixed package list. Used to test
    /// the client-side kind filter without spinning up wiremock.
    struct StubSource {
        tag: RegistrySource,
        packages: Vec<Package>,
    }

    #[async_trait]
    impl Source for StubSource {
        fn tag(&self) -> RegistrySource {
            self.tag
        }
        async fn search(&self, _query: &str) -> Result<Vec<Package>, RegistryError> {
            Ok(self.packages.clone())
        }
        async fn fetch(&self, id: &str) -> Result<Package, RegistryError> {
            Err(RegistryError::NotFound {
                source_tag: tag_to_static_str(self.tag),
                id: id.to_owned(),
            })
        }
    }

    fn pkg(source: RegistrySource, id: &str, kind: Option<&str>) -> Package {
        Package {
            id: id.to_owned(),
            source,
            name: id.to_owned(),
            version: "1.0.0".to_owned(),
            description: None,
            kind: kind.map(str::to_owned),
            install_hints: serde_json::Value::Null,
        }
    }

    #[test]
    fn normalize_kind_folds_singular_and_plural() {
        assert_eq!(normalize_kind("skill"), "skills");
        assert_eq!(normalize_kind("skills"), "skills");
        assert_eq!(normalize_kind("MCP"), "mcp");
        assert_eq!(normalize_kind("Subagent"), "subagents");
        // Unknown token survives (lowercased) instead of collapsing.
        assert_eq!(normalize_kind("future-kind"), "future-kind");
    }

    #[test]
    fn kind_matches_handles_singular_registry_kind() {
        // Registry emits singular `"skill"`; CLI requests plural `"skills"`.
        let p = pkg(RegistrySource::Pakx, "acme/one", Some("skill"));
        assert!(kind_matches(&p, "skills"));
        assert!(!kind_matches(&p, "mcp"));
    }

    #[test]
    fn kind_matches_treats_no_kind_as_mcp_only() {
        // Federated source: no kind discriminator ⇒ MCP only.
        let p = pkg(RegistrySource::Smithery, "@acme/srv", None);
        assert!(kind_matches(&p, "mcp"));
        assert!(!kind_matches(&p, "skills"));
        assert!(!kind_matches(&p, "prompts"));
    }

    /// `search_kind(_, Some("skills"))` keeps the pakx skill, drops the
    /// federated (no-kind) hits — the documented `--kind skills` behavior.
    #[tokio::test]
    async fn search_kind_skills_keeps_pakx_skill_drops_federated() {
        let client = RegistryClient::new()
            .with_source(Box::new(StubSource {
                tag: RegistrySource::Pakx,
                packages: vec![
                    pkg(RegistrySource::Pakx, "acme/a-skill", Some("skill")),
                    pkg(RegistrySource::Pakx, "acme/a-mcp", Some("mcp")),
                ],
            }))
            .with_source(Box::new(StubSource {
                tag: RegistrySource::Smithery,
                packages: vec![pkg(RegistrySource::Smithery, "@acme/srv", None)],
            }));

        let hits = client.search_kind("", Some("skills")).await;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "acme/a-skill");
        assert_eq!(hits[0].kind.as_deref(), Some("skill"));
    }

    /// `--kind mcp` keeps BOTH the pakx mcp package AND the federated
    /// no-kind hits (Smithery / official MCP list MCP servers only).
    #[tokio::test]
    async fn search_kind_mcp_keeps_pakx_mcp_and_federated() {
        let client = RegistryClient::new()
            .with_source(Box::new(StubSource {
                tag: RegistrySource::Pakx,
                packages: vec![
                    pkg(RegistrySource::Pakx, "acme/a-skill", Some("skill")),
                    pkg(RegistrySource::Pakx, "acme/a-mcp", Some("mcp")),
                ],
            }))
            .with_source(Box::new(StubSource {
                tag: RegistrySource::OfficialMcp,
                packages: vec![pkg(RegistrySource::OfficialMcp, "io.x/srv", None)],
            }));

        let hits = client.search_kind("", Some("mcp")).await;
        let ids: Vec<&str> = hits.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(hits.len(), 2);
        assert!(ids.contains(&"acme/a-mcp"));
        assert!(ids.contains(&"io.x/srv"));
        assert!(!ids.contains(&"acme/a-skill"));
    }

    /// `kind == None` is identical to plain `search` — no filtering.
    #[tokio::test]
    async fn search_kind_none_is_unfiltered() {
        let client = RegistryClient::new().with_source(Box::new(StubSource {
            tag: RegistrySource::Pakx,
            packages: vec![
                pkg(RegistrySource::Pakx, "acme/a-skill", Some("skill")),
                pkg(RegistrySource::Pakx, "acme/a-mcp", Some("mcp")),
            ],
        }));
        let hits = client.search_kind("", None).await;
        assert_eq!(hits.len(), 2);
    }
}
