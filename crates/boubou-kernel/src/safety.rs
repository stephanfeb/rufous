//! The tool safety gate (`_gate_check`, `_risk_classify`, `_safe_path`).
//!
//! Five-ring model, each ring with one job (read this before changing anything):
//!
//! | ring | mechanism | job |
//! |---|---|---|
//! | 0 | child sandbox | sub-agents are default-deny with no escalation path (not yet ported) |
//! | 1 | [`DANGER_CMD`], [`PS_HIGH_RISK`] | irreversible system damage: refused outright, no exit |
//! | 2 | [`risk_classify`] | uninstall / environment mutation (ask when attended, deny unattended); recursive delete and `find -delete` only inside the workspace, judged after `~`/`$HOME` expansion and any `cd` earlier in the command; a target or `cd` the gate cannot evaluate is refused |
//! | 3 | [`bash_boundary_target`], [`BASH_ESCAPE_PATTERNS`], [`safe_path`], [`SENSITIVE_PATTERNS`], [`SYSTEM_DIRS`] | boundary and sensitive access go to the [`Confirmer`] (attended: user decides; unattended: refused); a shell command's redirect, `tee`, `cp`/`mv`/`install` destination is judged as a file-tool path |
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

use crate::scope::{self, Scope, ScopeGateMode};
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

/// `DANGER_CMD` entries that are also ordinary words. A plain substring test refused a
/// `python -c "…"` CSV script because a comment read `# Format the result` (false_finish probe,
/// 2026-10-28, eleven refusals in one trial); `halt`, `shutdown`, `dism`, `shred`, `srm` sit in
/// prose the same way. These count only in *program position*: the word that a shell segment
/// actually runs, once leading `VAR=value` assignments, wrappers such as `sudo`, `nohup`,
/// `exec`, `timeout 5`, `sh -c`, shell keywords such as `then`/`do`, a directory prefix
/// (`/sbin/shutdown`) and surrounding quotes are stripped. Arguments (`echo shutdown done`,
/// `git commit -m "halt the retry"`) and comments never count. An entry written with a
/// trailing space (`format `, `shred `) needs an argument after the program, so
/// `format(total, '.2f')` in a script stays clean while `format c:` does not. Everything else
/// in the list (`rm -rf`, `of=/dev/`, `curl | bash`, …) stays a substring match.
pub const DANGER_CMD_TOKEN_ONLY: [&str; 12] = [
    "format ",
    "format c:",
    "format c:\\",
    "halt",
    "halt ",
    "shutdown",
    "shutdown ",
    "shred ",
    "srm ",
    "dism ",
    "poweroff",
    "poweroff ",
];

/// Words that run the program named after them. A token-only danger word that follows one
/// of these (and the wrapper's own flags) is in program position. Interpreters are here for
/// their `-c`/`-e` scripts; `systemctl` for `systemctl poweroff`.
const COMMAND_WRAPPERS: [&str; 46] = [
    "sudo",
    "doas",
    "su",
    "nohup",
    "exec",
    "time",
    "nice",
    "env",
    "command",
    "builtin",
    "eval",
    "xargs",
    "setsid",
    "watch",
    "timeout",
    "chroot",
    "systemctl",
    "then",
    "do",
    "else",
    "elif",
    "if",
    "while",
    "until",
    "!",
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "python",
    "python3",
    "python2",
    "perl",
    "ruby",
    "node",
    "osascript",
    "cmd",
    "cmd.exe",
    "powershell",
    "pwsh",
    "start",
    "call",
    "busybox",
    "run0",
];

/// Wrapper flags that consume the next word (`sudo -u root shutdown`, `nice -n 5 halt`).
const WRAPPER_FLAGS_WITH_VALUE: [&str; 9] = [
    "-u", "-g", "-n", "-s", "-k", "-p", "--user", "--group", "--signal",
];

/// Shell segment separators: whatever follows one of these starts a new command.
const SEGMENT_SEPARATORS: [char; 9] = ['\n', ';', '&', '|', '(', ')', '{', '}', '`'];

/// The `DANGER_CMD` entry a lower-cased command trips, if any.
pub fn danger_hit(low: &str) -> Option<&'static str> {
    DANGER_CMD.iter().copied().find(|d| {
        if DANGER_CMD_TOKEN_ONLY.contains(d) {
            runs_program(low, d)
        } else {
            low.contains(d)
        }
    })
}

/// Does some segment of `low` run the program `needle` names (see `DANGER_CMD_TOKEN_ONLY`)?
fn runs_program(low: &str, needle: &str) -> bool {
    let entry = needle.trim_end();
    let first = entry.split_whitespace().next().unwrap_or(entry);
    let multi_word = entry.contains(' ');
    let needs_args = needle.ends_with(' ');
    low.split(SEGMENT_SEPARATORS).any(|seg| {
        let Some((program, rest)) = program_of(seg) else {
            return false;
        };
        if program != first {
            return false;
        }
        if multi_word {
            format!("{program} {rest}").starts_with(entry)
        } else if needs_args {
            !rest.is_empty()
        } else {
            true
        }
    })
}

/// `VAR=value` (a shell assignment that precedes a command).
fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((k, _)) => !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        None => false,
    }
}

/// A word a wrapper consumes before the program: a flag, a `cmd /c` switch, a number
/// (`timeout 5`), or an assignment (`env FOO=1`).
fn is_wrapper_option(w: &str) -> bool {
    w.starts_with('-')
        || (w.starts_with('/') && w.len() <= 3)
        || w.starts_with(|c: char| c.is_ascii_digit())
        || is_assignment(w)
}

/// The program a segment runs as a bare, unquoted basename, and the rest of the segment
/// after it; `None` for an empty segment or a comment.
fn program_of(seg: &str) -> Option<(String, String)> {
    let words: Vec<&str> = seg.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() && is_assignment(words[i]) {
        i += 1;
    }
    loop {
        let w = words.get(i)?;
        if w.starts_with('#') {
            return None;
        }
        let base = program_basename(w);
        if COMMAND_WRAPPERS.contains(&base.as_str()) {
            i += 1;
            while let Some(a) = words.get(i) {
                if a.starts_with('#') {
                    return None;
                }
                if !is_wrapper_option(a) {
                    break;
                }
                i += 1;
                if WRAPPER_FLAGS_WITH_VALUE.contains(a) {
                    i += 1;
                }
            }
            continue;
        }
        return Some((base, words[i + 1..].join(" ")));
    }
}

/// `'/sbin/shutdown'` → `shutdown`: quotes off, directory off.
fn program_basename(w: &str) -> String {
    let bare = w.trim_matches(|c| c == '\'' || c == '"');
    bare.rsplit(['/', '\\']).next().unwrap_or(bare).to_string()
}

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
pub const SENSITIVE_PATTERNS: [&str; 19] = [
    ".ssh\\",
    ".aws\\",
    ".gnupg\\",
    ".env",
    ".pem",
    "id_rsa",
    "id_ed25519",
    "credentials\\",
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
    r"\.env\b",
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

/// Why the gate refused a call, as data (bead boubou-bj4). The text the model reads is
/// chosen for how a small model reads it and changes under measurement; the loop and the
/// bench branch on this instead of on that prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    /// Ring 1: a dangerous command or a high-risk powershell shape.
    Danger,
    /// Ring 2: uninstall, environment mutation, recursive delete outside the workspace.
    Risk,
    /// Ring 3: a path or a shell write target outside the working directory.
    Boundary,
    /// Ring 3: a sensitive path.
    Sensitive,
    /// Ring 3: a system-directory target.
    SystemDir,
    /// The scope gate: outside the turn's instruction.
    Scope,
    /// The declined file name at another path (bead boubou-sxx).
    Substitute,
    /// Ring 0: the child sandbox.
    Child,
    /// A project hook blocked the call.
    Hook,
}

impl RefusalKind {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Danger => "danger",
            Self::Risk => "risk",
            Self::Boundary => "boundary",
            Self::Sensitive => "sensitive",
            Self::SystemDir => "system_dir",
            Self::Scope => "scope",
            Self::Substitute => "substitute",
            Self::Child => "child",
            Self::Hook => "hook",
        }
    }
}

/// A refusal and who decided it: a person answering the dialog, or the gate alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Refused {
    pub kind: RefusalKind,
    pub by_user: bool,
}

impl Refused {
    pub fn gate(kind: RefusalKind) -> Self {
        Self {
            kind,
            by_user: false,
        }
    }
}

/// The human-in-the-loop channel (GUI dialog, CLI prompt). [`DenyAll`] is the headless default.
pub trait Confirmer: Send {
    fn confirm(&mut self, req: &ConfirmRequest<'_>) -> Verdict;
    /// Is a person answering? Decides which refusal the model reads after a Deny: the
    /// user's decision, or a boundary with nobody to ask. Headless confirmers return false.
    fn attended(&self) -> bool {
        true
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAll;

impl Confirmer for DenyAll {
    fn confirm(&mut self, _req: &ConfirmRequest<'_>) -> Verdict {
        Verdict::Deny
    }
    fn attended(&self) -> bool {
        false
    }
}

/// `os.path.realpath` semantics: walk the absolute path one component at a time, follow a
/// symlink the moment it is met, and let `..` act on the *resolved* prefix. A `..` after a
/// link therefore climbs out of the link's target, which is what the kernel does when the
/// path is opened; collapsing `..` lexically first (the previous version) let
/// `link/../x` pass as `<workdir>/x` while the write landed beside the link's target.
/// Works for paths that do not exist yet: the missing tail is appended as written.
/// Symlink chains are bounded at [`MAX_SYMLINK_HOPS`]; a dangling link resolves to its
/// target path as written.
pub fn resolve_path(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    };
    let mut out = PathBuf::new();
    realpath_into(&mut out, &abs, 0);
    out
}

const MAX_SYMLINK_HOPS: u32 = 40;

fn realpath_into(out: &mut PathBuf, p: &Path, hops: u32) {
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // `pop` on a bare root is a no-op, like realpath above `/`.
                out.pop();
            }
            Component::Normal(n) => {
                out.push(n);
                if hops < MAX_SYMLINK_HOPS {
                    if let Ok(target) = std::fs::read_link(&*out) {
                        out.pop();
                        if target.is_absolute() {
                            out.clear();
                        }
                        // A relative target is relative to the link's directory, which is
                        // `out` now; the target may itself hold links and `..`.
                        realpath_into(out, &target, hops + 1);
                    }
                }
            }
        }
    }
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
    safe_path_from(workdir, workdir, path, allow_dirs)
}

/// [`safe_path`] for a `path` that is relative to `base` rather than to the workdir: the
/// directory a shell command is in after a `cd`. Inside-ness is still judged against
/// `workdir`.
pub fn safe_path_from(
    base: &Path,
    workdir: &Path,
    path: &str,
    allow_dirs: &[PathBuf],
) -> (PathBuf, bool) {
    if !cfg!(windows) && DRIVE_RE.is_match(path) {
        return (PathBuf::from(path), false);
    }
    let wd = resolve_path(workdir);
    let real = resolve_path(&base.join(path));
    let inside = is_within(&real, &wd)
        || allow_dirs
            .iter()
            .any(|d| is_within(&real, &resolve_path(d)));
    (real, inside)
}

/// A shell path argument after home expansion: a definite path, or one the gate cannot
/// evaluate (a variable other than the home ones, a substitution, `~user`).
#[derive(Debug, PartialEq, Eq)]
enum ShellPath {
    Known(String),
    Unknown,
}

fn home_string() -> Option<String> {
    dirs::home_dir().map(|h| h.to_string_lossy().into_owned())
}

/// `~`, `~/x`, `$HOME/x`, `${HOME}/x`, `%USERPROFILE%\x` become the real home path; a
/// plain path is returned as is. Before this, `rm -r ~/x` was judged as `<workdir>/~/x`,
/// inside, and ran unprompted (review 2026-10-29; Mingbird has the same gap).
fn expand_shell_path(t: &str) -> ShellPath {
    let t = t.trim_matches(['"', '\'']);
    if t == "~" || t.starts_with("~/") || t.starts_with("~\\") {
        return match home_string() {
            Some(h) => ShellPath::Known(format!("{h}{}", &t[1..])),
            None => ShellPath::Unknown,
        };
    }
    if t.starts_with('~') {
        return ShellPath::Unknown; // ~user: another account's home
    }
    let low = t.to_lowercase();
    for var in [
        "${home}",
        "$home",
        "${userprofile}",
        "$userprofile",
        "%userprofile%",
        "%home%",
    ] {
        if let Some(rest) = low.strip_prefix(var) {
            if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') {
                return match home_string() {
                    Some(h) => ShellPath::Known(format!("{h}{}", &t[var.len()..])),
                    None => ShellPath::Unknown,
                };
            }
        }
    }
    if t.contains('$') || t.contains('`') || t.matches('%').count() >= 2 {
        return ShellPath::Unknown;
    }
    ShellPath::Known(t.to_string())
}

/// The directory a command is in by byte `offset`, after any `cd`/`pushd` in the segments
/// before it (`cd / && rm -r etc` deletes `/etc`, not `<workdir>/etc`). `None` when a `cd`
/// went somewhere the gate cannot evaluate (`cd -`, `cd $VAR`).
fn cwd_before(cmd: &str, offset: usize, workdir: &Path) -> Option<PathBuf> {
    let mut cwd = workdir.to_path_buf();
    for seg in cmd[..offset].split(['\n', ';', '&', '|']) {
        let mut words = seg.split_whitespace();
        let Some(prog) = words.next() else {
            continue;
        };
        if !matches!(prog.to_lowercase().as_str(), "cd" | "pushd" | "chdir") {
            continue;
        }
        let arg =
            words.find(|w| !(w.starts_with('-') && w.len() > 1) && !w.eq_ignore_ascii_case("/d"));
        match arg {
            None => cwd = dirs::home_dir()?,
            Some("-") => return None,
            Some(a) => match expand_shell_path(a) {
                ShellPath::Known(p) => cwd = resolve_path(&cwd.join(p)),
                ShellPath::Unknown => return None,
            },
        }
    }
    Some(cwd)
}

const UNKNOWN_CWD_MSG: &str = "[safety net: the command changes directory to a place the gate cannot evaluate (cd -, cd $VAR) before a recursive delete; refused. Delete with a path relative to the working directory instead.]";

pub fn is_sensitive_path(path: &Path) -> bool {
    let p = path.to_string_lossy().to_lowercase().replace('/', "\\");
    SENSITIVE_PATTERNS.iter().any(|s| p.contains(s))
        || path
            .file_name()
            .is_some_and(|f| is_token_file(&f.to_string_lossy()))
}

/// Extensions a stored credential comes in; source files are never one.
const CREDENTIAL_EXTS: [&str; 12] = [
    "", "json", "txt", "yaml", "yml", "toml", "ini", "cfg", "conf", "env", "key", "secret",
];

/// A credential file named after what it holds: `token`, `.token`, `tokens.json`,
/// `access_token.txt`, `api-token.yaml`. Mingbird matched the substring `\token`, which
/// also caught `tokens.py`, `tokenizer.rs` and `token_store.dart` and refused every write to
/// them (bench cell CP-02 at 8k, 2026-10-29, bead boubou-a09); the model then wrote the
/// file through `cat >` in a shell instead. Only the name's stem and extension decide now.
pub fn is_token_file(name: &str) -> bool {
    let name = name.to_lowercase();
    let name = name.trim_start_matches('.');
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, e),
        None => (name, ""),
    };
    let stem_is_token = stem == "token"
        || stem == "tokens"
        || stem.ends_with("_token")
        || stem.ends_with("-token")
        || stem.ends_with("_tokens")
        || stem.ends_with("-tokens");
    stem_is_token && CREDENTIAL_EXTS.contains(&ext)
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

/// Where a shell command writes, each target with its byte offset (so the `cd` before it
/// applies): redirect targets (`>`, `>>`, `>|`, `&>`, `2>`), what `tee` writes, and the
/// destination of `cp`, `mv`, `install`, `rsync` and `ln` (the last operand, or `-t dir`).
/// A descriptor (`2>&1`), a process substitution (`>(…)`) and a heredoc marker are not
/// paths and are skipped. The conversation rig (2026-10-31, bead boubou-hzl) found the
/// file-tool boundary one redirect away: `create_file /tmp/x` was denied, `echo hello >
/// /tmp/x` ran, because [`BASH_ESCAPE_PATTERNS`] knows drive letters and `~` but no redirect.
pub fn write_targets(cmd: &str) -> Vec<(usize, String)> {
    static REDIRECT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?:\d|&)?>>?\|?\s*("[^"]*"|'[^']*'|[^\s;&|<>()]+)"#).expect("regex")
    });
    let mut out: Vec<(usize, String)> = Vec::new();
    for cap in REDIRECT.captures_iter(cmd) {
        let m = cap.get(1).expect("group");
        out.push((m.start(), m.as_str().trim_matches(['"', '\'']).to_string()));
    }
    let mut pos = 0;
    for seg in cmd.split(SEGMENT_SEPARATORS) {
        let off = pos;
        pos += seg.len() + 1;
        let Some((program, rest)) = program_of(seg) else {
            continue;
        };
        let words: Vec<&str> = rest.split_whitespace().collect();
        let operand = |w: &&&str| !w.starts_with('-') && !w.contains(['>', '<']);
        match program.as_str() {
            "tee" => out.extend(
                words
                    .iter()
                    .filter(operand)
                    .map(|w| (off, w.trim_matches(['"', '\'']).to_string())),
            ),
            "cp" | "mv" | "install" | "rsync" | "ln" => {
                let dest = words
                    .iter()
                    .position(|w| *w == "-t" || *w == "--target-directory")
                    .and_then(|i| words.get(i + 1).copied())
                    .or_else(|| {
                        words
                            .iter()
                            .find_map(|w| w.strip_prefix("--target-directory="))
                    })
                    .or_else(|| {
                        let ops: Vec<&&str> = words.iter().filter(operand).collect();
                        (ops.len() >= 2).then(|| **ops.last().expect("two"))
                    });
                if let Some(d) = dest {
                    out.push((off, d.trim_matches(['"', '\'']).to_string()));
                }
            }
            _ => {}
        }
    }
    out
}

/// The first path a shell command writes outside the workdir and the `allow_dirs`, resolved
/// as the file tools resolve theirs ([`safe_path_from`] after home expansion and the `cd`
/// before it). `None` when every target is inside, a device file, or one the gate cannot
/// evaluate (`> $OUT`, a `cd $X` before it): a variable in a redirect is ordinary in a
/// script and the write, unlike a recursive delete, is bounded to one file.
pub fn bash_boundary_target(cmd: &str, workdir: &Path, allow_dirs: &[PathBuf]) -> Option<PathBuf> {
    resolved_write_targets(cmd, workdir, allow_dirs)
        .into_iter()
        .find_map(|(real, inside)| (!inside).then_some(real))
}

/// Every definite write target of a shell command as `(resolved path, inside)`; device
/// files and targets the gate cannot evaluate are left out.
fn resolved_write_targets(
    cmd: &str,
    workdir: &Path,
    allow_dirs: &[PathBuf],
) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    for (off, t) in write_targets(cmd) {
        let ShellPath::Known(p) = expand_shell_path(&t) else {
            continue;
        };
        if p.starts_with("/dev/") || p.eq_ignore_ascii_case("nul") {
            continue;
        }
        let Some(base) = cwd_before(cmd, off, workdir) else {
            continue;
        };
        out.push(safe_path_from(&base, workdir, &p, allow_dirs));
    }
    out
}

/// One question to the confirmer. `Ok(allow_all)` lets the call through; `Err(text)` is
/// what the model reads instead of the tool's output, and every word of it steers the next
/// call (conversation rig 2026-10-31, bead boubou-ik4): after the user's Deny the old text
/// said "CLI mode does not allow out-of-bounds access", and gemma4:e4b wrote the file inside
/// the project instead while qwen3.5:4b wrote it through a shell redirect, both reporting
/// success. Three texts now, by what happened: unattended (nobody in the loop), the user
/// declined (say so, forbid the workaround, ask for a report), or a headless boundary.
/// What the model reads instead of a tool's output, and whether a person said no.
struct Refusal {
    text: String,
    by_user: bool,
}

fn refused(text: String) -> Refusal {
    Refusal {
        text,
        by_user: false,
    }
}

fn ask(
    policy: &SafetyPolicy,
    confirmer: &mut dyn Confirmer,
    tool: &str,
    target: String,
    workdir: &Path,
    action: &str,
    reason: &str,
) -> Result<bool, Refusal> {
    if policy.unattended {
        return Err(refused(format!(
            "[safety gate: unattended mode (AGENT_UNATTENDED=1) has nobody in the loop; {action} is always refused. Target: {target}]"
        )));
    }
    let req = ConfirmRequest {
        tool,
        target: target.clone(),
        action,
        workdir,
    };
    match confirmer.confirm(&req) {
        Verdict::Allow => Ok(false),
        Verdict::AllowAll => Ok(true),
        // No reason in the user's refusal (bead boubou-3bv): with "outside the working
        // directory" in it, both 4B models quoted that phrase back as a system rule and
        // refused the user's own "try again" 6/6 (2026-10-31), whatever the text said about
        // who declined or about asking again. The dialog title already named the path. The
        // retry clause is bead boubou-77n.
        Verdict::Deny if confirmer.attended() => Err(Refusal {
            text: format!(
                "[safety gate: the user declined this call: {tool} on {target}. Do not do it another way or write it somewhere else; say what you could not do. If the user asks for it again, make the same call again: they are asked each time and may allow it.]"
            ),
            by_user: true,
        }),
        Verdict::Deny => Err(refused(format!(
            "[safety gate: {tool} on {target} is {reason}; refused.]"
        ))),
    }
}

fn boundary_reason(workdir: &Path) -> String {
    format!(
        "outside the working directory {}",
        resolve_path(workdir).display()
    )
}

/// Ring 2: `_risk_classify`. Returns the refusal text, or `None` to let the command through.
pub fn risk_classify(
    cmd: &str,
    workdir: &Path,
    policy: &SafetyPolicy,
    confirmer: &mut dyn Confirmer,
) -> Option<String> {
    risk_refusal(cmd, workdir, policy, confirmer).map(|r| r.text)
}

/// [`risk_classify`] with the deciding party kept.
fn risk_refusal(
    cmd: &str,
    workdir: &Path,
    policy: &SafetyPolicy,
    confirmer: &mut dyn Confirmer,
) -> Option<Refusal> {
    if UNINSTALL_RE.is_match(cmd) {
        if policy.allow_env_mutation {
            return None;
        }
        if policy.unattended {
            return Some(refused("[safety net: package uninstall/removal detected; refused by default in unattended mode (stops a small model impulsively removing dependencies). If you really must uninstall, set AGENT_ALLOW_ENV_MUTATION=1 and retry.]".into()));
        }
        return ask(
            policy,
            confirmer,
            "run_bash",
            format!("uninstall: {}", head(cmd, 60)),
            workdir,
            "uninstall a package",
            "a package uninstall",
        )
        .err();
    }
    if ENV_MUTATE_RE.is_match(cmd) {
        if policy.allow_env_mutation {
            return None;
        }
        if policy.unattended {
            return Some(refused("[safety net: system-environment change detected (setx / scheduled task / service removal / registry write); refused by default in unattended mode. If you really must, set AGENT_ALLOW_ENV_MUTATION=1 and retry.]".into()));
        }
        return ask(
            policy,
            confirmer,
            "run_bash",
            format!("env-mutate: {}", head(cmd, 60)),
            workdir,
            "change the system environment",
            "a system-environment change",
        )
        .err();
    }
    if let Some(m) = RECURSE_DELETE_RE.find(cmd) {
        let targets = delete_targets(cmd);
        if targets.is_empty() {
            return Some(refused(
                "[safety net: could not work out a definite target for this recursive delete; refused. Give an explicit target path.]".into(),
            ));
        }
        let Some(base) = cwd_before(cmd, m.start(), workdir) else {
            return Some(refused(UNKNOWN_CWD_MSG.into()));
        };
        for t in targets {
            let t = match expand_shell_path(&t) {
                ShellPath::Known(p) => p,
                ShellPath::Unknown => {
                    return Some(refused(format!(
                        "[safety net: recursive-delete target '{}' depends on a variable or substitution the gate cannot evaluate; refused. Give an explicit path.]",
                        head(&t, 60)
                    )));
                }
            };
            let (_, inside) = safe_path_from(&base, workdir, &t, &policy.allow_dirs);
            if !inside {
                return Some(refused(format!(
                    "[safety net: recursive-delete target '{}' is outside the working directory; refused. Recursive deletes inside the working directory are unrestricted; for files outside it use delete_file one at a time.]",
                    head(&t, 60)
                )));
            }
        }
    }
    if let Some(m) = FIND_DELETE_RE.captures(cmd) {
        let target = &m[1];
        let Some(base) = cwd_before(cmd, m.get(0).map_or(0, |w| w.start()), workdir) else {
            return Some(refused(UNKNOWN_CWD_MSG.into()));
        };
        let target = match expand_shell_path(target) {
            ShellPath::Known(p) => p,
            ShellPath::Unknown => {
                return Some(refused(format!(
                    "[safety net: the find -delete target '{}' depends on a variable or substitution the gate cannot evaluate; refused. Give an explicit path.]",
                    head(target, 60)
                )));
            }
        };
        let (_, inside) = safe_path_from(&base, workdir, &target, &policy.allow_dirs);
        if !inside {
            return Some(refused(format!(
                "[safety net: the find -delete target '{}' is outside the working directory; refused. find -delete inside the working directory is unrestricted.]",
                head(&target, 60)
            )));
        }
    }
    None
}

/// The gate with its per-run state ("allow all" grants).
pub struct Gate {
    pub policy: SafetyPolicy,
    pub confirmer: Box<dyn Confirmer>,
    allow_all: HashSet<&'static str>,
    /// The scope gate ([`crate::scope`], bead boubou-6x3): the turn's instruction class,
    /// set by the loop per turn; `None` is unarmed.
    scope: Option<Scope>,
    scope_mode: ScopeGateMode,
    /// The user allowed the drift this turn: no further asks.
    scope_cleared: bool,
    /// Shadow mode's would-have-asked notes, drained by the loop for its log.
    pub scope_notes: Vec<String>,
    /// Writes the user declined this turn, `(file name, declined path)` (bead boubou-sxx):
    /// gemma4:e4b answered a declined `/tmp/x.txt` by writing `x.txt` into the project 3/3
    /// and reporting success, whatever the refusal said. The same name at another path in
    /// the same turn is the substitution and is refused without asking.
    declined_writes: Vec<(String, PathBuf)>,
    /// The last refusal [`Gate::check`] issued, as data; `None` after a pass.
    last_refusal: Option<Refused>,
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

/// The `'static` name of a file tool, for the allow-all set.
fn file_tool(name: &str) -> &'static str {
    FILE_TOOLS
        .iter()
        .find(|t| **t == name)
        .copied()
        .unwrap_or("read_file")
}

impl Gate {
    pub fn new(policy: SafetyPolicy, confirmer: Box<dyn Confirmer>) -> Self {
        Self {
            policy,
            confirmer,
            allow_all: HashSet::new(),
            scope: None,
            scope_mode: ScopeGateMode::Off,
            scope_cleared: false,
            scope_notes: Vec::new(),
            declined_writes: Vec::new(),
            last_refusal: None,
        }
    }

    /// Why the last [`Gate::check`] refused, if it did.
    pub fn last_refusal(&self) -> Option<Refused> {
        self.last_refusal
    }

    /// Forget the last refusal (the executor calls this before a new tool call, so a call
    /// that fails before reaching the gate is not blamed on the previous one).
    pub fn clear_refusal(&mut self) {
        self.last_refusal = None;
    }

    /// Record a refusal and hand back its text.
    fn refuse(&mut self, kind: RefusalKind, by_user: bool, text: String) -> Option<String> {
        self.last_refusal = Some(Refused { kind, by_user });
        Some(text)
    }

    pub fn headless(policy: SafetyPolicy) -> Self {
        Self::new(policy, Box::new(DenyAll))
    }

    /// Arm the scope gate for a turn (`None` disarms); a new scope forgets an earlier
    /// turn's allowance.
    pub fn set_scope(&mut self, scope: Option<Scope>, mode: ScopeGateMode) {
        self.scope = scope.filter(|_| mode != ScopeGateMode::Off);
        self.scope_mode = mode;
        self.scope_cleared = false;
        self.declined_writes.clear();
    }

    /// Remember a write the user declined, so its name cannot reappear elsewhere this turn.
    fn note_declined(&mut self, real: &Path) {
        if let Some(name) = real.file_name().map(|n| n.to_string_lossy().into_owned()) {
            self.declined_writes.push((name, real.to_path_buf()));
        }
    }

    /// The refusal for a write that repeats a declined file name at another path.
    fn substitute_of(&self, real: &Path) -> Option<String> {
        let name = real.file_name()?.to_string_lossy();
        let (_, declined) = self
            .declined_writes
            .iter()
            .find(|(n, p)| *n == name && p.as_path() != real)?;
        Some(format!(
            "[safety gate: the user declined writing {name} at {}; writing it at {} instead is the same thing somewhere else. Say what you could not do and stop.]",
            declined.display(),
            real.display()
        ))
    }

    /// `_gate_check`: the refusal text, or `None` to let the call through. The five rings
    /// first; a call they pass is then held against the turn's scope.
    pub fn check(&mut self, inv: &ToolInvocation, workdir: &Path) -> Option<String> {
        self.last_refusal = None;
        if let Some(refusal) = self.rings(inv, workdir) {
            return Some(refusal);
        }
        self.scope_check(inv, workdir)
    }

    /// The scope gate: a producing call that leaves a tracker or docs instruction goes to
    /// the confirmer as "outside the instruction" (Allow clears the turn, Deny returns
    /// [`scope::deny_text`]); unattended refuses without asking; shadow mode only notes.
    fn scope_check(&mut self, inv: &ToolInvocation, workdir: &Path) -> Option<String> {
        if self.scope_cleared {
            return None;
        }
        let sc = self.scope.as_ref()?;
        let why = scope::out_of_scope(sc, inv)?;
        let target = match inv {
            ToolInvocation::RunBash { command } => format!("bash: {}", head(command, 60)),
            other => other.file_path().unwrap_or("").to_string(),
        };
        if self.scope_mode == ScopeGateMode::Shadow {
            self.scope_notes.push(format!(
                "would ask: {} {target} {why} under a {} instruction",
                inv.name(),
                sc.class.name()
            ));
            return None;
        }
        let text = scope::deny_text(sc, why);
        if self.policy.unattended {
            return self.refuse(RefusalKind::Scope, false, text);
        }
        let req = ConfirmRequest {
            tool: inv.name(),
            target,
            action: "outside the instruction",
            workdir,
        };
        match self.confirmer.confirm(&req) {
            Verdict::Allow | Verdict::AllowAll => {
                self.scope_cleared = true;
                None
            }
            Verdict::Deny => self.refuse(RefusalKind::Scope, true, text),
        }
    }

    fn rings(&mut self, inv: &ToolInvocation, workdir: &Path) -> Option<String> {
        let name = inv.name();
        if let ToolInvocation::RunBash { command } = inv {
            let low = command.to_lowercase();
            if danger_hit(&low).is_some() {
                let text = format!(
                    "[safety gate: the command contains a dangerous operation; refused. Original command: {}]",
                    head(command, 80)
                );
                return self.refuse(RefusalKind::Danger, false, text);
            }
            if low.contains("powershell") && PS_HIGH_RISK.iter().any(|p| low.contains(p)) {
                let text = format!(
                    "[safety gate: the powershell command contains a high-risk operation (download-and-execute / script injection / privilege escalation); refused. Original command: {}]",
                    head(command, 80)
                );
                return self.refuse(RefusalKind::Danger, false, text);
            }
            if !self.policy.unsafe_mode {
                if let Some(r) =
                    risk_refusal(command, workdir, &self.policy, self.confirmer.as_mut())
                {
                    return self.refuse(RefusalKind::Risk, r.by_user, r.text);
                }
            }
            let targets = resolved_write_targets(command, workdir, &self.policy.allow_dirs);
            if let Some(text) = targets
                .iter()
                .find_map(|(real, _)| self.substitute_of(real))
            {
                return self.refuse(RefusalKind::Substitute, true, text);
            }
            if self.allow_all.contains("run_bash") {
                return None;
            }
            if let Some((real, _)) = targets.into_iter().find(|(_, inside)| !inside) {
                return match ask(
                    &self.policy,
                    self.confirmer.as_mut(),
                    name,
                    real.display().to_string(),
                    workdir,
                    "access",
                    &boundary_reason(workdir),
                ) {
                    Ok(allok) => {
                        if allok {
                            self.allow_all.insert("run_bash");
                        }
                        None
                    }
                    Err(r) => {
                        if r.by_user {
                            self.note_declined(&real);
                        }
                        self.refuse(RefusalKind::Boundary, r.by_user, r.text)
                    }
                };
            }
            if matches_escape_pattern(&low) {
                return match ask(
                    &self.policy,
                    self.confirmer.as_mut(),
                    name,
                    format!("bash: {}", head(command, 60)),
                    workdir,
                    "run a command",
                    &format!("a command that reaches {}", boundary_reason(workdir)),
                ) {
                    Ok(allok) => {
                        if allok {
                            self.allow_all.insert("run_bash");
                        }
                        None
                    }
                    Err(r) => self.refuse(RefusalKind::Boundary, r.by_user, r.text),
                };
            }
            return None;
        }
        if FILE_TOOLS.contains(&name) {
            if let Some(path) = inv.file_path().filter(|p| !p.is_empty()) {
                let (real, _) = safe_path(workdir, path, &self.policy.allow_dirs);
                if WRITE_TOOLS.contains(&name) {
                    if let Some(text) = self.substitute_of(&real) {
                        return self.refuse(RefusalKind::Substitute, true, text);
                    }
                }
            }
            if self.allow_all.contains(name) {
                return None;
            }
            if let Some(path) = inv.file_path().filter(|p| !p.is_empty()) {
                let (real, inside) = safe_path(workdir, path, &self.policy.allow_dirs);
                if !inside {
                    return match ask(
                        &self.policy,
                        self.confirmer.as_mut(),
                        name,
                        real.display().to_string(),
                        workdir,
                        "access",
                        &boundary_reason(workdir),
                    ) {
                        Ok(allok) => {
                            if allok {
                                self.allow_all.insert(file_tool(name));
                            }
                            None
                        }
                        Err(r) => {
                            if r.by_user && WRITE_TOOLS.contains(&name) {
                                self.note_declined(&real);
                            }
                            self.refuse(RefusalKind::Boundary, r.by_user, r.text)
                        }
                    };
                }
                if is_sensitive_path(&real) {
                    if WRITE_TOOLS.contains(&name) {
                        let text = format!(
                            "[safety gate: path {} matches a sensitive pattern; writes are always refused.]",
                            real.display()
                        );
                        return self.refuse(RefusalKind::Sensitive, false, text);
                    }
                    return match ask(
                        &self.policy,
                        self.confirmer.as_mut(),
                        name,
                        real.display().to_string(),
                        workdir,
                        "read a sensitive file",
                        "a sensitive file",
                    ) {
                        Ok(allok) => {
                            if allok {
                                self.allow_all.insert(file_tool(name));
                            }
                            None
                        }
                        Err(r) => self.refuse(RefusalKind::Sensitive, r.by_user, r.text),
                    };
                }
            }
        }
        if WRITE_TOOLS.contains(&name) {
            if let Some(path) = inv.file_path() {
                let p = format!("{path}\\").to_lowercase();
                if SYSTEM_DIRS.iter().any(|d| p.contains(d)) {
                    let text = format!(
                        "[safety gate: the target path contains a system directory; refused. Path: {}]",
                        head(path, 80)
                    );
                    return self.refuse(RefusalKind::SystemDir, false, text);
                }
            }
        }
        None
    }
}
