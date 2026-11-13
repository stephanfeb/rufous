//! Tool execution inside the workspace (`run_tool`). Every call goes alias → type
//! coercion → typed decode → safety gate → dispatch. Ring 4 of the safety model lives
//! here: `.bak` before edits, `.mingbird_trash/` instead of unlink, and the overwrite guard.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

use crate::config::agent_home;
use crate::edit_match;
use crate::guards::format_bash_out;
use crate::hooks::Hooks;
use crate::mcp::{self, McpRegistry};
use crate::memory;
use crate::parallel::{ChildSandbox, ParallelConfig};
use crate::safety::{resolve_path, safe_path, Gate, RefusalKind, Refused, SafetyPolicy};
use crate::skills::{Skill, SkillSet};
use crate::text::{self, clen, head};
use crate::todo::run_todo;
use crate::tools::{
    self, coerce_args, coerce_object, is_mcp_tool, resolve_alias, SkillsAction, ToolInvocation,
    ADVANCED_TOOL_NAMES, CORE_TOOL_NAMES,
};
use crate::web::Web;
use crate::Error;

/// Recycle bin inside the workspace. Kept as Mingbird's name so old workspaces stay readable.
pub const TRASH_DIR: &str = ".mingbird_trash";

/// The result for a path the OS refused (`EPERM`, `EACCES`). Inside a host app's sandbox
/// that is a folder the user did not share with it (boubou-f8d); said apart from "not
/// found", since a small model told `[not a dir]` went searching elsewhere.
fn no_access(p: &Path) -> String {
    format!(
        "[no access: {} (not shared with this assistant)]",
        p.display()
    )
}

/// Did the OS refuse access, as opposed to the path being missing or wrong?
fn denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}

/// A store or disk error as a tool result: [`no_access`] for a refusal, else the error.
fn io_failure(p: &Path, e: std::io::Error) -> String {
    if denied(&e) {
        no_access(p)
    } else {
        format!("[tool error: {e}]")
    }
}

/// Where the text-file tools read and write the project: `read_file` (text), `create_file`,
/// `edit_file` and `append_file` go through this seam, so a host that owns the files can
/// stand in for the disk. The disk is the default ([`DiskStore`]); the ACP server supplies
/// one that asks the editor (`fs/read_text_file`, `fs/write_text_file`), so an unsaved
/// buffer is what the model reads and the edit lands in the buffer rather than beneath it.
/// Existence checks, the overwrite guard's size, images, `delete_file`, `list_dir`,
/// `search_files`, commands and their backups stay on disk: the protocol has no method for
/// them, and a path is still a path on this machine.
pub trait FileStore: Send {
    /// The whole text of `path`, decoded leniently; `NotFound` when there is no such file.
    fn read(&mut self, path: &Path) -> std::io::Result<String>;
    /// Replace the file with `content`, creating it (and its directories) if needed.
    fn write(&mut self, path: &Path, content: &str) -> std::io::Result<()>;
    /// Add `content` at the end, creating the file if needed.
    fn append(&mut self, path: &Path, content: &str) -> std::io::Result<()> {
        let mut s = match self.read(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        s.push_str(content);
        self.write(path, &s)
    }
}

/// The default [`FileStore`]: the file system, as the CLI and the bench have always used it.
#[derive(Debug, Default, Clone, Copy)]
pub struct DiskStore;

impl FileStore for DiskStore {
    fn read(&mut self, path: &Path) -> std::io::Result<String> {
        std::fs::read(path).map(|raw| text::decode_lenient(&raw))
    }
    fn write(&mut self, path: &Path, content: &str) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, content)
    }
    fn append(&mut self, path: &Path, content: &str) -> std::io::Result<()> {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .and_then(|mut f| f.write_all(content.as_bytes()))
    }
}

/// Opens the clause a file tool's result carries when the workdir-prefix rule rewrote its
/// path; [`rebased_path`] reads it back for the harness log.
pub const PATH_REBASED_MARK: &str = "(path rebased: ";

fn first_segment(path: &str) -> &str {
    path.split(['/', '\\']).next().unwrap_or(path)
}

/// The workdir-prefix rule ([`crate::profile::ModelProfile::rebase_workdir_prefix`]): for
/// a relative `path` whose first segment is `workdir`'s own name, when `<workdir>/<name>`
/// is not a directory, the path with that segment removed (`"."` when nothing is left).
/// `None` when the rule does not apply: an absolute path, another first segment, or a real
/// subdirectory of that name (a package named like its repository is common).
pub fn rebase_workdir_prefix(workdir: &Path, path: &str) -> Option<String> {
    if Path::new(path).is_absolute() {
        return None;
    }
    let name = workdir.file_name()?.to_str()?;
    let trimmed = path.trim_start_matches("./");
    let first = first_segment(trimmed);
    if first.is_empty() || first != name || workdir.join(first).is_dir() {
        return None;
    }
    let rest = trimmed[first.len()..].trim_start_matches(['/', '\\']);
    Some(if rest.is_empty() {
        ".".to_string()
    } else {
        rest.to_string()
    })
}

/// `(as given, as used)` from a tool result that carries [`PATH_REBASED_MARK`].
pub fn rebased_path(res: &str) -> Option<(&str, &str)> {
    let i = res.rfind(PATH_REBASED_MARK)? + PATH_REBASED_MARK.len();
    let body = res[i..].split(';').next()?;
    let (from, to) = body.split_once(" → ")?;
    Some((from.trim(), to.trim()))
}
/// Under a relocated harness directory: previous versions of edited files, mirrored by
/// their path in the workdir (`backups/lib/a.py.bak`).
pub const BACKUP_DIR: &str = "backups";
/// Where `web_fetch` keeps full page texts: under the workdir, or under the harness
/// directory when relocated (bead boubou-m64.4: fetch dumps were landing in the user's
/// project under an editor).
pub const SOURCES_DIR: &str = "sources";
/// Under a relocated harness directory: deleted files.
pub const RELOCATED_TRASH_DIR: &str = "trash";
pub const IMG_EXTS: [&str; 6] = [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp"];
use crate::agent::history::{ImageQueued, IMG_MAX_BYTES, IMG_PER_TURN};
const CLOBBER_MIN_OLD: u64 = 300;
const CLOBBER_RATIO: f64 = 0.5;
const READ_CHARS: usize = 6000;

/// Crawl guard v2: a byte budget per file; past it reads are throttled, never refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrawlConfig {
    /// budget = clamp(mult × file size, floor, cap)
    pub budget_mult: u64,
    pub budget_floor: u64,
    pub budget_cap: u64,
    /// chars returned per read once the budget is spent
    pub throttle: usize,
    /// read counts at which a one-line suggestion is prepended (at most two per file)
    pub nudge_at: [u32; 2],
}

impl Default for CrawlConfig {
    fn default() -> Self {
        Self {
            budget_mult: 2,
            budget_floor: 64 * 1024,
            budget_cap: 512 * 1024,
            throttle: 2000,
            nudge_at: [4, 10],
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrawlEntry {
    pub reads: u32,
    pub served_bytes: u64,
    pub nudges: u32,
    pub throttle_notified: bool,
}

/// What a tool call came to, as data beside its text (bead boubou-bj4). The text is for the
/// model and changes under measurement; the loop, the session status, the integrity record
/// and the bench read this. [`Executor::run_tool`] sets it from the gate and the hooks
/// directly and from a tool's own result through [`ToolStatus::of_text`], the one place
/// that knows the executor's result openings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// The tool did what was asked (a shell command that ran counts, whatever its exit).
    Ok,
    /// A write that changed nothing: the edit was already in the file.
    NoChange,
    /// The tool could not do it: bad arguments, a missing file, an edit text not found, an
    /// I/O error, the overwrite guard, a network or MCP error.
    Error,
    /// A gate refused it before it ran.
    Refused(Refused),
}

/// Result openings the executor writes for a call that did not do what was asked. New
/// openings go here; `of_text` is the only reader.
const ERROR_OPENINGS: [&str; 19] = [
    "[tool error",
    "[not run:",
    "[interrupted:",
    "[offline:",
    "[no access",
    "[edit failed",
    "[overwrite guard",
    "[not found",
    "[not a file",
    "[not a dir",
    "[unknown tool",
    "[invalid tool name",
    "[mcp_call error",
    "[MCP server",
    "[web_fetch error",
    "[web_search error",
    "[spawn failed",
    "[skill '",
    "[tool ",
];

impl ToolStatus {
    /// The status a result text stands for. Used by the executor for its own tools' results
    /// (the gate's and the hooks' refusals are typed before any text exists) and by the loop
    /// for a [`crate::agent::run::ToolRunner`] that reports no status (test doubles); the
    /// kinds it assigns to a refusal text it did not issue are the common ones.
    pub fn of_text(res: &str) -> Self {
        let r = res.trim_start();
        if r.starts_with("[no change") {
            return Self::NoChange;
        }
        if r.starts_with("[blocked: path out of bounds")
            || r.starts_with("[safety gate")
            || r.starts_with("[child safety gate")
        {
            return Self::Refused(Refused::gate(RefusalKind::Boundary));
        }
        if r.starts_with("[scope gate") {
            return Self::Refused(Refused::gate(RefusalKind::Scope));
        }
        if r.starts_with("[safety net") {
            return Self::Refused(Refused::gate(RefusalKind::Risk));
        }
        if ERROR_OPENINGS.iter().any(|o| r.starts_with(o)) {
            return Self::Error;
        }
        Self::Ok
    }

    /// The log tag after the result head: `error`, `nochange`, `refused:<kind>[:user]`;
    /// none for a call that did its job.
    pub fn tag(&self) -> Option<String> {
        match self {
            Self::Ok => None,
            Self::NoChange => Some("nochange".to_string()),
            Self::Error => Some("error".to_string()),
            Self::Refused(r) => Some(format!(
                "refused:{}{}",
                r.kind.tag(),
                if r.by_user { ":user" } else { "" }
            )),
        }
    }
}

pub struct Executor {
    pub workdir: PathBuf,
    /// Where the harness's own files go (`todo.json`, backups, trash, the image queue):
    /// the workdir itself in Mingbird's layout, or a directory outside the project for an
    /// editor session (`RunParams::harness_dir`). Relocated, the safety net stays but out
    /// of sight: no `.bak` beside the file, no `.mingbird_trash/`, and tool results do not
    /// name where the copies went.
    pub harness_dir: PathBuf,
    pub gate: Gate,
    pub crawl_cfg: CrawlConfig,
    pub crawl: HashMap<String, CrawlEntry>,
    /// `_SLIM_OUTPUT_MODE`: trim long command output to its tail.
    pub slim_output: bool,
    /// Profile rule ([`crate::profile::ModelProfile::rebase_workdir_prefix`]): a relative
    /// path whose first segment is the workdir's own name, when no such subdirectory
    /// exists, is taken relative to the workdir. Applied once, in [`Self::dispatch`], to
    /// every file tool; the result carries [`PATH_REBASED_MARK`].
    pub rebase_workdir_prefix: bool,
    /// `edit_file`'s matcher ([`crate::edit_match`]); off (ablation `edit_match`), the
    /// inherited exact-after-unescape matcher with the file-head preview.
    pub edit_match: bool,
    /// Where the text-file tools read and write ([`FileStore`]): the disk unless a host
    /// owns the files (the ACP server's editor buffers).
    pub store: Box<dyn FileStore>,
    /// The run's command hooks ([`crate::hooks`]): `PreToolUse` and `PostToolUse` fire
    /// here, around every call; none until the loop hands them over.
    pub hooks: Hooks,
    /// Advanced tools the model has enabled, in enable order.
    pub enabled: Vec<String>,
    pub bash_timeout: Duration,
    pub agent_home: PathBuf,
    pub install_dir: PathBuf,
    /// The skills compiled into the product ([`Skill::embedded`]), below the directories.
    pub builtin_skills: Vec<Skill>,
    /// Network side of the web tools (search backends, fetch, result cache).
    pub web: Web,
    /// MCP servers and their probed manifest; an empty registry when none are configured.
    pub mcp: Arc<McpRegistry>,
    /// Ring 0: installed in a dispatched child (`AGENT_CHILD_SANDBOX=1`), checked first.
    pub child: Option<ChildSandbox>,
    /// Set once the child sandbox trips its fuse; the loop ends the run with exit 53.
    child_terminated: Option<String>,
    /// What the last [`Self::run_tool`] came to.
    last_status: ToolStatus,
    /// The agent's stop flag: raised, a running command's process tree is terminated at
    /// the next poll and the tool returns what it had.
    pub stop: Option<Arc<AtomicBool>>,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor")
            .field("workdir", &self.workdir)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl Executor {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self::with_policy(workdir, SafetyPolicy::from_env())
    }

    pub fn with_policy(workdir: impl Into<PathBuf>, policy: SafetyPolicy) -> Self {
        let install_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let workdir: PathBuf = workdir.into();
        Self {
            harness_dir: workdir.clone(),
            workdir,
            gate: Gate::headless(policy),
            crawl_cfg: CrawlConfig::default(),
            crawl: HashMap::new(),
            slim_output: false,
            rebase_workdir_prefix: false,
            edit_match: true,
            store: Box::new(DiskStore),
            hooks: Hooks::none(),
            stop: None,
            enabled: Vec::new(),
            bash_timeout: Duration::from_secs(
                std::env::var("AGENT_BASH_TIMEOUT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(120),
            ),
            web: Web::new(&agent_home()),
            mcp: Arc::new(McpRegistry::new(&agent_home(), mcp::McpConfig::new())),
            agent_home: agent_home(),
            install_dir,
            builtin_skills: Vec::new(),
            child: None,
            last_status: ToolStatus::Ok,
            child_terminated: None,
        }
    }

    /// `install_child_mode`: default-deny sandbox, audit, fuse; the confirmation channel is
    /// short-circuited (nobody is in the loop of a headless child). Returns the tool names
    /// the child may use, for the tool face.
    pub fn install_child_mode(&mut self, cfg: &ParallelConfig) -> Vec<String> {
        const NEVER_WITHOUT_COMMANDS: [&str; 16] = [
            "run_bash",
            "run_command",
            "shell",
            "bash",
            "delete_file",
            "edit_file",
            "append_file",
            "web_search",
            "web_fetch",
            "web_search_multi",
            "mcp_call",
            "enable_tools",
            "memory_store",
            "memory_recall",
            "batch_tools",
            "search_files",
        ];
        let mut allowed: Vec<String> = if cfg.child_allowed_tools.is_empty() {
            crate::parallel::config::DEFAULT_CHILD_ALLOWED_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            cfg.child_allowed_tools.clone()
        };
        if !cfg.child_allow_commands {
            allowed.retain(|t| !NEVER_WITHOUT_COMMANDS.contains(&t.as_str()));
        }
        let mut c = cfg.clone();
        c.child_allowed_tools = allowed.clone();
        self.child = Some(ChildSandbox::new(&self.workdir, &c));
        let mut policy = self.gate.policy.clone();
        policy.unattended = true;
        self.gate = Gate::headless(policy);
        allowed
    }

    /// The fuse reason once a child sandbox has decided the process must end (exit 53).
    pub fn child_terminated(&self) -> Option<String> {
        self.child_terminated.clone()
    }

    /// Attach the MCP registry (shared with the loop, which asks it for tool definitions).
    pub fn with_mcp(mut self, mcp: Arc<McpRegistry>) -> Self {
        mcp.offline.store(self.web.offline, Ordering::Relaxed);
        self.mcp = mcp;
        self
    }

    /// Offline mode for this executor: the web tools and MCP servers on other machines are
    /// refused when called, not only hidden from the face (docs/egress.md).
    pub fn set_offline(&mut self, on: bool) {
        self.web.offline = on;
        self.mcp.offline.store(on, Ordering::Relaxed);
    }

    /// The hosts contacted since the last call: the web client's and the MCP registry's.
    pub fn take_egress(&self) -> Vec<crate::egress::Egress> {
        let mut out = self.web.egress.take();
        for e in self.mcp.egress.take() {
            if !out.contains(&e) {
                out.push(e);
            }
        }
        out
    }

    /// Load the session's MCP servers ([`mcp::load_for`]: the home `mcp.json`, then the
    /// workdir's `.mcp.json`) and attach them.
    pub fn with_configured_mcp(self) -> Self {
        let servers = mcp::load_for(&self.agent_home, &self.install_dir, &self.workdir);
        let reg = McpRegistry::new(&self.agent_home, servers);
        self.with_mcp(Arc::new(reg))
    }

    /// Replace the server set (a host adding its own for the session, as an ACP client
    /// does in `session/new`); the registry is rebuilt, the manifest cache is shared.
    pub fn set_mcp_servers(&mut self, servers: mcp::McpConfig) {
        let reg = McpRegistry::new(&self.agent_home, servers);
        reg.offline.store(self.web.offline, Ordering::Relaxed);
        self.mcp = Arc::new(reg);
    }

    /// Whether the harness files live outside the workdir.
    fn relocated(&self) -> bool {
        self.harness_dir != self.workdir
    }

    /// The backup tree when relocated; `None` means beside the file.
    fn backup_root(&self) -> Option<PathBuf> {
        self.relocated().then(|| self.harness_dir.join(BACKUP_DIR))
    }

    /// The `sources` directory for fetched page texts ([`SOURCES_DIR`]).
    pub fn sources_dir(&self) -> PathBuf {
        if self.relocated() {
            self.harness_dir.join(SOURCES_DIR)
        } else {
            self.workdir.join(SOURCES_DIR)
        }
    }

    /// How `web_fetch` saves and names a page's text for this executor: an absolute pointer
    /// when the directory lies outside the workdir.
    fn save_to<'a>(&self, sources: &'a Path) -> crate::web::SaveTo<'a> {
        crate::web::SaveTo {
            dir: sources,
            absolute: self.relocated(),
        }
    }

    /// Same-file key for the crawl guard (normalised, case-folded on Windows).
    pub fn crawl_key(&self, rel: &str) -> String {
        let p = resolve_path(&self.workdir.join(rel))
            .to_string_lossy()
            .into_owned();
        if cfg!(windows) {
            p.to_lowercase()
        } else {
            p
        }
    }

    pub fn crawl_entry(&self, rel: &str) -> Option<&CrawlEntry> {
        self.crawl.get(&self.crawl_key(rel))
    }

    /// Run one tool call as the model issued it and return the receipt text. The
    /// `PreToolUse` hooks run after the safety gate has passed the call (a hook can only
    /// narrow; its block reads as a tool error so the failure ladder sees it), the
    /// `PostToolUse` hooks after dispatch, their context appended to the result.
    /// What the last [`Self::run_tool`] came to, as data.
    pub fn last_status(&self) -> ToolStatus {
        self.last_status
    }

    pub fn run_tool(&mut self, name: &str, args: &Value) -> String {
        self.last_status = ToolStatus::Ok;
        self.gate.clear_refusal();
        let inv = match self.prepare(name, args) {
            Ok(inv) => inv,
            Err(text) => {
                if let Some(r) = self.gate.last_refusal() {
                    self.last_status = ToolStatus::Refused(r);
                } else if !matches!(self.last_status, ToolStatus::Refused(_)) {
                    self.last_status = ToolStatus::Error;
                }
                return text;
            }
        };
        let name = resolve_alias(name);
        if !self.hooks.is_empty() {
            let pre = self.hooks.pre_tool(name, args);
            for n in &pre.notes {
                tracing::warn!(target: "rufous_kernel::hooks", "{n}");
            }
            if let Some(reason) = pre.blocked {
                self.last_status = ToolStatus::Refused(Refused::gate(RefusalKind::Hook));
                return format!("[tool error: blocked by a project hook: {reason}]");
            }
        }
        let mut res = self.dispatch(inv);
        self.last_status = ToolStatus::of_text(&res);
        if !self.hooks.is_empty() {
            let post = self.hooks.post_tool(name, args, &res);
            for n in &post.notes {
                tracing::warn!(target: "rufous_kernel::hooks", "{n}");
            }
            for c in post.context {
                res.push_str(&format!("\n[hook: {c}]"));
            }
        }
        res
    }

    fn dispatch(&mut self, inv: ToolInvocation) -> String {
        let (inv, rebased) = self.rebase_invocation(inv);
        let mut res = self.dispatch_inner(inv);
        if let Some((from, to)) = rebased {
            res.push_str(&format!(
                " {PATH_REBASED_MARK}{from} → {to}; this directory has no `{}/` subdirectory, paths are relative to it)",
                first_segment(&from)
            ));
        }
        res
    }

    /// The one seam for the workdir-prefix rule: the path argument of a file tool, rewritten
    /// when the rule fires, with `(as given, as used)` for the result and the log.
    fn rebase_invocation(&self, inv: ToolInvocation) -> (ToolInvocation, Option<(String, String)>) {
        if !self.rebase_workdir_prefix {
            return (inv, None);
        }
        let rebase = |p: &str| -> Option<String> { rebase_workdir_prefix(&self.workdir, p) };
        macro_rules! with_path {
            ($variant:ident { $path:ident $(, $f:ident)* }) => {{
                let $path = $path;
                match rebase(&$path) {
                    Some(to) => {
                        let from = $path.clone();
                        (ToolInvocation::$variant { $path: to.clone() $(, $f)* }, Some((from, to)))
                    }
                    None => (ToolInvocation::$variant { $path $(, $f)* }, None),
                }
            }};
        }
        match inv {
            ToolInvocation::CreateFile {
                path,
                content,
                replace,
            } => {
                with_path!(CreateFile {
                    path,
                    content,
                    replace
                })
            }
            ToolInvocation::ReadFile {
                path,
                start_line,
                end_line,
            } => {
                with_path!(ReadFile {
                    path,
                    start_line,
                    end_line
                })
            }
            ToolInvocation::EditFile {
                path,
                old,
                new,
                all,
            } => with_path!(EditFile {
                path,
                old,
                new,
                all
            }),
            ToolInvocation::AppendFile { path, content } => {
                with_path!(AppendFile { path, content })
            }
            ToolInvocation::DeleteFile { path } => with_path!(DeleteFile { path }),
            ToolInvocation::ListDir { path } => with_path!(ListDir { path }),
            ToolInvocation::SearchFiles { path, pattern } => {
                with_path!(SearchFiles { path, pattern })
            }
            other => (other, None),
        }
    }

    fn dispatch_inner(&mut self, inv: ToolInvocation) -> String {
        match inv {
            ToolInvocation::CreateFile {
                path,
                content,
                replace,
            } => self.create_file(&path, &content, replace),
            ToolInvocation::ReadFile {
                path,
                start_line,
                end_line,
            } => self.read_file(&path, start_line, end_line),
            ToolInvocation::EditFile {
                path,
                old,
                new,
                all,
            } => self.edit_file(&path, &old, &new, all),
            ToolInvocation::AppendFile { path, content } => self.append_file(&path, &content),
            ToolInvocation::DeleteFile { path } => self.delete_file(&path),
            ToolInvocation::ListDir { path } => self.list_dir(&path),
            ToolInvocation::SearchFiles { path, pattern } => self.search_files(&path, &pattern),
            ToolInvocation::RunBash { command } => self.run_bash(&command),
            ToolInvocation::Todo(args) => run_todo(&self.harness_dir, &args),
            ToolInvocation::Skills { action, name } => self.skills(action, name.as_deref()),
            ToolInvocation::EnableTools { tools } => self.enable_tools(&tools),
            ToolInvocation::Finish { summary } => format!("[TASK_COMPLETE] {summary}"),
            ToolInvocation::BatchTools { calls } => self.batch_tools(&calls),
            ToolInvocation::MemoryStore { text } => memory::memory_store(&self.agent_home, &text),
            ToolInvocation::MemoryRecall { query, limit } => memory::memory_recall(
                &self.agent_home,
                &query,
                limit.map(|l| l as usize).unwrap_or(3),
            ),
            net @ (ToolInvocation::WebSearch { .. }
            | ToolInvocation::WebFetch { .. }
            | ToolInvocation::WebSearchMulti { .. }
            | ToolInvocation::McpCall { .. }) => self.dispatch_network(&net),
        }
    }

    fn dispatch_network(&self, inv: &ToolInvocation) -> String {
        let sources = self.sources_dir();
        net_dispatch(
            &self.web,
            &self.mcp,
            &self.workdir,
            self.save_to(&sources),
            inv,
        )
    }

    /// Alias → coerce → parse → gate, without executing. `Err` is the text to return.
    /// Public for the replay corpus (`tests/replay.rs`), which pins what the harness does to
    /// a call a real model made.
    pub fn prepare(
        &mut self,
        name: &str,
        args: &Value,
    ) -> std::result::Result<ToolInvocation, String> {
        let name = resolve_alias(name);
        let mut map = coerce_object(name, args).map_err(|e| format!("[tool error: {e}]"))?;
        coerce_args(name, &mut map)?;
        // Ring 0: the child sandbox decides first and has no escalation path.
        if let Some(sb) = self.child.as_mut() {
            let chk = sb.check_tool(name, &map);
            if !chk.allowed() {
                self.last_status = ToolStatus::Refused(Refused::gate(RefusalKind::Child));
                if sb.should_terminate() && self.child_terminated.is_none() {
                    self.child_terminated = Some(format!(
                        "{} target={}",
                        chk.reason,
                        text::head(&chk.target, 120)
                    ));
                }
                return Err(format!(
                    "[child safety gate (default-deny, no escalation path): {}. Target: {}. The only available tools are {:?}; do not try to work around this, turn the current step into a deterministic output inside this directory.]",
                    chk.reason,
                    text::head(&chk.target, 120),
                    sb.allowed_tools_sorted()
                ));
            }
        }
        if is_mcp_tool(name) {
            // Flattened MCP tool `server.tool`: same gate, then the MCP dispatcher.
            let Some((server, tool)) = mcp::split_qualified(name) else {
                return Err("[unknown tool]".to_string());
            };
            let inv = ToolInvocation::McpCall {
                server: server.to_string(),
                tool: tool.to_string(),
                args: map,
            };
            if let Some(blocked) = self.gate.check(&inv, &self.workdir) {
                return Err(blocked);
            }
            return Ok(inv);
        }
        let inv = match ToolInvocation::parse_map(name, &map) {
            Ok(inv) => inv,
            Err(Error::UnknownTool(_)) => return Err("[unknown tool]".to_string()),
            Err(Error::ToolArgs { reason, .. }) => {
                if let Some(key) = reason
                    .strip_prefix("missing required argument `")
                    .and_then(|r| r.strip_suffix('`'))
                {
                    return Err(missing_arg_message(name, key, &map));
                }
                return Err(format!("[tool error: {name}: {reason}]"));
            }
            Err(e) => return Err(format!("[tool error: {e}]")),
        };
        // The relocated harness directory is an allowed root: the model reads the page texts
        // web_fetch saved there (bead boubou-m64.4; the gate refused the absolute pointer
        // until this) and the backups it is told about.
        if self.relocated() && !self.gate.policy.allow_dirs.contains(&self.harness_dir) {
            self.gate.policy.allow_dirs.push(self.harness_dir.clone());
        }
        if let Some(blocked) = self.gate.check(&inv, &self.workdir) {
            return Err(blocked);
        }
        // The shell gets the command as the model wrote it (bead boubou-bk9.10: rewriting
        // literal `\n` and `\"` into real characters broke sed replacements, `python -c`
        // programs and quoted heredocs that were right as written). The gate still judges
        // the unescaped form too, so a literal `\n` cannot hide a second command from the
        // token-only danger rules, which split on newlines.
        if let ToolInvocation::RunBash { command } = &inv {
            let unescaped = unescape_command(command);
            if unescaped != *command {
                let shadow = ToolInvocation::RunBash { command: unescaped };
                if let Some(blocked) = self.gate.check(&shadow, &self.workdir) {
                    return Err(blocked);
                }
            }
        }
        Ok(inv)
    }

    // ---- files ----

    fn create_file(&mut self, path: &str, content: &str, replace: bool) -> String {
        if path.starts_with('~') {
            return "[safety net: a path starting with ~ would be created literally, outside the home directory; rejected. Use a relative path inside the current working directory (e.g. outputs/data.json).]".to_string();
        }
        let p = self.workdir.join(path);
        if !replace {
            if let Ok(meta) = std::fs::metadata(&p) {
                let old = meta.len();
                let new = clen(content) as u64;
                if old > CLOBBER_MIN_OLD && (new as f64) < CLOBBER_RATIO * old as f64 {
                    return format!(
                        "[overwrite guard: '{path}' already holds {old} bytes and your new content is only {new} bytes (this looks like a large file being overwritten with a small fragment). To replace the whole file, retry with replace=true; to add content, use append_file; for a local change, use edit_file.]"
                    );
                }
            }
        }
        let (out, repaired) = repair_escapes(&p, content);
        if let Err(e) = self.store.write(&p, &out) {
            return io_failure(&p, e);
        }
        let tag = if repaired {
            "escapes auto-repaired".to_string()
        } else {
            format!("{} bytes", clen(content))
        };
        format!("[created {path} ({tag})]")
    }

    fn read_file(&mut self, path: &str, start_line: Option<u64>, end_line: Option<u64>) -> String {
        let p = self.workdir.join(path);
        // `exists()` is false for a path the OS refuses; ask first so it is not "not found".
        if let Err(e) = std::fs::symlink_metadata(&p) {
            if denied(&e) {
                return no_access(&p);
            }
        }
        if !p.exists() {
            return format!("[not found: {path}]");
        }
        // Said here, before any store is asked: an editor answers a directory with an
        // internal error that tells the model nothing (live Zed, bead boubou-m64.3).
        if p.is_dir() {
            return format!("[not a file: {path} is a directory; use list_dir]");
        }
        let low = path.to_lowercase();
        if IMG_EXTS.iter().any(|e| low.ends_with(e)) {
            let (real, inside) = safe_path(&self.workdir, path, &self.gate.policy.allow_dirs);
            if !inside {
                return "[blocked: path out of bounds (working-directory boundary guard)]"
                    .to_string();
            }
            return self.read_image(path, &real);
        }
        let key = self.crawl_key(path);
        let cfg = self.crawl_cfg;
        let entry = self.crawl.entry(key).or_default();
        entry.reads += 1;

        let full = match self.store.read(&p) {
            Ok(t) => t,
            Err(e) if denied(&e) => return no_access(&p),
            Err(e) => return format!("[tool error: {}]", Error::io(&p, e)),
        };
        let sl = start_line.unwrap_or(0) as usize;
        let el = end_line.unwrap_or(0) as usize;
        let (body, meta) = if sl > 0 || el > 0 {
            let lines: Vec<&str> = full.lines().collect();
            let el = if el == 0 {
                lines.len()
            } else {
                el.min(lines.len())
            };
            let from = sl.saturating_sub(1).min(lines.len());
            let chunk = if from < el {
                lines[from..el].join("\n")
            } else {
                String::new()
            };
            let meta = format!(
                "\n[file has {} lines; read lines {}-{}. For other parts use read_file(start_line=.., end_line=..).]",
                lines.len(),
                from + 1,
                el
            );
            (head(&chunk, READ_CHARS).to_string(), meta)
        } else {
            (head(&full, READ_CHARS).to_string(), String::new())
        };
        entry.served_bytes += body.len() as u64;
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        let budget = (cfg.budget_mult * size)
            .max(cfg.budget_floor)
            .min(cfg.budget_cap);
        if entry.served_bytes > budget {
            let note = if !entry.throttle_notified {
                entry.throttle_notified = true;
                format!(
                    "[crawl-guard: '{path}' has been read {} bytes in total, over this file's budget of {budget} ({}× the file size). Reads are not refused, but each further read of this file returns at most {} chars. To process this file efficiently, run a python script via run_bash that does the counting/searching/aggregating in one pass, e.g.:\n  python -c \"import collections; ls=open(r'{path}',encoding='utf-8').readlines(); print(collections.Counter(w for l in ls for w in l.split()).most_common(10))\"\nWrite the result to an output file and come back for specific details only when needed.]",
                    entry.served_bytes, cfg.budget_mult, cfg.throttle
                )
            } else {
                format!(
                    "[crawl-guard: reads of '{path}' are rate-limited (at most {} chars per read, {}/{budget} so far). A run_bash script can take it all in one go.]",
                    cfg.throttle, entry.served_bytes
                )
            };
            return format!("{note}\n{}{meta}", head(&body, cfg.throttle));
        }
        if cfg.nudge_at.contains(&entry.reads) && entry.nudges < 2 {
            entry.nudges += 1;
            return format!(
                "[hint: this is read number {} of '{path}'. If you are counting/searching/aggregating, a python script via run_bash does it in one pass; ignore this hint if you are just re-reading.]\n{body}{meta}",
                entry.reads
            );
        }
        format!("{body}{meta}")
    }

    fn read_image(&self, display: &str, real: &Path) -> String {
        let size = std::fs::metadata(real).map(|m| m.len()).unwrap_or(0);
        if size > IMG_MAX_BYTES {
            return format!(
                "[image too large: {display} ({} MB, limit 20 MB)]",
                size / 1024 / 1024
            );
        }
        let Ok(bytes) = std::fs::read(real) else {
            return format!("[not found: {display}]");
        };
        let b64 = text::base64_encode(&bytes);
        match crate::agent::history::queue_image(&self.harness_dir, display, b64, size) {
            ImageQueued::QueueFull => format!(
                "[image queued: {display} ({} KB). The attachment queue is full ({IMG_PER_TURN} images); analyse the attached images first, then read the next one.]",
                size / 1024
            ),
            ImageQueued::TooLarge => format!(
                "[image too large: {display} ({} MB, limit 20 MB)]",
                size / 1024 / 1024
            ),
            ImageQueued::Attached => format!("[image attached: {display} ({} KB)] The image is loaded with this turn's request; you can describe it, read its text or analyse its content directly.", size / 1024),
        }
    }

    fn edit_file(&mut self, path: &str, old: &str, new: &str, all: bool) -> String {
        let p = self.workdir.join(path);
        let s = match self.store.read(&p) {
            Ok(s) => s,
            Err(e) if denied(&e) => return no_access(&p),
            Err(e) => return format!("[tool error: {}]", Error::io(&p, e)),
        };
        if !self.edit_match {
            return self.edit_file_legacy(&p, path, &s, old, new);
        }
        match edit_match::find(&s, old, new) {
            edit_match::Found::Hits { tier, ranges } => {
                let n = ranges.len();
                if n > 1 && !all {
                    // Each place with a line of context, so the model can copy the one it
                    // means (bead boubou-7td): told to "include more surrounding lines" it
                    // re-sent the same quote until the corrector blocked the tool.
                    let mut places = String::new();
                    for &(s0, e0) in ranges.iter().take(4) {
                        let (a, b, region) = edit_match::place(&s, s0, e0, 1);
                        let shown = head(&region, 400);
                        let cut = if shown.len() < region.len() {
                            "…"
                        } else {
                            ""
                        };
                        places.push_str(&format!("\nLines {a}–{b} read:\n{shown}{cut}"));
                    }
                    if n > 4 {
                        places.push_str(&format!("\n(and {} more)", n - 4));
                    }
                    return format!(
                        "[edit failed: old text found {n} times in {path}; nothing was written.{places}\nQuote one of these regions as old, exactly as shown, so it matches once; edit each place separately.]"
                    );
                }
                let new_used = if tier == edit_match::Tier::Unescaped {
                    edit_match::unescape(new)
                } else {
                    new.to_string()
                };
                let out = edit_match::apply(&s, &ranges, &new_used, all);
                let mut r = self.write_edited(&p, path, &s, &out);
                if r.starts_with("[edited") {
                    if all && n > 1 {
                        r.push_str(&format!(" ({n} occurrences replaced)"));
                    }
                    if tier != edit_match::Tier::Exact {
                        r.push_str(&format!(" (old text matched {})", tier.describe()));
                    }
                }
                r
            }
            edit_match::Found::AlreadyApplied {
                first_line,
                last_line,
                region,
            } => {
                let shown = head(&region, 800);
                let cut = if shown.len() < region.len() {
                    "…"
                } else {
                    ""
                };
                format!(
                    "[no change: this edit is already done. {path} lines {first_line}–{last_line} now read:\n{shown}{cut}\nThe old text is not in the file and nothing was written. Do not repeat this edit; go on to the next step.]"
                )
            }
            edit_match::Found::Miss {
                first_line,
                last_line,
                region,
            } => {
                let shown = head(&region, 800);
                let cut = if shown.len() < region.len() {
                    "…"
                } else {
                    ""
                };
                format!(
                    "[edit failed: old text not found. Closest match in {path}, lines {first_line}–{last_line}:\n{shown}{cut}\nQuote the lines exactly as they are in the file, whitespace included, or rewrite the file with create_file.]"
                )
            }
        }
    }

    /// Mingbird's matcher, kept for the `edit_match` ablation: `\\n`, `\\t`, `\\"` unescaped in
    /// both texts, exact `contains`, the first occurrence replaced, a miss shows the file head.
    fn edit_file_legacy(&mut self, p: &Path, path: &str, s: &str, old: &str, new: &str) -> String {
        let (old, new) = (edit_match::unescape(old), edit_match::unescape(new));
        if !s.contains(&old) {
            let preview = head(s, 200).replace('\n', "↵");
            return format!(
                "[edit failed: old text not found. Preview of the file start: {preview}...] Use read_file to see the whole file and build an exactly matching old text, or rewrite the file with create_file."
            );
        }
        self.write_edited(p, path, s, &s.replacen(&old, &new, 1))
    }

    /// Back the file up (the text as it was read, so the copy is of what the model saw
    /// even when the store is not the disk), write it, and report the way every edit has.
    fn write_edited(&mut self, p: &Path, path: &str, before: &str, out: &str) -> String {
        let _ = std::fs::write(
            backup_target(p, &self.workdir, self.backup_root().as_deref()),
            before,
        );
        let (out, repaired) = repair_escapes(p, out);
        if let Err(e) = self.store.write(p, &out) {
            return io_failure(p, e);
        }
        let tag = if repaired {
            "backed up, escapes auto-repaired"
        } else {
            "backed up"
        };
        if self.relocated() {
            format!("[edited {path} ({tag})]")
        } else {
            format!("[edited {path} ({tag}); .bak backup available]")
        }
    }

    /// `append_file` starts on a new line (bead boubou-l3e): when the file is there, is not
    /// empty and does not end with a newline, and the content does not start with one, a
    /// newline goes first. qwen3.5:4b created `# Heron report` without one and appended
    /// `## Attendance…` 3/3, leaving a title with no heading after it. A byte-faithful
    /// append to a partial line is the shell's; nothing is ever removed.
    fn append_file(&mut self, path: &str, content: &str) -> String {
        let p = self.workdir.join(path);
        let (out, _) = repair_escapes(&p, content);
        let out = match self.store.read(&p) {
            Ok(s)
                if !s.is_empty()
                    && !s.ends_with('\n')
                    && !out.is_empty()
                    && !out.starts_with('\n') =>
            {
                format!("\n{out}")
            }
            _ => out,
        };
        if let Err(e) = self.store.append(&p, &out) {
            return io_failure(&p, e);
        }
        format!("[appended {path}]")
    }

    fn delete_file(&mut self, path: &str) -> String {
        let p = self.workdir.join(path);
        if let Err(e) = std::fs::symlink_metadata(&p) {
            if denied(&e) {
                return no_access(&p);
            }
        }
        if !p.exists() {
            return format!("[not found: {path}]");
        }
        let trash = if self.relocated() {
            self.harness_dir.join(RELOCATED_TRASH_DIR)
        } else {
            self.workdir.join(TRASH_DIR)
        };
        let base = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dst = trash.join(format!("{base}.{}.trash", hhmmss()));
        if std::fs::create_dir_all(&trash)
            .and_then(|_| std::fs::rename(&p, &dst))
            .is_ok()
        {
            return if self.relocated() {
                format!("[deleted {path} (recoverable)]")
            } else {
                format!("[deleted {path} → moved to {TRASH_DIR}/ (recoverable)]")
            };
        }
        match std::fs::remove_file(&p) {
            Ok(()) => format!("[deleted {path}]"),
            Err(e) => io_failure(&p, e),
        }
    }

    fn list_dir(&self, path: &str) -> String {
        let d = self.workdir.join(path);
        match std::fs::read_dir(&d) {
            Ok(rd) => {
                let mut names: Vec<String> = rd
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !is_harness_name(n))
                    .collect();
                names.sort();
                names.join("\n")
            }
            Err(e) if denied(&e) => no_access(&d),
            Err(_) => format!("[not a dir: {}]", d.display()),
        }
    }

    /// Skips binary files (a NUL in the first 8000 bytes, as git judges it), harness names, and
    /// the stores in `SEARCH_SKIP_DIRS` below the path given: from a project root the model was
    /// handed `.git/index` and dolt noms bytes (bead boubou-r7i).
    fn search_files(&self, path: &str, pattern: &str) -> String {
        let d = self.workdir.join(path);
        let re = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => return format!("[tool error: {e}]"),
        };
        if let Err(e) = std::fs::read_dir(&d) {
            if denied(&e) {
                return no_access(&d);
            }
        }
        let mut out = Vec::new();
        fn walk(dir: &Path, root: &Path, re: &Regex, out: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            let mut entries: Vec<PathBuf> = rd
                .flatten()
                .filter(|e| !is_harness_name(&e.file_name().to_string_lossy()))
                .map(|e| e.path())
                .collect();
            entries.sort();
            for fp in entries.iter().filter(|p| p.is_file()).take(200) {
                let Ok(bytes) = std::fs::read(fp) else {
                    continue;
                };
                if bytes[..bytes.len().min(8000)].contains(&0) {
                    continue;
                }
                let content = String::from_utf8_lossy(&bytes);
                for (i, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        let rel = fp.strip_prefix(root).unwrap_or(fp).display();
                        out.push(format!("{rel}:{}: {}", i + 1, head(line.trim(), 100)));
                    }
                    if out.len() >= 20 {
                        return;
                    }
                }
            }
            let skipped = |p: &&PathBuf| {
                p.file_name()
                    .is_some_and(|n| SEARCH_SKIP_DIRS.contains(&n.to_string_lossy().as_ref()))
            };
            for sub in entries.iter().filter(|p| p.is_dir() && !skipped(p)) {
                walk(sub, root, re, out);
                if out.len() >= 20 {
                    return;
                }
            }
        }
        walk(&d, &d, &re, &mut out);
        if out.is_empty() {
            "(no matches)".to_string()
        } else {
            out.join("\n")
        }
    }

    // ---- commands ----

    /// `command` runs as the model wrote it; `prepare` judged it and its unescaped form.
    fn run_bash(&mut self, command: &str) -> String {
        let cmd: &str = command;
        let snapped = bash_snapshot(cmd, &self.workdir, self.backup_root().as_deref());
        let run = run_command(cmd, &self.workdir, self.bash_timeout, self.stop.as_deref());
        if run.timed_out || run.interrupted {
            let note = if run.timed_out {
                let secs = self.bash_timeout.as_secs();
                format!("\n[timed out after {secs}s: the limit was reached and the process tree was terminated]")
            } else {
                "\n[stopped by the user: the process tree was terminated]".to_string()
            };
            return format_bash_out(
                if run.code == 0 { 1 } else { run.code },
                &format!("{}{note}", head(&run.stdout, 4000)),
                &run.stderr,
                self.slim_output,
            );
        }
        let mut out = format_bash_out(run.code, &run.stdout, &run.stderr, self.slim_output);
        // Worded so the model has nothing to act on: "[auto-backed up: X]" sent a 4B model
        // hunting for X.bak eight times in one turn (live Zed 2026-10-31, bead boubou-bk9.11).
        if !snapped.is_empty() {
            if self.relocated() {
                out.push_str(&format!(
                    "\n[backup of {} kept by the harness; nothing to clean up]",
                    snapped.join(", ")
                ));
            } else {
                out.push_str(&format!(
                    "\n[backup of {} kept beside each as .bak for rollback; leave them]",
                    snapped.join(", ")
                ));
            }
        }
        if cfg!(windows) {
            static LINUX_PATH: LazyLock<Regex> = LazyLock::new(|| {
                Regex::new(r"(^|\s)(cd|mkdir|ls|rm|cat|touch)\s+/").expect("regex")
            });
            let linuxish = LINUX_PATH.captures(cmd).is_some_and(|c| {
                let after = &cmd[c.get(0).map(|m| m.end()).unwrap_or(0)..];
                !(after.starts_with("Users")
                    || after.starts_with("home")
                    || after.chars().nth(1) == Some(':'))
            });
            if linuxish || cmd.contains("cd /workspace") {
                out.push_str("\n[hint: this is Windows; do not use Linux paths like /data /tmp /workspace. The working directory is set: use relative paths or local drive-letter paths.]");
            }
            if cmd.trim_start().starts_with("time ") {
                out.push_str("\n[hint: Windows has no bash time command. Time the work inside the script with the python time module, or with python -c.]");
            }
        }
        out
    }

    // ---- meta tools ----

    fn skills(&self, action: SkillsAction, name: Option<&str>) -> String {
        let set = SkillSet::standard(&self.workdir, &self.agent_home, &self.install_dir)
            .with_builtin(self.builtin_skills.iter().cloned());
        let listing = || {
            if set.is_empty() {
                "(none)".to_string()
            } else {
                set.iter()
                    .map(|s| format!("• {}: {}", s.name, s.description))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        match action {
            SkillsAction::List => listing(),
            SkillsAction::Load => {
                let name = name.unwrap_or_default();
                match set.get(name) {
                    Ok(skill) => match skill.load() {
                        Ok(body) => format!("### SKILL: {name}\n{body}"),
                        Err(e) => format!("[tool error: {e}]"),
                    },
                    Err(_) => format!("[skill '{name}' not found. Available: {}]", listing()),
                }
            }
        }
    }

    fn enable_tools(&mut self, names: &[String]) -> String {
        let mut added = Vec::new();
        let mut mcp_added = Vec::new();
        let mut unknown = Vec::new();
        for n in names {
            if ADVANCED_TOOL_NAMES.contains(&n.as_str()) {
                if !self.enabled.contains(n) {
                    self.enabled.push(n.clone());
                    added.push(n.clone());
                }
            } else if is_mcp_tool(n) {
                if !mcp_added.contains(n) {
                    mcp_added.push(n.clone());
                }
            } else {
                unknown.push(n.clone());
            }
        }
        let mut note = String::new();
        if !mcp_added.is_empty() {
            note.push_str(&format!(
                " MCP tools loaded on demand: {}",
                mcp_added.join(", ")
            ));
        }
        if !unknown.is_empty() {
            note.push_str(&format!(
                " Unknown tool name(s) (do not exist, cannot be enabled): {}. Advanced tools that can be enabled: {}.",
                unknown.join(", "),
                ADVANCED_TOOL_NAMES
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if added.is_empty() && mcp_added.is_empty() && unknown.is_empty() {
            note = " Nothing to enable: the tools you asked for are already available, just call them.".to_string();
        }
        let available: Vec<&str> = CORE_TOOL_NAMES
            .iter()
            .copied()
            .chain(self.enabled.iter().map(String::as_str))
            .collect();
        format!(
            "[enabled tools: {}. Now available: {}]{note}",
            if added.is_empty() {
                "(none)".to_string()
            } else {
                added.join(", ")
            },
            available.join(", ")
        )
    }

    /// Sub-calls go through the same alias/coerce/parse/gate pipeline as `run_tool`.
    /// Network tools run in parallel on threads; file and command tools run in order so a
    /// `create_file` followed by a `read_file` cannot race. Cap 8, no nesting, 2000 chars each.
    fn batch_tools(&mut self, calls: &[tools::BatchCall]) -> String {
        if calls.is_empty() {
            return "[batch_tools: a calls array is required, of the form [{tool, args}, ...]]"
                .to_string();
        }
        let calls = &calls[..calls.len().min(8)];
        if calls.iter().any(|c| c.tool == "batch_tools") {
            return "[batch_tools: nested batch_tools is not allowed]".to_string();
        }
        const PARALLEL_SAFE: [&str; 4] =
            ["web_search", "web_fetch", "web_search_multi", "mcp_call"];
        let mut results: Vec<Option<String>> = vec![None; calls.len()];
        // Gate the network calls up front (needs `&mut`), then fan out on `&self`.
        let mut net: Vec<(usize, ToolInvocation)> = Vec::new();
        for (i, c) in calls.iter().enumerate() {
            let real = resolve_alias(&c.tool);
            if PARALLEL_SAFE.contains(&real) || is_mcp_tool(real) {
                match self.prepare(&c.tool, &c.args) {
                    Ok(inv) => net.push((i, inv)),
                    Err(text) => results[i] = Some(text),
                }
            }
        }
        if !net.is_empty() {
            let (web, reg, workdir) = (&self.web, &self.mcp, self.workdir.as_path());
            let sources = self.sources_dir();
            let save = self.save_to(&sources);
            let outs: Vec<(usize, String)> = std::thread::scope(|s| {
                let handles: Vec<_> = net
                    .iter()
                    .map(|(i, inv)| {
                        s.spawn(move || (*i, net_dispatch(web, reg, workdir, save, inv)))
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            for (i, r) in outs {
                results[i] = Some(r);
            }
        }
        for (i, c) in calls.iter().enumerate() {
            if results[i].is_none() {
                results[i] = Some(self.run_tool(&c.tool, &c.args));
            }
        }
        calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let tool = if c.tool.is_empty() {
                    "?"
                } else {
                    c.tool.as_str()
                };
                let r = results[i].take().unwrap_or_else(|| "(failed)".to_string());
                format!("### [{}] {tool}\n{}", i + 1, head(&r, 2000))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// The network tools need only the web client and the workdir, so `batch_tools` and
/// `web_search_multi` can fan them out on threads without touching the executor.
fn net_dispatch(
    web: &Web,
    mcp: &McpRegistry,
    workdir: &Path,
    save: crate::web::SaveTo<'_>,
    inv: &ToolInvocation,
) -> String {
    if web.offline && !matches!(inv, ToolInvocation::McpCall { .. }) {
        // The face hides these offline; a call that arrives anyway is refused here
        // (docs/egress.md). An MCP call is the registry's to judge: loopback servers stay.
        return crate::egress::offline_refusal(inv.name());
    }
    match inv {
        ToolInvocation::WebSearch { query } => web.search(query, 5),
        ToolInvocation::WebFetch { url } => web.fetch(url, Some(save)),
        ToolInvocation::WebSearchMulti {
            queries,
            max_results,
        } => web.search_multi(queries, max_results.map(|m| m as usize).unwrap_or(3)),
        ToolInvocation::McpCall { server, tool, args } => mcp.call(server, tool, args, workdir),
        other => format!("[tool {}: not a network tool]", other.name()),
    }
}

/// `_REQ_ARGS`-style actionable message for a missing argument, with a correct example
/// for the write tools (small models drop `path` three times in a row otherwise).
fn missing_arg_message(name: &str, key: &str, got: &serde_json::Map<String, Value>) -> String {
    let req = tools::required_args(name);
    let got_keys: Vec<&String> = got.keys().collect();
    let missing: Vec<&String> = if got_keys.is_empty() {
        req.iter().collect()
    } else {
        req.iter().filter(|k| !got.contains_key(*k)).collect()
    };
    let example = match name {
        "create_file" => {
            " Correct example: create_file(path=\"app.py\", content=\"...file content...\")"
        }
        "edit_file" => {
            " Correct example: edit_file(path=\"filename\", old=\"old text\", new=\"new text\")"
        }
        "append_file" => {
            " Correct example: append_file(path=\"filename\", content=\"text to append\")"
        }
        _ => "",
    };
    format!(
        "[tool error: {name} is missing argument '{key}'. Required arguments: {missing:?}. You passed: {got_keys:?}.{example} Add the missing argument and retry with the same content (change nothing else).]"
    )
}

fn bak_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".bak");
    PathBuf::from(s)
}

/// Where the backup of `p` goes: beside it (`<p>.bak`), or under `root` mirroring the
/// path relative to `workdir` (the parent directories are created).
fn backup_target(p: &Path, workdir: &Path, root: Option<&Path>) -> PathBuf {
    let Some(root) = root else {
        return bak_path(p);
    };
    let rel = match p.strip_prefix(workdir) {
        Ok(r) => r.to_path_buf(),
        Err(_) => p
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("file")),
    };
    let dst = bak_path(&root.join(rel));
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    dst
}

/// The harness's own files, kept out of `list_dir` so the model does not spend its turn on
/// them (boubou-7s3: nine deletes of one `.bak` in a 47-call editor turn). Mingbird's
/// layout still writes them into the workdir; they are simply not listed.
/// VCS, tracker and package-manager stores `search_files` does not descend into.
const SEARCH_SKIP_DIRS: [&str; 6] = [".git", ".hg", ".svn", ".jj", ".beads", "node_modules"];

pub fn is_harness_name(name: &str) -> bool {
    name.ends_with(".bak")
        || name == TRASH_DIR
        || name.starts_with(".agent_state")
        || name == ".hb_pending_images.json"
}

fn hhmmss() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        % 86_400;
    format!(
        "{:02}{:02}{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Mingbird's `_auto_repair`: in a `.md`, `.txt` or `.csv` file, literal `\n` and `\t`
/// sequences a model wrote (three or more) become the real characters. Pure: the text to
/// write and whether it changed, so the repaired text goes through the [`FileStore`]
/// rather than being read back from the disk.
pub fn repair_escapes(path: &Path, content: &str) -> (String, bool) {
    static N: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+n").expect("regex"));
    static T: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+t").expect("regex"));
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_lowercase();
    if !matches!(ext.as_str(), "md" | "txt" | "csv")
        || !content.contains('\\')
        || N.find_iter(content).count() + T.find_iter(content).count() < 3
    {
        return (content.to_string(), false);
    }
    let fixed = T
        .replace_all(&N.replace_all(content, "\n"), "\t")
        .into_owned();
    let changed = fixed != content;
    (fixed, changed)
}

/// Literal `\n`, `\t`, `\"` and `\'` sequences read as the real characters: what a model
/// that double-escaped would have meant (Mingbird's `run_bash` unescape). Since 2026-10-31
/// (bead boubou-bk9.10) the shell never gets this form, which broke sed replacements,
/// `python -c` programs and quoted heredocs that were right as written; `prepare` only runs
/// the safety gate over it as well, so a literal `\n` cannot hide a second command.
pub fn unescape_command(command: &str) -> String {
    static NL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+n").expect("regex"));
    static TAB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+t").expect("regex"));
    static DQ: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\\+""#).expect("regex"));
    static SQ: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+'").expect("regex"));
    let cmd = NL.replace_all(command, "\n");
    let cmd = TAB.replace_all(&cmd, "\t");
    let cmd = DQ.replace_all(&cmd, "\"");
    SQ.replace_all(&cmd, "'").into_owned()
}

/// `_bash_snapshot`: `sed -i`, redirections, `cp`/`mv`, `rm` and friends rewrite or remove
/// files behind the `.bak` net; back up every existing workspace file the command names
/// first, globs expanded (`rm lib/*.dart`, bead boubou-bk9.12), beside the file or under
/// `backup_root` (see [`Executor::harness_dir`]).
pub fn bash_snapshot(cmd: &str, workdir: &Path, backup_root: Option<&Path>) -> Vec<String> {
    // `:` and `~` for a drive and an 8.3 short name (`C:\Users\RUNNER~1\…`); without them the
    // token was cut there and named no file, so nothing was backed up (bead boubou-2pl).
    static TOK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[A-Za-z0-9_\-./\\:~]+\.[A-Za-z0-9]{1,8}").expect("regex"));
    static GLOB: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[A-Za-z0-9_\-./\\:~*?]*[*?][A-Za-z0-9_\-./\\:~*?]*").expect("regex")
    });
    let low = format!(" {cmd} ");
    let suspects = [
        "sed -i",
        "sed --in-place",
        ">",
        ">>",
        " tee ",
        "cp ",
        "mv ",
        "truncate ",
        "dd ",
        "perl -pi",
        "python -m fileinput",
        "rm ",
    ];
    if !suspects.iter().any(|s| low.contains(s)) {
        return Vec::new();
    }
    let wd = resolve_path(workdir);
    let mut out: Vec<String> = Vec::new();
    let snap = |p: &Path, out: &mut Vec<String>| {
        let base = p
            .file_name()
            .and_then(|b| b.to_str())
            .unwrap_or_default()
            .to_string();
        if base.is_empty() || base.ends_with(".bak") || base.starts_with('.') {
            return;
        }
        let rp = resolve_path(p);
        if !rp.starts_with(&wd) || rp == wd || !rp.is_file() {
            return;
        }
        if std::fs::copy(&rp, backup_target(&rp, &wd, backup_root)).is_ok() && !out.contains(&base)
        {
            out.push(base);
        }
    };
    let abs = |tok: &str| {
        if Path::new(tok).is_absolute() {
            PathBuf::from(tok)
        } else {
            workdir.join(tok)
        }
    };
    for tok in TOK.find_iter(cmd).map(|m| m.as_str()) {
        snap(&abs(tok), &mut out);
        if out.len() >= 20 {
            return out;
        }
    }
    for tok in GLOB.find_iter(cmd).map(|m| m.as_str()) {
        let Some(pattern) = abs(tok).to_str().map(str::to_string) else {
            continue;
        };
        let Ok(paths) = glob::glob(&pattern) else {
            continue;
        };
        for p in paths.flatten() {
            snap(&p, &mut out);
            if out.len() >= 20 {
                return out;
            }
        }
    }
    out
}

#[derive(Debug)]
pub(crate) struct CommandRun {
    pub(crate) code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
    /// The stop flag was raised while the command ran.
    pub(crate) interrupted: bool,
}

/// Run through the platform shell with a wall-clock limit; on timeout, or when `stop` is
/// raised, the whole process tree is killed (orphaned grandchildren would otherwise hold
/// the pipes open forever).
pub(crate) fn run_command(
    cmd: &str,
    cwd: &Path,
    timeout: Duration,
    stop: Option<&AtomicBool>,
) -> CommandRun {
    use std::process::{Command, Stdio};
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    for var in crate::config::API_KEY_ENVS {
        command.env_remove(var);
    }
    command
        .current_dir(cwd)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CommandRun {
                code: 1,
                stdout: String::new(),
                stderr: format!("[spawn failed: {e}]"),
                timed_out: false,
                interrupted: false,
            }
        }
    };
    let _live = LiveGroup::register(child.id());
    // Each pipe is read on its own thread and arrives over a channel, so the wait for it
    // can be bounded: a process the command backgrounded inherits the pipes and holds them
    // open for as long as it lives (bead boubou-d4z).
    let reader = |pipe: Option<std::process::ChildStdout>,
                  err: Option<std::process::ChildStderr>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            if let Some(mut e) = err {
                let _ = e.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let out_rx = reader(child.stdout.take(), None);
    let err_rx = reader(None, child.stderr.take());
    let start = Instant::now();
    let mut timed_out = false;
    let mut interrupted = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() >= timeout => {
                timed_out = true;
                kill_tree(child.id());
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) if stop.is_some_and(|s| s.load(Ordering::Relaxed)) => {
                interrupted = true;
                kill_tree(child.id());
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    // The shell is gone; its output may still be held by a process it put in the
    // background. Wait out a short grace (never past the command's own deadline), then stop
    // the process group and say so.
    let deadline = (Instant::now() + BACKGROUND_GRACE).min(start + timeout);
    let (mut out, mut err) = (None, None);
    let mut held = false;
    loop {
        recv_ready(&out_rx, &mut out);
        recv_ready(&err_rx, &mut err);
        if out.is_some() && err.is_some() {
            break;
        }
        if !interrupted && stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
            interrupted = true;
        }
        if interrupted || timed_out || Instant::now() >= deadline {
            held = !interrupted && !timed_out;
            kill_tree(child.id());
            let after = Instant::now() + BACKGROUND_GRACE;
            while (out.is_none() || err.is_none()) && Instant::now() < after {
                recv_ready(&out_rx, &mut out);
                recv_ready(&err_rx, &mut err);
                std::thread::sleep(Duration::from_millis(20));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let stdout = text::decode_console(&out.unwrap_or_default());
    let mut stderr = text::decode_console(&err.unwrap_or_default());
    if held {
        stderr.push_str(BACKGROUND_NOTE);
    }
    let code = status.map(exit_code).unwrap_or(1);
    CommandRun {
        code,
        stdout,
        stderr,
        timed_out,
        interrupted,
    }
}

/// How long a command's output may stay open after its shell has exited.
const BACKGROUND_GRACE: Duration = Duration::from_secs(2);

const BACKGROUND_NOTE: &str = "\n[a background process kept the command's output open after the shell exited; it was stopped. To leave a process running, redirect its output (cmd > file.log 2>&1 &).]";

fn recv_ready(rx: &std::sync::mpsc::Receiver<Vec<u8>>, slot: &mut Option<Vec<u8>>) {
    if slot.is_none() {
        if let Ok(b) = rx.try_recv() {
            *slot = Some(b);
        }
    }
}

fn exit_code(st: std::process::ExitStatus) -> i32 {
    if let Some(c) = st.code() {
        return c;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = st.signal() {
            return -sig;
        }
    }
    1
}

/// Process groups rufous started and has not reaped yet: `run_bash` commands and stdio MCP
/// servers, by leader pid. [`kill_live_groups`] stops them when rufous itself is going away
/// (a host closed its stdin, or a signal), so nothing it started outlives it (boubou-f8d).
static LIVE_GROUPS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// One entry in the live-group registry, removed when dropped (the group was reaped, or
/// its owner is done with it).
#[derive(Debug)]
pub struct LiveGroup(u32);

impl LiveGroup {
    pub fn register(pid: u32) -> Self {
        live_groups().push(pid);
        LiveGroup(pid)
    }
}

impl Drop for LiveGroup {
    fn drop(&mut self) {
        live_groups().retain(|&p| p != self.0);
    }
}

fn live_groups() -> std::sync::MutexGuard<'static, Vec<u32>> {
    LIVE_GROUPS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Kill every registered group ([`kill_tree`]); for the moment rufous exits.
pub fn kill_live_groups() {
    let pids = std::mem::take(&mut *live_groups());
    for pid in pids {
        kill_tree(pid);
    }
}

/// Kills `pid` and everything it started. On unix the command was spawned as a process-group
/// leader (`process_group(0)`, so its pgid is its pid) and the whole group gets SIGKILL through
/// killpg(2): the external `kill -9 -<pid>` returns an error under procps-ng 4 (Ubuntu
/// 24.04) and left a shell's children holding the output pipe (bead boubou-tfr).
pub(crate) fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }
    #[cfg(unix)]
    if let Ok(pgid) = i32::try_from(pid) {
        // A group that has already exited gives ESRCH, which is the outcome wanted.
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pgid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}
