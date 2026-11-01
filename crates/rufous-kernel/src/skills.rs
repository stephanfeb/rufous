//! Progressive-disclosure skills: Markdown files with a `name`/`description` frontmatter.
//! Only the one-line listing enters the prompt; the body is loaded on `skills(load)`.
//!
//! Two shapes (bead boubou-w3i.1): a flat `<dir>/<name>.md`, rufous's own, and a folder
//! `<dir>/<name>/SKILL.md` in the Agent Skills convention the other harnesses share, which
//! may carry `scripts/`, `references/` and `assets/` beside the file. A folder skill's
//! [`Skill::load`] ends with the list of those files as absolute paths, so the model can
//! `read_file` or `run_bash` them: the convention's third level of disclosure, paid only
//! on load. The frontmatter parser takes the convention's folded multi-line `description`
//! and its nested `metadata` block (parsed, not acted on). `allowed-tools` is not read:
//! rufous's tools are gated by the safety rings, not by a skill.
//!
//! Two optional frontmatter keys make a skill apply without the model asking for it:
//! `triggers: a, b, c` (case-insensitive substrings of the task text) and `autoload: true`
//! (every task). Matching skills are prefilled into the task system prompt, at most
//! [`MAX_PREFILL_SKILLS`] per task, in name order. This is the deliberate way to bias a run
//! towards a domain: put the skill in `<workdir>/skills/` and it applies to that folder only.
//!
//! Lookup order (first match wins, [`standard_dirs`]): `<workdir>/skills`,
//! `<workdir>/.agents/skills`, `<workdir>/.claude/skills`, `<agent_home>/skills`,
//! `~/.agents/skills`, `~/.claude/skills`, `<install_dir>/skills`, then the skills built into
//! the product ([`Skill::embedded`], [`SkillSet::with_builtin`]): each product crate compiles
//! its own `skills/*.md` in, so a binary carries the skills of the harness it is, and a
//! user's file of the same name shadows a built-in one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Upper bound on skills prefilled into one task prompt (small models, small contexts).
pub const MAX_PREFILL_SKILLS: usize = 3;

/// The file that makes a directory a skill in the Agent Skills convention.
pub const SKILL_FILE: &str = "SKILL.md";

/// Upper bound on companion files named when a folder skill is loaded.
pub const MAX_LISTED_FILES: usize = 20;

/// The directories a skill may sit in, in precedence order: the project's own, the
/// conventional project directories, rufous's home, the conventional home directories,
/// the install directory. One list for the loop, the `skills` tool and the CLI.
pub fn standard_dirs(workdir: &Path, agent_home: &Path, install_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        workdir.join("skills"),
        workdir.join(".agents").join("skills"),
        workdir.join(".claude").join("skills"),
        agent_home.join("skills"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".agents").join("skills"));
        dirs.push(home.join(".claude").join("skills"));
    }
    dirs.push(install_dir.join("skills"));
    dirs
}

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
        Self::parse(&text, path, None)
    }

    /// A folder skill: `<dir>/SKILL.md`. The convention names the skill after its folder,
    /// so a frontmatter without `name` takes the folder's name.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let path = dir.join(SKILL_FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        let folder = dir.file_name().and_then(|n| n.to_str());
        Self::parse(&text, &path, folder)
    }

    /// The folder of a folder skill; `None` for a flat file or a built-in.
    pub fn dir(&self) -> Option<&Path> {
        (self.embedded.is_none() && self.path.file_name().is_some_and(|f| f == SKILL_FILE))
            .then(|| self.path.parent())
            .flatten()
    }

    /// A skill compiled into a product crate (`include_str!`): the body stays in memory and
    /// the path reads `<built-in>/<name>.md`, for listings only.
    pub fn embedded(text: &'static str) -> Result<Self> {
        let mut skill = Self::parse(text, Path::new("<built-in>"), None)?;
        skill.path = PathBuf::from(format!("<built-in>/{}.md", skill.name));
        skill.embedded = Some(text);
        Ok(skill)
    }

    fn parse(text: &str, path: &Path, default_name: Option<&str>) -> Result<Self> {
        let (meta, _) = split_frontmatter(text).ok_or_else(|| Error::SkillFormat {
            path: path.to_path_buf(),
            reason: "missing `---` frontmatter".into(),
        })?;
        let get = |key: &str| {
            meta.get(key)
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or_else(|| Error::SkillFormat {
                    path: path.to_path_buf(),
                    reason: format!("frontmatter lacks `{key}`"),
                })
        };
        let name = match get("name") {
            Ok(n) => n,
            Err(e) => default_name.map(str::to_string).ok_or(e)?,
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
            name,
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

    /// Full instructions (frontmatter stripped), from memory for a built-in skill. A folder
    /// skill's body ends with its companion files ([`Self::companion_files`]).
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
        let mut out = body(&text);
        let files = self.companion_files();
        if !files.is_empty() {
            out = out.trim_end().to_string();
            out.push_str(
                "\n\nFiles in this skill's folder (use the full path with read_file or run_bash):",
            );
            for f in files.iter().take(MAX_LISTED_FILES) {
                out.push_str(&format!("\n- {}", f.display()));
            }
            if files.len() > MAX_LISTED_FILES {
                out.push_str(&format!("\n(and {} more)", files.len() - MAX_LISTED_FILES));
            }
            out.push('\n');
        }
        Ok(out)
    }

    /// The files beside a folder skill's `SKILL.md` (scripts, references, assets), as
    /// absolute paths in sorted order; hidden entries skipped. Empty for a flat skill.
    pub fn companion_files(&self) -> Vec<PathBuf> {
        let Some(dir) = self.dir() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let hidden = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'));
                if hidden || p == self.path {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }

    /// The one line that goes into the prompt.
    pub fn list_line(&self) -> String {
        format!("{}: {}", self.name, self.description)
    }
}

/// Returns `(frontmatter key/values, body)` for a `---` fenced header. A key's value may
/// continue on indented lines (YAML's folded `>` / literal `|` scalars and plain
/// continuation), joined with spaces; an indented `key: value` under a key with no value of
/// its own (the convention's `metadata:` block) is kept as `parent.key`.
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
    let mut meta: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in header.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if !indented {
            if let Some((k, v)) = t.split_once(':') {
                let v = v.trim();
                let v = if v == ">" || v == "|" || v == ">-" || v == "|-" {
                    ""
                } else {
                    v
                };
                let k = k.trim().to_string();
                meta.insert(k.clone(), v.to_string());
                current = Some(k);
            }
            continue;
        }
        let Some(parent) = current.clone() else {
            continue;
        };
        let parent_empty = meta.get(&parent).is_some_and(String::is_empty);
        match t.split_once(": ") {
            // A nested mapping under a key with no scalar of its own (`metadata:`).
            Some((k, v)) if parent_empty && !k.contains(' ') => {
                meta.insert(format!("{parent}.{}", k.trim()), v.trim().to_string());
            }
            _ => {
                let slot = meta.entry(parent).or_default();
                if !slot.is_empty() {
                    slot.push(' ');
                }
                slot.push_str(t);
            }
        }
    }
    Some((meta, body))
}

#[derive(Debug, Default, Clone)]
pub struct SkillSet {
    by_name: BTreeMap<String, Skill>,
}

impl SkillSet {
    /// Scan directories in priority order; earlier directories shadow later ones. In each
    /// directory a `*.md` file is a flat skill and a subdirectory holding `SKILL.md` is a
    /// folder skill, in path order. Unreadable or malformed files are skipped and logged,
    /// never fatal.
    pub fn discover<P: AsRef<Path>>(dirs: impl IntoIterator<Item = P>) -> Self {
        let mut set = Self::default();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir.as_ref()) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    (p.is_file()
                        && p.extension().is_some_and(|x| x == "md")
                        && p.file_name().is_some_and(|f| f != "README.md"))
                        || (p.is_dir() && p.join(SKILL_FILE).is_file())
                })
                .collect();
            paths.sort();
            for p in paths {
                let parsed = if p.is_dir() {
                    Skill::from_dir(&p)
                } else {
                    Skill::from_path(&p)
                };
                match parsed {
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

    /// The standard directory lookup ([`standard_dirs`]); the product adds its built-in
    /// skills with [`Self::with_builtin`].
    pub fn standard(workdir: &Path, agent_home: &Path, install_dir: &Path) -> Self {
        Self::discover(standard_dirs(workdir, agent_home, install_dir))
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
