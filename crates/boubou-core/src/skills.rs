//! Progressive-disclosure skills: Markdown files with a `name`/`description` frontmatter.
//! Only the one-line listing enters the prompt; the body is loaded on `skills(load)`.
//!
//! Lookup order (first match wins): `<workdir>/skills`, `<agent_home>/skills`,
//! `<install_dir>/skills`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

impl Skill {
    /// Parse frontmatter only; the body stays on disk until `load`.
    pub fn from_path(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        let (meta, _) = split_frontmatter(&text).ok_or_else(|| Error::SkillFormat {
            path: path.to_path_buf(),
            reason: "missing `---` frontmatter".into(),
        })?;
        let get = |key: &str| {
            meta.get(key).cloned().ok_or_else(|| Error::SkillFormat {
                path: path.to_path_buf(),
                reason: format!("frontmatter lacks `{key}`"),
            })
        };
        Ok(Self {
            name: get("name")?,
            description: get("description")?,
            path: path.to_path_buf(),
        })
    }

    /// Full instructions (frontmatter stripped).
    pub fn load(&self) -> Result<String> {
        let text = std::fs::read_to_string(&self.path).map_err(|e| Error::io(&self.path, e))?;
        Ok(split_frontmatter(&text)
            .map(|(_, body)| body.trim_start().to_string())
            .unwrap_or(text))
    }

    /// The one line that goes into the prompt.
    pub fn list_line(&self) -> String {
        format!("{}: {}", self.name, self.description)
    }
}

/// Returns `(frontmatter key/values, body)` for a `---` fenced header.
pub fn split_frontmatter(text: &str) -> Option<(BTreeMap<String, String>, &str)> {
    let rest = text.strip_prefix("\u{feff}").unwrap_or(text);
    let rest = rest.strip_prefix("---")?;
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    let end = rest.find("\n---")?;
    let header = &rest[..end];
    let body = &rest[end + 4..];
    let body = body.strip_prefix('\r').unwrap_or(body);
    let body = body.strip_prefix('\n').unwrap_or(body);
    let mut meta = BTreeMap::new();
    for line in header.lines() {
        if let Some((k, v)) = line.split_once(':') {
            meta.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    Some((meta, body))
}

#[derive(Debug, Default, Clone)]
pub struct SkillSet {
    by_name: BTreeMap<String, Skill>,
}

impl SkillSet {
    /// Scan directories in priority order; earlier directories shadow later ones.
    /// Unreadable or malformed files are skipped and logged, never fatal.
    pub fn discover<P: AsRef<Path>>(dirs: impl IntoIterator<Item = P>) -> Self {
        let mut set = Self::default();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir.as_ref()) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .filter(|p| p.file_name().is_some_and(|f| f != "README.md"))
                .collect();
            paths.sort();
            for p in paths {
                match Skill::from_path(&p) {
                    Ok(skill) => {
                        set.by_name.entry(skill.name.clone()).or_insert(skill);
                    }
                    Err(e) => tracing::warn!("skipping skill {}: {e}", p.display()),
                }
            }
        }
        set
    }

    /// Standard three-level lookup.
    pub fn standard(workdir: &Path, agent_home: &Path, install_dir: &Path) -> Self {
        Self::discover([
            workdir.join("skills"),
            agent_home.join("skills"),
            install_dir.join("skills"),
        ])
    }

    pub fn get(&self, name: &str) -> Result<&Skill> {
        self.by_name
            .get(name)
            .ok_or_else(|| Error::SkillNotFound(name.to_string()))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.by_name.values()
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Prompt listing, one skill per line.
    pub fn listing(&self) -> String {
        self.iter()
            .map(Skill::list_line)
            .collect::<Vec<_>>()
            .join("\n")
    }
}
