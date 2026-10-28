//! The tool safety gate (`_gate_check`, `_risk_classify`, `_safe_path`).
//!
//! Five-ring model, each ring with one job (read this before changing anything):
//!
//! | ring | mechanism | job |
//! |---|---|---|
//! | 0 | child sandbox | sub-agents are default-deny with no escalation path (not yet ported) |
//! | 1 | [`DANGER_CMD`], [`PS_HIGH_RISK`] | irreversible system damage: refused outright, no exit |
//! | 2 | [`risk_classify`] | uninstall / environment mutation (ask when attended, deny unattended); recursive delete and `find -delete` only inside the workspace |
//! | 3 | [`BASH_ESCAPE_PATTERNS`], [`safe_path`], [`SENSITIVE_PATTERNS`], [`SYSTEM_DIRS`] | boundary and sensitive access go to the [`Confirmer`] (attended: user decides; unattended: refused) |
//! | 4 | `.mingbird_trash`, `.bak`, overwrite guard (in the executor) | damage that gets through rings 0–3 stays reversible |
//!
//! Orthogonal mechanisms that are *not* part of the gate: the crawl guard (context
//! hygiene), the anti-loop ladder, the finish gates (delivery integrity), the output cap.
//! Ring 3's bash part is heuristic (a shell cannot be parsed reliably); `python -c` can
//! step around the workspace boundary, which rings 1/2 and ring 4 backstop.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::text::head;
use crate::tools::ToolInvocation;

pub const SYSTEM_DIRS: [&str; 9] = [
    "\\windows\\",
    "\\program files\\",
    "\\system32\\",
    "/windows/",
    "/program files/",
    "/usr",
    "/etc/",
    "/bin/",
    "/root",
];

/// Ring 1: substrings that mark an irreversible, system-level command.
pub const DANGER_CMD: [&str; 46] = [
    "rm -rf",
    "rm -fr",
    "format c:",
    "format c:\\",
    "del /s /q c:",
    "rd /s /q c:\\",
    "diskpart",
    "mkfs",
    "shutdown",
    "taskkill /f /im",
    "curl | bash",
    "curl | sh",
    "wget | bash",
    "netsh",
    "sc create",
    "certutil",
    "del c:\\*",
    "rm -rf /",
    "rm -fr /",
    "sudo rm",
    "vssadmin delete",
    "bcdedit",
    "cipher /w",
    "format ",
    "del /f /s /q c:",
    "wipefs",
    "of=/dev/",
    "shred ",
    "srm ",
    "userdel",
    "halt",
    "poweroff",
    "wsl --unregister",
    "dism ",
    "pnputil /delete-driver",
    "uninstall-windowsfeature",
    "remove-windowscapability",
    // kept as separate entries so the list stays greppable against Mingbird's
    "rm -rf ~",
    "rm -fr ~",
    "mkfs.",
    "shutdown ",
    "halt ",
    "poweroff ",
    "diskpart ",
    "wipefs ",
    "userdel ",
];

/// Ring 1: powershell is not blocked wholesale (too many legitimate queries), only these
/// download-and-execute / injection / encoded / hidden-window / policy / elevation shapes.
pub const PS_HIGH_RISK: [&str; 12] = [
    "iex ",
    "iex;",
    "(iex",
    "invoke-expression",
    "downloadstring",
    "-encodedcommand",
    "-enc ",
    "net.webclient",
    "start-process -verb runas",
    "set-executionpolicy",
    "-w hidden",
    "bypass -w",
];

/// Ring 3: path fragments (matched against the lower-cased real path with `\` separators)
/// that are always refused for writes and need confirmation for reads.
pub const SENSITIVE_PATTERNS: [&str; 20] = [
    ".ssh\\",
    ".aws\\",
    ".gnupg\\",
    ".env",
    ".pem",
    "id_rsa",
    "id_ed25519",
    "credentials\\",
    "\\token",
    "secrets",
    ".wav",
    "gui_prefs.json",
    "mcp.json",
    "config.json",
    "memory.json",
    "app_lang.txt",
    "\\program files\\",
    "\\windows\\",
    "\\system32\\",
    ".boubou_trash\\",
];

/// Ring 3: command shapes that look like they reach outside the workspace (lower-cased).
pub const BASH_ESCAPE_PATTERNS: [&str; 33] = [
    r"cd\s+[a-z]:\\\\",
    r"cd\s+/d\s+[a-z]:",
    r"del\s+[a-z]:\\",
    r"type\s+[a-z]:\\",
    r"dir\s+[a-z]:\\",
    r"copy\s+[a-z]:\\",
    r"move\s+[a-z]:\\",
    r"ren\s+[a-z]:\\",
    r"echo\s+.*>\\[a-z]:\\",
    r"more\s+[a-z]:\\",
    r"xcopy\s+[a-z]:\\",
    r"rd\s+[a-z]:\\",
    r"attrib\s+[a-z]:\\",
    r"cacls\s+[a-z]:\\",
    r"icacls\s+[a-z]:\\",
    r"takeown\s+[a-z]:\\",
    r"%userprofile%",
    r"%appdata%",
    r"%localappdata%",
    r"\.ssh",
    r"\.aws",
    r"\.env",
    r"mv\s+[^&|;]*\s+~",
    r"cp\s+[^&|;]*\s+~",
    r"mv\s+[^&|;]*/home/",
    r"cp\s+[^&|;]*/home/",
    r"mv\s+[^&|;]*/etc/",
    r"mv\s+[^&|;]*/usr/",
    r"cp\s+[^&|;]*/etc/",
    r"cp\s+[^&|;]*/usr/",
    r"c:\\users",
    r"c:\\program",
    r"d:\\",
];

static ESCAPE_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    BASH_ESCAPE_PATTERNS
        .iter()
        .map(|p| Regex::new(p).expect("regex"))
        .collect()
});

pub fn matches_escape_pattern(cmd_lower: &str) -> bool {
    ESCAPE_RES.iter().any(|re| re.is_match(cmd_lower))
}

static UNINSTALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(pip3?|python\s+-m\s+pip)\s+uninstall\b|\bnpm\s+uninstall\b|\byarn\s+remove\b|\bwinget\s+uninstall\b|\bchoco\s+(uninstall|remove)\b|\bapt(-get)?\s+(remove|purge)\b|\byum\s+remove\b|\bdnf\s+remove\b|\bsnap\s+remove\b|\bpacman\s+(-r|--remove)\b|\bzypper\s+(remove|\brm\b)\b|\bbrew\s+(uninstall|remove)\b|\bflatpak\s+uninstall\b|\bemerge\s+--unmerge\b|\bwsl\s+--uninstall\b|\bmsiexec\s+/x\b|\bscoop\s+uninstall\b|\bwinget\s+remove\b|\bdpkg\s+(-r|--remove|--purge)\b|\brpm\s+-e\b|\bapt(-get)?\s+autoremove\b|\bmake\s+uninstall\b",
    )
    .expect("regex")
});
static ENV_MUTATE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\bsetx\b|\bschtasks\s+/(create|delete|change)\b|\bsc\s+delete\b|\breg\s+(add|delete|import)\b|\bsystemctl\s+(stop|disable|mask)\b|\bcrontab\s+-r\b|\blaunchctl\s+(remove|disable|unload)\b",
    )
    .expect("regex")
});
/// Mingbird excludes `del /s q c:` with a lookahead; that case is already ring 1 here.
static RECURSE_DELETE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\bremove-item\b[^&|;]*-recurse|\brd\s+/s\b|\bdel\s+/s\b|\brmdir\s+/s\b|\brm\s+(-[a-z]*r[a-z]*|--recursive)\b|\brm\s+-\w+\s+-\w*r\b",
    )
    .expect("regex")
});
static FIND_DELETE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bfind\s+(\S+)[^&|;]*-delete\b").expect("regex"));
static DRIVE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]:[\\/]").expect("regex"));

/// Who is in the loop and what they have pre-authorised.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SafetyPolicy {
    /// `AGENT_UNATTENDED=1`: nobody can answer a confirmation, so every ask is a deny.
    pub unattended: bool,
    /// `AGENT_ALLOW_ENV_MUTATION=1`: uninstall / environment changes allowed unattended.
    pub allow_env_mutation: bool,
    /// `AGENT_UNSAFE=1`: bypasses ring 2 only. Rings 1, 3 and 4 are unaffected.
    pub unsafe_mode: bool,
    /// `AGENT_ALLOW_DIRS` (`;`-separated): extra directories treated as inside.
    pub allow_dirs: Vec<PathBuf>,
}

impl SafetyPolicy {
    pub fn from_env() -> Self {
        let flag = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        Self {
            unattended: flag("AGENT_UNATTENDED"),
            allow_env_mutation: flag("AGENT_ALLOW_ENV_MUTATION"),
            unsafe_mode: flag("AGENT_UNSAFE"),
            allow_dirs: std::env::var("AGENT_ALLOW_DIRS")
                .map(|v| {
                    v.split(';')
                        .filter(|s| !s.trim().is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn unattended() -> Self {
        Self {
            unattended: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmRequest<'a> {
    pub tool: &'a str,
    pub target: String,
    pub action: &'a str,
    pub workdir: &'a Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Allow, and do not ask again for this tool during the run (paths only, never ring 1).
    AllowAll,
    Deny,
}

/// The human-in-the-loop channel (GUI dialog, CLI prompt). [`DenyAll`] is the headless default.
pub trait Confirmer: Send {
    fn confirm(&mut self, req: &ConfirmRequest<'_>) -> Verdict;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAll;

impl Confirmer for DenyAll {
    fn confirm(&mut self, _req: &ConfirmRequest<'_>) -> Verdict {
        Verdict::Deny
    }
}

/// Lexically normalise, then canonicalise the longest existing prefix (Python
/// `realpath` semantics: works for paths that do not exist yet).
pub fn resolve_path(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    };
    let mut norm = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                norm.pop();
            }
            other => norm.push(other.as_os_str()),
        }
    }
    let mut existing = norm.clone();
    let mut rest = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(n) => {
                rest.push(n.to_os_string());
                existing.pop();
            }
            None => break,
        }
    }
    let mut out = existing.canonicalize().unwrap_or(existing);
    for r in rest.iter().rev() {
        out.push(r);
    }
    out
}

fn normcase(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

fn is_within(child: &Path, base: &Path) -> bool {
    let (c, b) = (normcase(child), normcase(base));
    c == b
        || c.strip_prefix(b.as_str())
            .is_some_and(|rest| rest.starts_with(std::path::MAIN_SEPARATOR))
}

/// `_safe_path`: `(real_absolute, inside)`. Collapses `..`, follows symlinks, and treats
/// `allow_dirs` as inside. A Windows drive path can never be inside a POSIX workspace.
pub fn safe_path(workdir: &Path, path: &str, allow_dirs: &[PathBuf]) -> (PathBuf, bool) {
    if !cfg!(windows) && DRIVE_RE.is_match(path) {
        return (PathBuf::from(path), false);
    }
    let base = resolve_path(workdir);
    let real = resolve_path(&workdir.join(path));
    let inside = is_within(&real, &base)
        || allow_dirs
            .iter()
            .any(|d| is_within(&real, &resolve_path(d)));
    (real, inside)
}

pub fn is_sensitive_path(path: &Path) -> bool {
    let p = path.to_string_lossy().to_lowercase().replace('/', "\\");
    SENSITIVE_PATTERNS.iter().any(|s| p.contains(s))
}

/// `_delete_targets`: path candidates in a recursive-delete command (verbs and flags skipped).
pub fn delete_targets(cmd: &str) -> Vec<String> {
    let Some(m) = RECURSE_DELETE_RE.find(cmd) else {
        return Vec::new();
    };
    let verbs = ["remove-item", "rd", "del", "rmdir", "rm"];
    cmd[m.start()..]
        .split_whitespace()
        .map(|t| t.trim_matches(['"', '\'']))
        .filter(|t| !t.is_empty())
        .filter(|t| !verbs.contains(&t.to_lowercase().as_str()) && !t.starts_with('-'))
        .filter(|t| !(t.len() == 2 && t.starts_with('/') && t.as_bytes()[1].is_ascii_alphabetic()))
        .filter(|t| !(t.len() == 2 && t.ends_with(':') && t.as_bytes()[0].is_ascii_alphabetic()))
        .map(str::to_string)
        .collect()
}

fn ask(
    policy: &SafetyPolicy,
    confirmer: &mut dyn Confirmer,
    tool: &str,
    target: String,
    workdir: &Path,
    action: &str,
) -> (bool, bool, String) {
    if policy.unattended {
        return (
            false,
            false,
            format!("[safety gate: unattended mode (AGENT_UNATTENDED=1) has nobody in the loop; {action} is always refused. Target: {target}]"),
        );
    }
    let req = ConfirmRequest {
        tool,
        target: target.clone(),
        action,
        workdir,
    };
    match confirmer.confirm(&req) {
        Verdict::Allow => (true, false, String::new()),
        Verdict::AllowAll => (true, true, String::new()),
        Verdict::Deny => (
            false,
            false,
            format!(
                "[safety gate: path {target} is outside the working directory. Working directory: {}. Refused. CLI mode does not allow out-of-bounds access.]",
                resolve_path(workdir).display()
            ),
        ),
    }
}

/// Ring 2: `_risk_classify`. Returns the refusal text, or `None` to let the command through.
pub fn risk_classify(
    cmd: &str,
    workdir: &Path,
    policy: &SafetyPolicy,
    confirmer: &mut dyn Confirmer,
) -> Option<String> {
    if UNINSTALL_RE.is_match(cmd) {
        if policy.allow_env_mutation {
            return None;
        }
        if policy.unattended {
            return Some("[safety net: package uninstall/removal detected; refused by default in unattended mode (stops a small model impulsively removing dependencies). If you really must uninstall, set AGENT_ALLOW_ENV_MUTATION=1 and retry.]".into());
        }
        let (allow, _, msg) = ask(
            policy,
            confirmer,
            "run_bash",
            format!("uninstall: {}", head(cmd, 60)),
            workdir,
            "uninstall a package",
        );
        return if allow {
            None
        } else {
            Some(if msg.is_empty() {
                "[safety net: uninstall refused]".into()
            } else {
                msg
            })
        };
    }
    if ENV_MUTATE_RE.is_match(cmd) {
        if policy.allow_env_mutation {
            return None;
        }
        if policy.unattended {
            return Some("[safety net: system-environment change detected (setx / scheduled task / service removal / registry write); refused by default in unattended mode. If you really must, set AGENT_ALLOW_ENV_MUTATION=1 and retry.]".into());
        }
        let (allow, _, msg) = ask(
            policy,
            confirmer,
            "run_bash",
            format!("env-mutate: {}", head(cmd, 60)),
            workdir,
            "change the system environment",
        );
        return if allow {
            None
        } else {
            Some(if msg.is_empty() {
                "[safety net: system-environment change refused]".into()
            } else {
                msg
            })
        };
    }
    if RECURSE_DELETE_RE.is_match(cmd) {
        let targets = delete_targets(cmd);
        if targets.is_empty() {
            return Some(
                "[safety net: could not work out a definite target for this recursive delete; refused. Give an explicit target path.]".into(),
            );
        }
        for t in targets {
            let (_, inside) = safe_path(workdir, &t, &policy.allow_dirs);
            if !inside {
                return Some(format!(
                    "[safety net: recursive-delete target '{}' is outside the working directory; refused. Recursive deletes inside the working directory are unrestricted; for files outside it use delete_file one at a time.]",
                    head(&t, 60)
                ));
            }
        }
    }
    if let Some(m) = FIND_DELETE_RE.captures(cmd) {
        let target = &m[1];
        let (_, inside) = safe_path(workdir, target, &policy.allow_dirs);
        if !inside {
            return Some(format!(
                "[safety net: the find -delete target '{}' is outside the working directory; refused. find -delete inside the working directory is unrestricted.]",
                head(target, 60)
            ));
        }
    }
    None
}

/// The gate with its per-run state ("allow all" grants).
pub struct Gate {
    pub policy: SafetyPolicy,
    pub confirmer: Box<dyn Confirmer>,
    allow_all: HashSet<&'static str>,
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gate")
            .field("policy", &self.policy)
            .field("allow_all", &self.allow_all)
            .finish()
    }
}

const FILE_TOOLS: [&str; 7] = [
    "create_file",
    "read_file",
    "edit_file",
    "append_file",
    "delete_file",
    "list_dir",
    "search_files",
];
const WRITE_TOOLS: [&str; 4] = ["create_file", "edit_file", "append_file", "delete_file"];

impl Gate {
    pub fn new(policy: SafetyPolicy, confirmer: Box<dyn Confirmer>) -> Self {
        Self {
            policy,
            confirmer,
            allow_all: HashSet::new(),
        }
    }

    pub fn headless(policy: SafetyPolicy) -> Self {
        Self::new(policy, Box::new(DenyAll))
    }

    /// `_gate_check`: the refusal text, or `None` to let the call through.
    pub fn check(&mut self, inv: &ToolInvocation, workdir: &Path) -> Option<String> {
        let name = inv.name();
        if let ToolInvocation::RunBash { command } = inv {
            let low = command.to_lowercase();
            if DANGER_CMD.iter().any(|d| low.contains(d)) {
                return Some(format!(
                    "[safety gate: the command contains a dangerous operation; refused. Original command: {}]",
                    head(command, 80)
                ));
            }
            if low.contains("powershell") && PS_HIGH_RISK.iter().any(|p| low.contains(p)) {
                return Some(format!(
                    "[safety gate: the powershell command contains a high-risk operation (download-and-execute / script injection / privilege escalation); refused. Original command: {}]",
                    head(command, 80)
                ));
            }
            if !self.policy.unsafe_mode {
                if let Some(r) =
                    risk_classify(command, workdir, &self.policy, self.confirmer.as_mut())
                {
                    return Some(r);
                }
            }
            if !self.allow_all.contains("run_bash") && matches_escape_pattern(&low) {
                let (allow, allok, msg) = ask(
                    &self.policy,
                    self.confirmer.as_mut(),
                    name,
                    format!("bash: {}", head(command, 60)),
                    workdir,
                    "run a command",
                );
                if allow {
                    if allok {
                        self.allow_all.insert("run_bash");
                    }
                    return None;
                }
                return Some(if msg.is_empty() {
                    format!(
                        "[safety gate: the command appears to access paths outside the working directory; refused. Original command: {}]",
                        head(command, 80)
                    )
                } else {
                    msg
                });
            }
            return None;
        }
        if FILE_TOOLS.contains(&name) {
            if self.allow_all.contains(name) {
                return None;
            }
            if let Some(path) = inv.file_path().filter(|p| !p.is_empty()) {
                let (real, inside) = safe_path(workdir, path, &self.policy.allow_dirs);
                if !inside {
                    let (allow, allok, msg) = ask(
                        &self.policy,
                        self.confirmer.as_mut(),
                        name,
                        real.display().to_string(),
                        workdir,
                        "access",
                    );
                    if allow {
                        if allok {
                            self.allow_all.insert(
                                FILE_TOOLS
                                    .iter()
                                    .find(|t| **t == name)
                                    .copied()
                                    .unwrap_or("read_file"),
                            );
                        }
                        return None;
                    }
                    return Some(if msg.is_empty() {
                        format!(
                            "[safety gate: path {} is outside the working directory. Working directory: {}. Refused. If access is really needed, confirm it in the GUI prompt.]",
                            real.display(),
                            resolve_path(workdir).display()
                        )
                    } else {
                        msg
                    });
                }
                if is_sensitive_path(&real) {
                    if WRITE_TOOLS.contains(&name) {
                        return Some(format!(
                            "[safety gate: path {} matches a sensitive pattern; writes are always refused.]",
                            real.display()
                        ));
                    }
                    let (allow, allok, msg) = ask(
                        &self.policy,
                        self.confirmer.as_mut(),
                        name,
                        real.display().to_string(),
                        workdir,
                        "read a sensitive file",
                    );
                    if allow {
                        if allok {
                            self.allow_all.insert(
                                FILE_TOOLS
                                    .iter()
                                    .find(|t| **t == name)
                                    .copied()
                                    .unwrap_or("read_file"),
                            );
                        }
                        return None;
                    }
                    return Some(if msg.is_empty() {
                        format!(
                            "[safety gate: path {} matches a sensitive pattern; read refused.]",
                            real.display()
                        )
                    } else {
                        msg
                    });
                }
            }
        }
        if WRITE_TOOLS.contains(&name) {
            if let Some(path) = inv.file_path() {
                let p = format!("{path}\\").to_lowercase();
                if SYSTEM_DIRS.iter().any(|d| p.contains(d)) {
                    return Some(format!(
                        "[safety gate: the target path contains a system directory; refused. Path: {}]",
                        head(path, 80)
                    ));
                }
            }
        }
        None
    }
}
