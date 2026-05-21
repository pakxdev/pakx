//! Claude Code adapter: skills install under `~/.claude/skills/<owner>/<name>/`.

use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use pakx_core::install::compute_integrity;
use pakx_core::manifest::PackageType;
use pakx_core::Skill;
use tokio::fs;

use crate::adapter::{Adapter, Installed};
use crate::error::AdapterError;

/// Adapter for Anthropic's Claude Code CLI.
///
/// Detection is `<config_dir>.is_dir()` against `~/.claude` by default.
/// Tests construct with [`Self::with_config_dir`] to point at a temp tree.
#[derive(Debug, Clone)]
pub struct ClaudeCodeAdapter {
    config_dir: PathBuf,
}

impl ClaudeCodeAdapter {
    pub const ID: &'static str = "claude-code";

    /// Construct against `~/.claude`. Returns `None` if the platform has
    /// no resolvable home directory.
    #[must_use]
    pub fn new() -> Option<Self> {
        dirs::home_dir().map(|h| Self {
            config_dir: h.join(".claude"),
        })
    }

    /// Explicit config-dir constructor. Use for tests and for users with a
    /// non-default Claude install path.
    #[must_use]
    pub fn with_config_dir(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
        }
    }

    fn skills_root(&self) -> PathBuf {
        self.config_dir.join("skills")
    }

    fn skill_dir(&self, owner: &str, name: &str) -> PathBuf {
        self.skills_root().join(owner).join(name)
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

    async fn uninstall(&self, id: &str) -> Result<(), AdapterError> {
        let Some((owner, name)) = id.split_once('/') else {
            return Err(AdapterError::Invalid {
                id: id.to_owned(),
                reason: "id must be `<owner>/<name>`".into(),
            });
        };
        let target = self.skill_dir(owner, name);
        if !target.is_dir() {
            return Err(AdapterError::NotInstalled { id: id.to_owned() });
        }
        fs::remove_dir_all(&target)
            .await
            .map_err(|e| AdapterError::Io {
                source: e,
                path: Some(target),
            })?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<Installed>, AdapterError> {
        let root = self.skills_root();
        if !root.is_dir() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        let mut owners = fs::read_dir(&root).await.map_err(|e| AdapterError::Io {
            source: e,
            path: Some(root.clone()),
        })?;
        while let Some(owner_entry) = owners.next_entry().await.map_err(|e| AdapterError::Io {
            source: e,
            path: Some(root.clone()),
        })? {
            if !owner_entry.file_type().await.is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let owner = owner_entry.file_name().to_string_lossy().into_owned();
            let owner_path = owner_entry.path();
            let mut names = fs::read_dir(&owner_path)
                .await
                .map_err(|e| AdapterError::Io {
                    source: e,
                    path: Some(owner_path.clone()),
                })?;
            while let Some(name_entry) = names.next_entry().await.map_err(|e| AdapterError::Io {
                source: e,
                path: Some(owner_path.clone()),
            })? {
                if !name_entry.file_type().await.is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let name = name_entry.file_name().to_string_lossy().into_owned();
                let path = name_entry.path();
                let version = read_skill_version(&path)
                    .await
                    .unwrap_or_else(|| "unknown".into());
                out.push(Installed {
                    id: format!("{owner}/{name}"),
                    kind: PackageType::Skills,
                    version,
                    path,
                });
            }
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
