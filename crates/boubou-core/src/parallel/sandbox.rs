//! The child-agent sandbox (`parallel_safety.py`): permissions strictly smaller than the
//! parent's, default-deny, no escalation path.
//!
//! The parent agent, even unattended, has a confirmation dialog as a last resort. A child
//! runs headless in a batch with nobody in the loop, so every path that the parent would
//! "ask and then allow" is a plain refusal here. Four layers, all data-driven, factory
//! defaults at the strictest setting:
//!
//! 1. tool face: read / write / list / finish / todo only, no commands, web, MCP or
//!    `enable_tools`;
//! 2. path boundary: writes outside the child's own partition directory are refused;
//!    sensitive locations are refused even for reads unless allow-listed; sensitive
//!    patterns inside the workdir are refused for writes too;
//! 3. dangerous-command blacklist: quote/escape normalisation, chained segments (`&& || ; |`)
//!    checked one by one, redirect targets checked as write paths;
//! 4. audit: one JSONL line per file write / command / refusal; ≥1 severe refusal
//!    terminates the child (exit 53), `max_denies_per_child` plain refusals do the same.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Map, Value};

use super::config::{CommandRule, ParallelConfig};
use crate::safety::safe_path;

/// Exit status of a child that hit the severe fuse. The dispatcher never retries it.
pub const SEVERE_EXIT_CODE: i32 = 53;

/// Patterns that are dangerous even inside the workdir: credentials, keys, VM disks, the
/// agent's own data files. Matched against the normalised (lower-case, backslash) path.
pub const DEFAULT_SENSITIVE_DIR_PATTERNS: &[&str] = &[
    // VM disks / images
    ".vhdx",
    ".vhd",
    ".vmdk",
    ".vdi",
    ".qcow2",
    ".vmx",
    ".vmsn",
    ".nvram",
    // credentials / keys / sensitive config
    ".ssh",
    ".aws",
    ".gnupg",
    ".gpg",
    ".env",
    "credentials",
    "credential",
    ".ollama",
    ".myagents",
    "id_rsa",
    "id_ed25519",
    ".pem",
    ".key",
    ".kube",
    "secrets",
    // the agent's own files and the host workspace
    "mcp.json",
    "config.json",
    "gui_prefs.json",
    "memory.json",
    "sessions",
    ".git",
    "node_modules",
    // Boubou's own data files
    "boubou.json",
    ".boubou",
];

/// Absolute system locations. Outside the workdir they are already refused by the
/// boundary rule; this list gives the audit line a precise reason and guards against a
/// workdir misconfigured into a system area. Deliberately *not* matched against relative
/// paths inside the workdir: a workdir under `AppData\Local\Temp` is legitimate.
pub const DEFAULT_SYSTEM_DIR_PATTERNS: &[&str] = &[
    // VMs / subsystems (a local model once deleted a user's WSL distribution)
    "\\\\wsl",
    "\\wsl$",
    "wsl.localhost",
    "\\wslg",
    "\\appdata\\local\\packages",
    "\\hyper-v",
    "\\virtualbox",
    "\\vmware",
    "oracle virtualbox",
    "\\vagrant",
    "\\podman",
    // system areas
    "\\windows",
    "\\program files",
    "\\programdata",
    "\\system32",
    "\\syswow64",
    "\\drivers",
    "\\boot\\",
    "\\efi\\",
    "\\recycler",
    "\\system volume information",
    // user areas (covered by the boundary rule; listed for the audit reason)
    "\\appdata",
    "\\application data",
    // POSIX system roots
    "\\etc\\",
];

/// Core system subset for the workdir-misconfiguration guard. No bare `\appdata`:
/// `AppData\Local\Temp` is a normal place for a workdir.
pub const DEFAULT_CORE_SYSTEM_PATTERNS: &[&str] = &[
    "\\windows",
    "\\program files",
    "\\programdata",
    "\\system32",
    "\\syswow64",
    "\\system volume information",
    "\\recycler",
    "\\efi\\",
    "\\boot\\",
    "\\\\wsl",
    "\\wsl$",
    "wsl.localhost",
    "\\appdata\\local\\packages",
    "\\hyper-v",
    "\\virtualbox",
    "\\vmware",
    "\\etc\\",
];

/// The factory dangerous-command blacklist for children (strictest tier).
pub fn default_dangerous_commands() -> Vec<CommandRule> {
    use CommandRule as R;
    vec![
        // VMs / subsystems (highest risk)
        R::severe(
            r"wsl\s+--unregister",
            "wsl --unregister deletes a whole distribution",
        ),
        R::severe(r"wsl\s+--import", "wsl --import overwrites a distribution"),
        R::severe(r"wsl\s+--unmount", "unmounts a wsl virtual disk"),
        R::deny(
            r"wsl\s+--manage.*--set-sparse|wsl\s+--export",
            "wsl management/export",
        ),
        R::deny(
            r"\bwsl\b",
            "a sub-agent should not touch wsl / the VM subsystem",
        ),
        R::severe(r"\bdiskpart\b", "disk partitioning tool"),
        R::severe(r"\bformat(\s|\.com)", "formats a drive"),
        R::severe(r"cipher\s+/w", "cipher /w wipes free space"),
        R::severe(r"\bbcdedit\b", "boot configuration"),
        R::severe(r"vssadmin\s+delete", "deletes shadow copies"),
        R::severe(r"\bwmic\s+.*(delete|shadowcopy)", "wmic delete operation"),
        R::severe(r"\bdism\b", "system image servicing"),
        R::deny(r"\bsfc\b", "system file checker"),
        // recursive / forced deletion
        R::severe(r"\brd\s+/s", "rd /s deletes a directory tree"),
        R::severe(r"\bdel\s+(/f|/s|/q)", "del forced/recursive delete"),
        R::severe(
            r"remove-item\b.*(-recurse|-force)",
            "PowerShell recursive forced delete",
        ),
        R::severe(
            r"\brm\s+(-[a-z]*r[a-z]*f|-[a-z]*f[a-z]*r|-[a-z]*r|-\-recursive)",
            "rm recursive delete",
        ),
        R::severe(r"\brm\s+-[a-z]*f", "rm forced delete"),
        R::severe(r"-delete\b", "find -delete recursive delete"),
        R::severe(r"(^|\s)dd\s+", "dd raw device write"),
        R::severe(r"\bmkfs", "creates a file system"),
        R::severe(r"robocopy\b.*(/mir|/purge|/mov)", "robocopy mirror/purge"),
        R::deny(r"\bxcopy\b.*(/y|/q)", "xcopy overwriting copy"),
        // registry / services / network
        R::severe(
            r"\breg(\.exe)?\s+(add|delete|import|restore|load|unload|save|export)",
            "registry write/delete",
        ),
        R::deny(r"\breg(\.exe)?\s+query", "registry read"),
        R::severe(
            r"\bsc(\.exe)?\s+(create|delete|config|stop)",
            "service management",
        ),
        R::severe(r"\bschtasks\b", "scheduled task (persistence)"),
        R::severe(r"\bnetsh\b", "network stack configuration"),
        R::severe(
            r"\bnet\s+(user|localgroup|share)\b",
            "user/share management",
        ),
        R::severe(r"\bshutdown\b", "shutdown"),
        R::deny(r"\btaskkill\b", "kills processes"),
        R::severe(
            r"\bbcdboot\b|\breagentc\b|\bsetx\b",
            "system-level configuration",
        ),
        R::severe(r"\bmklink\b", "symbolic link (can point at system paths)"),
        R::deny(
            r"\b attrib\s*\+s|\b attrib\s*\+h",
            "hidden/system attributes",
        ),
        // remote / download-and-execute
        R::severe(
            r"(curl|wget|invoke-webrequest|invoke-restmethod|iex|invoke-expression|irm)\b.*(\|\s*(bash|sh|powershell|iex))",
            "download and execute",
        ),
        R::deny(r"(curl|wget)\b", "a sub-agent has no need for the network"),
        R::deny(
            r"\bpip\s+install|\bnpm\s+install|\bnpx\b",
            "installing dependencies is not a sub-agent's job",
        ),
        // privilege escalation
        R::severe(
            r"\bsudo\b|\brunas\b|\bstart-process\b.*-verb\s+runas",
            "privilege escalation",
        ),
        R::severe(
            r"\btakeown\b|\bicacls\b|\bcacls\b",
            "changes ownership/ACLs",
        ),
        // POSIX equivalents
        R::severe(
            r"\bchmod\s+(-r|--recursive)\b|\bchown\s+(-r|--recursive)\b",
            "recursive permission/owner change",
        ),
        R::severe(
            r"\blaunchctl\s+(remove|disable|unload)\b|\bsystemctl\s+(stop|disable|mask)\b",
            "system services",
        ),
    ]
}

/// Tools whose very invocation by a child is severe (commands, network, deletion,
/// self-escalation).
const SEVERE_TOOLS: [&str; 10] = [
    "run_bash",
    "run_command",
    "shell",
    "bash",
    "delete_file",
    "web_search",
    "web_fetch",
    "mcp_call",
    "enable_tools",
    "web_search_multi",
];
const COMMAND_TOOLS: [&str; 4] = ["run_bash", "run_command", "shell", "bash"];
const WRITE_TOOLS: [&str; 4] = ["create_file", "edit_file", "append_file", "delete_file"];
const READ_TOOLS: [&str; 3] = ["read_file", "list_dir", "search_files"];

static CHAIN_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&&|\|\||;|\||\n|&").expect("regex"));
static REDIRECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\d)?>{1,2}\s*([^\s&|;]+)|\b(?:out-file|tee)\s+([^\s&|;]+)").expect("regex")
});
static ESCAPED_QUOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\\["']"#).expect("regex"));
static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("regex"));
static DRIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]:[\\/]").expect("regex"));

// ---------------- normalisation ----------------

/// `norm_path`: strip quotes, lower-case, `/` → `\`, collapse repeated separators.
pub fn norm_path(p: &str) -> String {
    let s = p
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .replace('/', "\\");
    let mut out = String::with_capacity(s.len());
    let mut prev_sep = false;
    for ch in s.chars() {
        if ch == '\\' {
            if prev_sep {
                continue;
            }
            prev_sep = true;
        } else {
            prev_sep = false;
        }
        out.push(ch);
    }
    out.to_lowercase()
}

/// `norm_command`: drop quotes and escapes (cmd `^`, PowerShell backtick), fold whitespace,
/// lower-case, so `wsl --unregister "Ubuntu"`, `w^sl --unregister Ubuntu` and
/// `WSL  --UNREGISTER  Ubuntu` all land on the same string.
pub fn norm_command(cmd: &str) -> String {
    let mut s = cmd.to_string();
    for q in ['\u{201c}', '\u{201d}', '\u{2018}', '\u{2019}'] {
        s = s.replace(q, " ");
    }
    s = s.replace(['^', '`'], "");
    s = ESCAPED_QUOTE.replace_all(&s, " ").into_owned();
    s = s.replace(['"', '\''], " ");
    s = WS.replace_all(&s, " ").into_owned();
    s.trim().to_lowercase()
}

/// `split_chain`: `&& || ; | &` and newlines all split (each segment is checked alone).
pub fn split_chain(cmd: &str) -> Vec<String> {
    CHAIN_SPLIT
        .split(cmd)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// `redirect_targets`: `>`/`>>`/`2>` targets plus `tee` / `Out-File` arguments — the other
/// way of saying "write here".
pub fn redirect_targets(cmd: &str) -> Vec<String> {
    REDIRECT
        .captures_iter(cmd)
        .filter_map(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .map(|m| m.as_str().trim().to_string())
        })
        .filter(|t| !t.is_empty() && !matches!(t.as_str(), "nul" | "/dev/null" | "null"))
        .collect()
}

/// `match_sensitive`: the first pattern whose normalised form is a substring of `norm`.
pub fn match_sensitive<'a>(norm: &str, patterns: &'a [String]) -> Option<&'a str> {
    patterns.iter().map(String::as_str).find(|pat| {
        let p = norm_path(pat);
        !p.is_empty() && norm.contains(&p)
    })
}

/// `resolve_inside`: `(real_absolute, inside_workdir)`. `..` is collapsed and symlinks are
/// followed so a link inside the workdir cannot become an escape hatch. On POSIX hosts a
/// Windows-style path (`C:\…`, `\\server\…`) is outside by definition and backslashes in a
/// relative path count as separators, because that is what the model meant.
pub fn resolve_inside(workdir: &Path, path: &str) -> (PathBuf, bool) {
    if cfg!(windows) {
        return safe_path(workdir, path, &[]);
    }
    let fixed = path.replace('\\', "/");
    if DRIVE.is_match(&fixed) || fixed.starts_with("//") {
        return (PathBuf::from(path), false);
    }
    safe_path(workdir, &fixed, &[])
}

// ---------------- command rules ----------------

/// Compiled dangerous-command rules (invalid patterns are skipped, as in Mingbird).
#[derive(Debug, Clone)]
pub struct RuleSet {
    rules: Vec<(Regex, CommandRule)>,
}

impl RuleSet {
    pub fn compile(rules: &[CommandRule]) -> Self {
        Self {
            rules: rules
                .iter()
                .filter_map(|r| Regex::new(&r.pattern).ok().map(|re| (re, r.clone())))
                .collect(),
        }
    }

    /// The configured blacklist (factory default when empty) plus the `extra_*` rules.
    pub fn from_config(cfg: &ParallelConfig) -> Self {
        let mut rules = if cfg.dangerous_command_patterns.is_empty() {
            default_dangerous_commands()
        } else {
            cfg.dangerous_command_patterns.clone()
        };
        rules.extend(cfg.extra_dangerous_command_patterns.iter().cloned());
        Self::compile(&rules)
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// `match_command`: the first rule matching a normalised command segment.
    pub fn find(&self, segment_norm: &str) -> Option<&CommandRule> {
        self.rules
            .iter()
            .find(|(re, _)| re.is_match(segment_norm))
            .map(|(_, r)| r)
    }
}

// ---------------- verdicts ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildVerdict {
    Allow,
    /// Suspicious / out of bounds: counted, `max_denies_per_child` of them terminate.
    Deny,
    /// Destructive / irreversible / system level: terminates the child at once.
    Severe,
}

impl ChildVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            ChildVerdict::Allow => "allow",
            ChildVerdict::Deny => "deny",
            ChildVerdict::Severe => "severe",
        }
    }

    fn from_severity(s: &str) -> Self {
        if s == "severe" {
            ChildVerdict::Severe
        } else {
            ChildVerdict::Deny
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

impl Access {
    fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
        }
    }
}

/// Result of [`ChildSandbox::check_tool`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCheck {
    pub verdict: ChildVerdict,
    pub reason: String,
    /// What was judged: the resolved path, the command, or the tool name.
    pub target: String,
}

impl ToolCheck {
    pub fn allowed(&self) -> bool {
        self.verdict == ChildVerdict::Allow
    }
}

/// The child-side enforced sandbox: default-deny, no escalation, audited, with the
/// dangerous-behaviour counters that trip the fuse.
#[derive(Debug, Clone)]
pub struct ChildSandbox {
    pub workdir: PathBuf,
    pub audit_path: PathBuf,
    pub allowed_tools: BTreeSet<String>,
    sensitive: Vec<String>,
    system_dirs: Vec<String>,
    read_allow: Vec<String>,
    rules: RuleSet,
    max_denies: usize,
    /// The workdir itself sits in a core system area: every write is refused.
    pub workdir_is_system_area: bool,
    pub denies: usize,
    pub severes: usize,
    pub events: usize,
    terminated: bool,
}

fn or_default(user: &[String], dflt: &[&str]) -> Vec<String> {
    if user.is_empty() {
        dflt.iter().map(|s| s.to_string()).collect()
    } else {
        user.to_vec()
    }
}

impl ChildSandbox {
    pub fn new(workdir: impl Into<PathBuf>, cfg: &ParallelConfig) -> Self {
        let workdir: PathBuf = workdir.into();
        let workdir = std::path::absolute(&workdir).unwrap_or(workdir);
        let allowed: Vec<String> = if cfg.child_allowed_tools.is_empty() {
            super::config::DEFAULT_CHILD_ALLOWED_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            cfg.child_allowed_tools.clone()
        };
        let core = or_default(&cfg.core_system_dir_patterns, DEFAULT_CORE_SYSTEM_PATTERNS);
        let workdir_is_system_area =
            match_sensitive(&norm_path(&workdir.to_string_lossy()), &core).is_some();
        Self {
            audit_path: workdir.join("audit.log"),
            allowed_tools: allowed.into_iter().collect(),
            sensitive: or_default(&cfg.sensitive_dir_patterns, DEFAULT_SENSITIVE_DIR_PATTERNS),
            system_dirs: or_default(&cfg.system_dir_patterns, DEFAULT_SYSTEM_DIR_PATTERNS),
            read_allow: cfg.read_allow_patterns.clone(),
            rules: RuleSet::from_config(cfg),
            max_denies: cfg.max_denies_per_child,
            workdir_is_system_area,
            denies: 0,
            severes: 0,
            events: 0,
            terminated: false,
            workdir,
        }
    }

    pub fn with_audit_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.audit_path = p.into();
        self
    }

    pub fn allowed_tools_sorted(&self) -> Vec<String> {
        self.allowed_tools.iter().cloned().collect()
    }

    // ---- audit ----
    fn audit(&mut self, kind: &str, target: &str, verdict: ChildVerdict, reason: &str) {
        self.events += 1;
        let mono = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_secs_f64() * 1000.0).round() / 1000.0)
            .unwrap_or(0.0);
        let rec = json!({
            "ts": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "mono": mono,
            "kind": kind,
            "target": crate::text::head(target, 300),
            "verdict": verdict.as_str(),
            "severity": verdict.as_str(),
            "reason": reason,
            "workdir": self.workdir.to_string_lossy(),
        });
        // A failed audit write never blocks the run (the file lives in the workdir).
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit_path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{rec}");
        }
    }

    // ---- counters and the fuse ----
    pub fn should_terminate(&self) -> bool {
        self.terminated
    }

    fn register(&mut self, verdict: ChildVerdict) {
        match verdict {
            ChildVerdict::Severe => {
                self.severes += 1;
                self.terminated = true;
            }
            ChildVerdict::Deny => {
                self.denies += 1;
                if self.denies >= self.max_denies {
                    self.terminated = true;
                }
            }
            ChildVerdict::Allow => {}
        }
    }

    // ---- the three checks ----

    /// Layer 1, the tool face. `None` when allowed; otherwise the verdict (severe for
    /// commands / network / deletion / self-escalation, deny for the rest) and reason.
    pub fn check_tool_allowed(&self, name: &str) -> Option<(ChildVerdict, String)> {
        if self.allowed_tools.contains(name) {
            return None;
        }
        let v = if SEVERE_TOOLS.contains(&name) {
            ChildVerdict::Severe
        } else {
            ChildVerdict::Deny
        };
        Some((
            v,
            format!(
                "tool_not_allowed(the sub-agent tool face is narrowed to {:?}; {name} is unavailable, do not try to work around it)",
                self.allowed_tools_sorted()
            ),
        ))
    }

    /// Layer 2, path boundary and sensitive patterns. Writes: workdir only and no
    /// sensitive hit. Reads: sensitive needs an allow-list entry. Order: outside the workdir
    /// → write severe / read deny (with the system-area reason when one matches); inside →
    /// only the "meaningful inside the workdir" list is consulted.
    pub fn check_path(&self, path: &str, action: Access) -> (ChildVerdict, PathBuf, String) {
        let (real, inside) = resolve_inside(&self.workdir, path);
        let norm = norm_path(&real.to_string_lossy());
        if !inside {
            let verdict = if action == Access::Write {
                ChildVerdict::Severe
            } else {
                ChildVerdict::Deny
            };
            let why = match_sensitive(&norm, &self.system_dirs)
                .map(|h| format!("sensitive_path({h}) "))
                .unwrap_or_default();
            return (
                verdict,
                real,
                format!(
                    "{why}outside_workdir(anything outside the workdir is refused{})",
                    action.as_str()
                ),
            );
        }
        if self.workdir_is_system_area && action == Access::Write {
            return (
                ChildVerdict::Severe,
                real,
                "workdir_misconfigured_into_system_area(write refused)".to_string(),
            );
        }
        if let Some(hit) = match_sensitive(&norm, &self.sensitive) {
            if action == Access::Write {
                return (
                    ChildVerdict::Severe,
                    real,
                    format!("sensitive_path({hit}) write to a sensitive path"),
                );
            }
            let allowed = self.read_allow.iter().any(|a| {
                let n = norm_path(a);
                !n.is_empty() && norm.contains(&n)
            });
            if allowed {
                return (ChildVerdict::Allow, real, String::new());
            }
            return (
                ChildVerdict::Deny,
                real,
                format!("sensitive_path({hit}) read of a sensitive path (no allow-list)"),
            );
        }
        (ChildVerdict::Allow, real, String::new())
    }

    /// Layer 3, the dangerous-command blacklist: normalise, check the whole line (catches
    /// `curl … | bash`, which chain splitting would cut apart), then every chained segment,
    /// then every redirect target as a write path.
    pub fn check_command(&self, command: &str) -> (ChildVerdict, String, String) {
        let norm_all = norm_command(command);
        let mut scopes = vec![norm_all.clone()];
        let segs = split_chain(&norm_all);
        if segs.is_empty() {
            scopes.push(norm_all.clone());
        } else {
            scopes.extend(segs);
        }
        for scope in &scopes {
            if let Some(rule) = self.rules.find(scope) {
                let why = if rule.reason.is_empty() {
                    rule.pattern.clone()
                } else {
                    rule.reason.clone()
                };
                return (
                    ChildVerdict::from_severity(&rule.severity),
                    command.to_string(),
                    format!("dangerous_command({why})"),
                );
            }
        }
        for tgt in redirect_targets(&norm_all) {
            let (verdict, _, reason) = self.check_path(&tgt, Access::Write);
            if verdict != ChildVerdict::Allow {
                return (
                    verdict,
                    command.to_string(),
                    format!("redirect_target({reason})"),
                );
            }
        }
        (ChildVerdict::Allow, command.to_string(), String::new())
    }

    /// The single entry point: tool face → command → path. Every judged call is audited;
    /// an allowed tool that takes neither a path nor a command (`todo`, `finish`) passes
    /// without touching the counters.
    pub fn check_tool(&mut self, name: &str, args: &Map<String, Value>) -> ToolCheck {
        if let Some((sev, reason)) = self.check_tool_allowed(name) {
            self.register(sev);
            self.audit("tool", name, sev, &reason);
            return ToolCheck {
                verdict: sev,
                reason,
                target: name.to_string(),
            };
        }
        if COMMAND_TOOLS.contains(&name) {
            let cmd = args.get("command").map(arg_str).unwrap_or_default();
            let (verdict, target, reason) = self.check_command(&cmd);
            self.register(verdict);
            self.audit("command", &target, verdict, &reason);
            return ToolCheck {
                verdict,
                reason,
                target,
            };
        }
        let action = if WRITE_TOOLS.contains(&name) {
            Access::Write
        } else if READ_TOOLS.contains(&name) {
            Access::Read
        } else {
            return ToolCheck {
                verdict: ChildVerdict::Allow,
                reason: String::new(),
                target: name.to_string(),
            };
        };
        let path = ["path", "dir", "filepath"]
            .iter()
            .filter_map(|k| args.get(*k))
            .map(arg_str)
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        let (verdict, real, reason) = self.check_path(&path, action);
        self.register(verdict);
        let target = if real.as_os_str().is_empty() {
            path
        } else {
            real.to_string_lossy().into_owned()
        };
        self.audit(action.as_str(), &target, verdict, &reason);
        ToolCheck {
            verdict,
            reason,
            target,
        }
    }
}

fn arg_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ---------------- harness-side readers ----------------

/// `read_audit`: the child's JSONL audit log; missing file or bad lines → skipped.
pub fn read_audit(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// `audit_summary`: counts plus the first ten dangerous entries (severe first).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditSummary {
    pub events: usize,
    pub denies: usize,
    pub severes: usize,
    pub detail: Vec<Value>,
}

pub fn audit_summary(events: &[Value]) -> AuditSummary {
    fn verdict(e: &Value) -> &str {
        e.get("verdict").and_then(Value::as_str).unwrap_or("")
    }
    let denies: Vec<&Value> = events.iter().filter(|e| verdict(e) == "deny").collect();
    let severes: Vec<&Value> = events.iter().filter(|e| verdict(e) == "severe").collect();
    let detail = severes
        .iter()
        .chain(denies.iter())
        .take(10)
        .map(|e| {
            json!({
                "target": e.get("target").cloned().unwrap_or(Value::Null),
                "severity": e.get("severity").cloned().unwrap_or(Value::Null),
                "reason": e.get("reason").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    AuditSummary {
        events: events.len(),
        denies: denies.len(),
        severes: severes.len(),
        detail,
    }
}
