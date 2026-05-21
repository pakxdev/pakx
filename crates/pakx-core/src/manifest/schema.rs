//! Strongly-typed representation of `agents.yml`.
//!
//! The manifest is the single source of truth for what gets installed across
//! every detected agent. Schema mirrors the master prompt spec verbatim.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Known package types installable across agents. Used as keys in
/// `dependencies` (YAML) and as the `type` discriminator in lockfile entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageType {
    Skills,
    Mcp,
    Subagents,
    Prompts,
    Commands,
    Hooks,
}

impl PackageType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Mcp => "mcp",
            Self::Subagents => "subagents",
            Self::Prompts => "prompts",
            Self::Commands => "commands",
            Self::Hooks => "hooks",
        }
    }
}

/// All package type variants in canonical order. Use this for deterministic
/// iteration in writers, doctor reports, etc.
pub const PACKAGE_TYPES: [PackageType; 6] = [
    PackageType::Skills,
    PackageType::Mcp,
    PackageType::Subagents,
    PackageType::Prompts,
    PackageType::Commands,
    PackageType::Hooks,
];

/// Known built-in agent ids. The wire type is still `String`/`AgentId`, so
/// adding a new adapter requires no schema bump.
pub const KNOWN_AGENT_IDS: &[&str] = &["claude-code", "cursor", "codex", "copilot", "windsurf"];

static AGENT_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9-]*$").expect("static regex compiles"));

/// Validated agent identifier (lowercase kebab-case).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    /// Construct without validation. Intended for trusted inputs (constants,
    /// adapter self-registration). Untrusted strings should go through
    /// [`AgentId::parse`].
    pub fn new_unchecked(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Validate and wrap an agent id string.
    pub fn parse(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        if AGENT_ID_RE.is_match(&s) {
            Ok(Self(s))
        } else {
            Err(format!(
                "invalid agent id {s:?}: must be lowercase kebab-case starting with a letter"
            ))
        }
    }

    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for AgentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Dependency specs
// ---------------------------------------------------------------------------

/// Shorthand id string like `owner/name@^1.0` or `acme/skill`.
/// Validated at deserialization: no whitespace, non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct StringSpec(String);

impl StringSpec {
    pub fn parse(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        if s.is_empty() {
            return Err("dep shorthand must not be empty".into());
        }
        if s.chars().any(char::is_whitespace) {
            return Err(format!(
                "dep shorthand {s:?} contains whitespace; use the object form for git/registry"
            ));
        }
        Ok(Self(s))
    }

    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for StringSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(s).map_err(serde::de::Error::custom)
    }
}

/// Git-sourced dep: `{ git: "https://...", ref: "v1.3.0" }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSpec {
    pub git: String,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
}

/// Registry-explicit dep: `{ registry: "official", name: "filesystem", args: [...] }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySpec {
    pub registry: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}

/// A single dep entry inside a `dependencies.<type>` list. Accepts all three
/// forms from the spec: bare string, git object, or registry object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DepSpec {
    String(StringSpec),
    Git(GitSpec),
    Registry(RegistrySpec),
}

// ---------------------------------------------------------------------------
// Dependencies + Manifest
// ---------------------------------------------------------------------------

/// All declared dependencies grouped by package type. Every field is
/// optional in the YAML source; missing fields deserialize to `None`.
/// Empty arrays are skipped on serialization.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependencies {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<DepSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<Vec<DepSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<Vec<DepSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<DepSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<DepSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Vec<DepSpec>>,
}

impl Dependencies {
    pub const fn get(&self, kind: PackageType) -> Option<&Vec<DepSpec>> {
        match kind {
            PackageType::Skills => self.skills.as_ref(),
            PackageType::Mcp => self.mcp.as_ref(),
            PackageType::Subagents => self.subagents.as_ref(),
            PackageType::Prompts => self.prompts.as_ref(),
            PackageType::Commands => self.commands.as_ref(),
            PackageType::Hooks => self.hooks.as_ref(),
        }
    }
}

/// Project-level manifest persisted as `agents.yml`. Field order here is
/// also the canonical write order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<AgentId>>,
    #[serde(default, skip_serializing_if = "is_empty_dependencies")]
    pub dependencies: Dependencies,
}

const fn is_empty_dependencies(d: &Dependencies) -> bool {
    d.skills.is_none()
        && d.mcp.is_none()
        && d.subagents.is_none()
        && d.prompts.is_none()
        && d.commands.is_none()
        && d.hooks.is_none()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl DepSpec {
    /// Convenience: the textual "name" hint a user would recognise, for
    /// log lines / conflict messages. Not a stable identity.
    pub fn display_hint(&self) -> Cow<'_, str> {
        match self {
            Self::String(s) => Cow::Borrowed(s.as_str()),
            Self::Git(g) => Cow::Owned(format!("git:{}", g.git)),
            Self::Registry(r) => Cow::Owned(format!("{}/{}", r.registry, r.name)),
        }
    }
}
