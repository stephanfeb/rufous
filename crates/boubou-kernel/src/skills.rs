//! Progressive-disclosure skills: Markdown files with a `name`/`description` frontmatter.
//! Only the one-line listing enters the prompt; the body is loaded on `skills(load)`.
//!
//! Two optional frontmatter keys make a skill apply without the model asking for it:
//! `triggers: a, b, c` (case-insensitive substrings of the task text) and `autoload: true`
//! (every task). Matching skills are prefilled into the task system prompt, at most
//! [`MAX_PREFILL_SKILLS`] per task, in name order. This is the deliberate way to bias a run
//! towards a domain: put the skill in `<workdir>/skills/` and it applies to that folder only.
//!
//! Lookup order (first match wins): `<workdir>/skills`, `<agent_home>/skills`,
//! `<install_dir>/skills`, then the skills built into the product ([`Skill::embedded`],
//! [`SkillSet::with_builtin`]): each product crate compiles its own `skills/*.md` in, so a
//! binary carries the skills of the harness it is, and a user's file of the same name
//! shadows a built-in one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Upper bound on skills prefilled into one task prompt (small models, small contexts).
pub const MAX_PREFILL_SKILLS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// Lower-cased task-text substrings that put this skill in effect for a task.
    pub triggers: Vec<String>,
    /// In effect for every task found from its directory.
    pub autoload: bool,
    /// The full text of a skill compiled into the binary; `None` for one read from disk.
    pub embedded: Option<&'static str>,
}

/// Directory of the running binary: built-in skills live in `<install_dir>/skills`.
pub fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Skill {
    /// Parse frontmatter only; the body stays on disk until `load`.
    pub fn from_path(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Self::parse(&text, path)
    }

    /// A skill compiled into a product crate (`include_str!`): the body stays in memory and
    /// the path reads `<built-in>/<name>.md`, for listings only.
    pub fn embedded(text: &'static str) -> Result<Self> {
        let mut skill = Self::parse(text, Path::new("<built-in>"))?;
        skill.path = PathBuf::from(format!("<built-in>/{}.md", skill.name));
        skill.embedded = Some(text);
        Ok(skill)
    }

    fn parse(text: &str, path: &Path) -> Result<Self> {
        let (meta, _) = split_frontmatter(text).ok_or_else(|| Error::SkillFormat {
            path: path.to_path_buf(),
            reason: "missing `---` frontmatter".into(),
        })?;
        let get = |key: &str| {
            meta.get(key).cloned().ok_or_else(|| Error::SkillFormat {
                path: path.to_path_buf(),
                reason: format!("frontmatter lacks `{key}`"),
            })
        };
        let triggers = meta
            .get("triggers")
            .map(|v| {
                v.split(',')
                    .map(|t| t.trim().to_lowercase())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let autoload = meta
            .get("autoload")
            .is_some_and(|v| matches!(v.to_lowercase().as_str(), "true" | "yes" | "1" | "on"));
        Ok(Self {
            name: get("name")?,
            description: get("description")?,
            path: path.to_path_buf(),
            triggers,
            autoload,
            embedded: None,
        })
    }

    /// Does this skill apply to `task` without being asked for?
    pub fn applies_to(&self, task_lower: &str) -> bool {
        self.autoload
            || self
                .triggers
                .iter()
                .any(|t| task_lower.contains(t.as_str()))
    }

    /// Full instructions (frontmatter stripped), from memory for a built-in skill.
    pub fn load(&self) -> Result<String> {
        let body = |text: &str| {
            split_frontmatter(text)
                .map(|(_, body)| body.trim_start().to_string())
                .unwrap_or_else(|| text.to_string())
        };
        if let Some(text) = self.embedded {
            return Ok(body(text));
        }
        let text = std::fs::read_to_string(&self.path).map_err(|e| Error::io(&self.path, e))?;
        Ok(body(&text))
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

    /// The skills built into the product, below every directory: a file of the same name
    /// anywhere on disk wins.
    pub fn with_builtin(mut self, skills: impl IntoIterator<Item = Skill>) -> Self {
        for skill in skills {
            self.by_name.entry(skill.name.clone()).or_insert(skill);
        }
        self
    }

    /// Standard three-level directory lookup; the product adds its built-in skills with
    /// [`Self::with_builtin`].
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

    /// Skills in effect for `task` (autoload, or a trigger found in the task text), in name
    /// order, capped at [`MAX_PREFILL_SKILLS`].
    pub fn for_task(&self, task: &str) -> Vec<&Skill> {
        self.for_task_capped(task, MAX_PREFILL_SKILLS)
    }

    /// [`Self::for_task`] with the cap taken from the model's profile.
    pub fn for_task_capped(&self, task: &str, cap: usize) -> Vec<&Skill> {
        let low = task.to_lowercase();
        self.iter()
            .filter(|s| s.applies_to(&low))
            .take(cap)
            .collect()
    }

    /// Prompt listing, one skill per line.
    pub fn listing(&self) -> String {
        self.iter()
            .map(Skill::list_line)
            .collect::<Vec<_>>()
            .join("\n")
    }
}
