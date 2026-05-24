//! Claude Code adapter.
//!
//! - Skills install under `<config_dir>/skills/<owner>-<name>/` (default
//!   `~/.claude/skills/...`). The single-level dash-separated leaf
//!   matches the install runner (`crates/pakx/src/install/skill.rs` +
//!   `install/bundle.rs`) and Claude Code's organic "one flat dir per
//!   skill, no version subdir" convention. A previous two-level
//!   `<owner>/<name>` layout in this module silently diverged from the
//!   installer, causing `Adapter::list()` to miss every installer-side
//!   skill and `pakx list` / `pakx doctor` to report them as drift.
//! - MCP servers are written into `<project_root>/.mcp.json` — Claude
//!   Code's project-scoped MCP convention. `project_root` defaults to the
//!   process cwd, but tests and callers pass an explicit path.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use pakx_core::install::compute_integrity;
use pakx_core::manifest::PackageType;
use pakx_core::{atomic_write, McpServer, McpTransport, Skill};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::adapter::{Adapter, Installed};
use crate::error::AdapterError;

/// Filename of Claude Code's project-scoped MCP config.
const MCP_FILENAME: &str = ".mcp.json";

/// Adapter for Anthropic's Claude Code CLI.
///
/// Detection is `<config_dir>.is_dir()` against `~/.claude` by default.
/// Tests construct with [`Self::with_config_dir`] to point at a temp tree.
#[derive(Debug, Clone)]
pub struct ClaudeCodeAdapter {
    config_dir: PathBuf,
    project_root: PathBuf,
}

impl ClaudeCodeAdapter {
    pub const ID: &'static str = "claude-code";

    /// Construct against `~/.claude` + the process cwd as project root.
    /// Returns `None` if either cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = dirs::home_dir()?;
        let cwd = std::env::current_dir().ok()?;
        Some(Self {
            config_dir: home.join(".claude"),
            project_root: cwd,
        })
    }

    /// Explicit config-dir constructor. Project root defaults to `.`.
    #[must_use]
    pub fn with_config_dir(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
            project_root: PathBuf::from("."),
        }
    }

    /// Builder: override the project root used for project-scoped writes
    /// (currently `.mcp.json`).
    #[must_use]
    pub fn with_project_root(mut self, project_root: impl Into<PathBuf>) -> Self {
        self.project_root = project_root.into();
        self
    }

    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    fn skills_root(&self) -> PathBuf {
        self.config_dir.join("skills")
    }

    /// Canonical on-disk path for a skill: `<skills_root>/<owner>-<name>/`.
    ///
    /// The single-level dash-separated leaf mirrors the install
    /// runner (`install::skill::install_skill_from_pakx` →
    /// `claude_home.join("skills").join(format!("{owner}-{name}"))`) so
    /// the adapter's `list` / `uninstall` paths see exactly what the
    /// installer wrote. Diverging from the installer (e.g. a two-level
    /// `<owner>/<name>` tree) silently breaks `pakx list` + `pakx
    /// doctor` for every Phase B skill.
    fn skill_dir(&self, owner: &str, name: &str) -> PathBuf {
        self.skills_root().join(format!("{owner}-{name}"))
    }

    fn mcp_config_path(&self) -> PathBuf {
        self.project_root.join(MCP_FILENAME)
    }
}

#[async_trait]
impl Adapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    async fn install_skill(&self, skill: &Skill) -> Result<Installed, AdapterError> {
        // Integrity gate: skip work entirely if the payload doesn't match
        // its declared digest. Tampering or registry inconsistency.
        let computed = compute_integrity(&skill.files);
        if computed != skill.integrity {
            return Err(AdapterError::IntegrityMismatch {
                id: skill.id(),
                expected: skill.integrity.as_str().to_owned(),
                computed: computed.as_str().to_owned(),
            });
        }

        // Path traversal guard: refuse any relative_path that resolves outside
        // its skill_dir.
        validate_skill_files(skill)?;

        let dst_root = self.skill_dir(&skill.owner, &skill.name);

        // Idempotency: if the on-disk integrity (recomputed from files on
        // disk) equals the payload's declared integrity, nothing to do.
        if dst_root.is_dir() {
            if let Some(existing) = read_skill_integrity(&dst_root).await? {
                if existing == skill.integrity {
                    return Err(AdapterError::AlreadyInstalled { id: skill.id() });
                }
            }
            // Drift: remove the old install before rewriting.
            fs::remove_dir_all(&dst_root)
                .await
                .map_err(|e| AdapterError::Io {
                    source: e,
                    path: Some(dst_root.clone()),
                })?;
        }

        for f in &skill.files {
            write_file_atomic(&dst_root, &f.relative_path, &f.contents).await?;
        }

        Ok(Installed {
            id: skill.id(),
            kind: PackageType::Skills,
            version: skill.version.clone(),
            path: dst_root,
        })
    }

    async fn install_mcp(&self, mcp: &McpServer) -> Result<Installed, AdapterError> {
        let path = self.mcp_config_path();
        let mut file = read_mcp_config(&path).await?;

        let key = mcp.short_name();
        let new_entry = McpEntry::from_transport(&mcp.transport);

        if let Some(existing) = file.mcp_servers.get(&key) {
            if existing == &new_entry {
                return Err(AdapterError::AlreadyInstalled { id: mcp.id.clone() });
            }
        }

        file.mcp_servers.insert(key, new_entry);
        write_mcp_config(&path, &file).await?;

        Ok(Installed {
            id: mcp.id.clone(),
            kind: PackageType::Mcp,
            version: mcp.version.clone(),
            path,
        })
    }

    async fn uninstall(&self, id: &str) -> Result<(), AdapterError> {
        // Try skill removal first; fall through to MCP if no skill dir.
        if let Some((owner, name)) = id.split_once('/') {
            let target = self.skill_dir(owner, name);
            if target.is_dir() {
                fs::remove_dir_all(&target)
                    .await
                    .map_err(|e| AdapterError::Io {
                        source: e,
                        path: Some(target),
                    })?;
                return Ok(());
            }
        } else {
            return Err(AdapterError::Invalid {
                id: id.to_owned(),
                reason: "id must be `<owner>/<name>`".into(),
            });
        }

        // MCP fallback: strip from `.mcp.json` by short-name.
        let key = id.rsplit('/').next().unwrap_or(id).to_lowercase();
        let path = self.mcp_config_path();
        if path.is_file() {
            let mut file = read_mcp_config(&path).await?;
            if file.mcp_servers.remove(&key).is_some() {
                write_mcp_config(&path, &file).await?;
                return Ok(());
            }
        }

        Err(AdapterError::NotInstalled { id: id.to_owned() })
    }

    /// Enumerate every installed skill under `<skills_root>/`.
    ///
    /// Walks one level deep — each entry is a `<owner>-<name>/`
    /// directory matching the installer's layout (see
    /// [`Self::skill_dir`]). Non-conforming names (no `-` separator) are
    /// surfaced verbatim as `id = <dir_name>` so a hand-rolled / drifted
    /// install still shows up in `pakx list` instead of disappearing.
    async fn list(&self) -> Result<Vec<Installed>, AdapterError> {
        let root = self.skills_root();
        if !root.is_dir() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        let mut entries = fs::read_dir(&root).await.map_err(|e| AdapterError::Io {
            source: e,
            path: Some(root.clone()),
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| AdapterError::Io {
            source: e,
            path: Some(root.clone()),
        })? {
            if !entry.file_type().await.is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            // Restore the canonical `<owner>/<name>` id from the
            // dash-flattened on-disk name. The installer always writes
            // `<owner>-<name>` (with exactly one separator joining the
            // two halves of the registry id), so splitting on the FIRST
            // `-` recovers the original split — even when the `<name>`
            // itself contains additional dashes (e.g.
            // `arwenizer/hello-world` → `arwenizer-hello-world` →
            // `arwenizer/hello-world`).
            let id = dir_name
                .split_once('-')
                .map_or_else(|| dir_name.clone(), |(o, n)| format!("{o}/{n}"));
            let version = read_skill_version(&path)
                .await
                .unwrap_or_else(|| "unknown".into());
            out.push(Installed {
                id,
                kind: PackageType::Skills,
                version,
                path,
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Reject `..`, absolute paths, and empty/Windows-drive components.
fn validate_skill_files(skill: &Skill) -> Result<(), AdapterError> {
    if skill.files.is_empty() {
        return Err(AdapterError::Invalid {
            id: skill.id(),
            reason: "skill has no files".into(),
        });
    }
    for f in &skill.files {
        let p = Path::new(&f.relative_path);
        if p.is_absolute() {
            return Err(AdapterError::Invalid {
                id: skill.id(),
                reason: format!("file path {:?} is absolute", f.relative_path),
            });
        }
        for component in p.components() {
            use std::path::Component;
            match component {
                Component::ParentDir => {
                    return Err(AdapterError::Invalid {
                        id: skill.id(),
                        reason: format!("file path {:?} contains `..`", f.relative_path),
                    });
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(AdapterError::Invalid {
                        id: skill.id(),
                        reason: format!(
                            "file path {:?} contains a root or drive prefix",
                            f.relative_path
                        ),
                    });
                }
                Component::CurDir | Component::Normal(_) => {}
            }
        }
    }
    Ok(())
}

/// Write `contents` to `<dst_root>/<relative_path>` atomically: temp file in
/// the same directory + rename. Creates parent dirs as needed.
async fn write_file_atomic(
    dst_root: &Path,
    relative_path: &str,
    contents: &[u8],
) -> Result<(), AdapterError> {
    let target = dst_root.join(relative_path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| AdapterError::Io {
                source: e,
                path: Some(parent.to_path_buf()),
            })?;
    }

    // Sync tempfile (no async API exposed). Done off-thread to avoid blocking
    // the executor.
    let target_clone = target.clone();
    let bytes = contents.to_vec();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let parent = target_clone
            .parent()
            .expect("parent created above; cannot be root");
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        std::io::Write::write_all(&mut tmp, &bytes)?;
        tmp.persist(&target_clone).map_err(|e| e.error)?;
        Ok(())
    })
    .await
    .map_err(|join_err| AdapterError::Io {
        source: io::Error::other(join_err),
        path: Some(target.clone()),
    })?
    .map_err(|e| AdapterError::Io {
        source: e,
        path: Some(target),
    })
}

/// Read every regular file under `skill_dir` (sorted), build a synthetic
/// `Vec<SkillFile>`, and recompute its integrity. Returns None if the dir
/// contains nothing readable.
async fn read_skill_integrity(
    skill_dir: &Path,
) -> Result<Option<pakx_core::Integrity>, AdapterError> {
    let files = collect_files(skill_dir).await?;
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(compute_integrity(&files)))
}

async fn collect_files(skill_dir: &Path) -> Result<Vec<pakx_core::SkillFile>, AdapterError> {
    let mut out = Vec::new();
    let mut stack = vec![skill_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = fs::read_dir(&dir).await.map_err(|e| AdapterError::Io {
            source: e,
            path: Some(dir.clone()),
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| AdapterError::Io {
            source: e,
            path: Some(dir.clone()),
        })? {
            let path = entry.path();
            let ft = entry.file_type().await.map_err(|e| AdapterError::Io {
                source: e,
                path: Some(path.clone()),
            })?;
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                let relative = path
                    .strip_prefix(skill_dir)
                    .expect("path is under skill_dir")
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = fs::read(&path).await.map_err(|e| AdapterError::Io {
                    source: e,
                    path: Some(path.clone()),
                })?;
                out.push(pakx_core::SkillFile {
                    relative_path: relative,
                    contents: bytes,
                });
            }
        }
    }
    Ok(out)
}

/// Best-effort version read from a `SKILL.md` frontmatter `version:` line.
/// Returns None when no such file or no parseable version exists.
async fn read_skill_version(skill_dir: &Path) -> Option<String> {
    let candidate = skill_dir.join("SKILL.md");
    let text = fs::read_to_string(&candidate).await.ok()?;
    for line in text.lines().take(50) {
        if let Some(rest) = line.strip_prefix("version:") {
            return Some(
                rest.trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_owned(),
            );
        }
    }
    None
}

// ---------------------------------------------------------------------------
// .mcp.json schema (Claude Code project-scoped MCP config)
// ---------------------------------------------------------------------------

/// On-disk shape of Claude Code's `.mcp.json`. We model just `mcpServers`
/// and pass everything else through unchanged via [`serde_json::Value`] in
/// [`McpConfigFile::extra`] so non-pakx fields survive round-trips.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct McpConfigFile {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: BTreeMap<String, McpEntry>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum McpEntry {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

impl McpEntry {
    fn from_transport(t: &McpTransport) -> Self {
        match t.clone() {
            McpTransport::Stdio { command, args, env } => Self::Stdio { command, args, env },
            McpTransport::Http { url, headers } => Self::Http { url, headers },
        }
    }
}

async fn read_mcp_config(path: &Path) -> Result<McpConfigFile, AdapterError> {
    match fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| AdapterError::Invalid {
            id: path.display().to_string(),
            reason: format!("malformed .mcp.json: {e}"),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(McpConfigFile::default()),
        Err(source) => Err(AdapterError::Io {
            source,
            path: Some(path.to_path_buf()),
        }),
    }
}

async fn write_mcp_config(path: &Path, file: &McpConfigFile) -> Result<(), AdapterError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| AdapterError::Io {
                    source: e,
                    path: Some(parent.to_path_buf()),
                })?;
        }
    }
    let mut body = serde_json::to_string_pretty(file).map_err(|e| AdapterError::Io {
        source: io::Error::other(e),
        path: Some(path.to_path_buf()),
    })?;
    body.push('\n');
    // Route the `.mcp.json` write through `atomic_write` so a crash
    // mid-flush cannot leave a half-serialised JSON file on disk —
    // matches the discipline applied to `agents.lock`, `agents.yml`,
    // and the federated cache. `atomic_write` is sync, so wrap in
    // `spawn_blocking` to keep the async caller non-blocking.
    let path_buf = path.to_path_buf();
    let bytes = body.into_bytes();
    tokio::task::spawn_blocking(move || atomic_write(&path_buf, &bytes))
        .await
        .map_err(|e| AdapterError::Io {
            source: io::Error::other(e),
            path: Some(path.to_path_buf()),
        })?
        .map_err(|e| AdapterError::Io {
            source: e,
            path: Some(path.to_path_buf()),
        })
}
