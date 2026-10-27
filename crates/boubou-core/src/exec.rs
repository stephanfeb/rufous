//! Tool execution inside the workspace (`run_tool`). Every call goes alias → type
//! coercion → typed decode → safety gate → dispatch. Ring 4 of the safety model lives
//! here: `.bak` before edits, `.mingbird_trash/` instead of unlink, and the overwrite guard.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

use crate::config::agent_home;
use crate::guards::format_bash_out;
use crate::safety::{resolve_path, safe_path, Gate, SafetyPolicy};
use crate::skills::SkillSet;
use crate::text::{self, clen, head};
use crate::todo::run_todo;
use crate::tools::{
    self, coerce_args, coerce_object, is_mcp_tool, resolve_alias, SkillsAction, ToolInvocation,
    ADVANCED_TOOL_NAMES, CORE_TOOL_NAMES,
};
use crate::Error;

/// Recycle bin inside the workspace. Kept as Mingbird's name so old workspaces stay readable.
pub const TRASH_DIR: &str = ".mingbird_trash";
pub const IMG_EXTS: [&str; 6] = [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp"];
const IMG_MAX_BYTES: u64 = 20 * 1024 * 1024;
const IMG_PER_TURN: usize = 4;
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

pub struct Executor {
    pub workdir: PathBuf,
    pub gate: Gate,
    pub crawl_cfg: CrawlConfig,
    pub crawl: HashMap<String, CrawlEntry>,
    /// `_SLIM_OUTPUT_MODE`: trim long command output to its tail.
    pub slim_output: bool,
    /// Advanced tools the model has enabled, in enable order.
    pub enabled: Vec<String>,
    pub bash_timeout: Duration,
    pub agent_home: PathBuf,
    pub install_dir: PathBuf,
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
        Self {
            workdir: workdir.into(),
            gate: Gate::headless(policy),
            crawl_cfg: CrawlConfig::default(),
            crawl: HashMap::new(),
            slim_output: false,
            enabled: Vec::new(),
            bash_timeout: Duration::from_secs(
                std::env::var("AGENT_BASH_TIMEOUT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(120),
            ),
            agent_home: agent_home(),
            install_dir,
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

    /// Run one tool call as the model issued it and return the receipt text.
    pub fn run_tool(&mut self, name: &str, args: &Value) -> String {
        let name = resolve_alias(name);
        let mut map = match coerce_object(name, args) {
            Ok(m) => m,
            Err(e) => return format!("[tool error: {e}]"),
        };
        if let Err(msg) = coerce_args(name, &mut map) {
            return msg;
        }
        if is_mcp_tool(name) {
            return format!("[tool {name}: MCP tools are not yet ported in Boubou]");
        }
        let inv = match ToolInvocation::parse_map(name, &map) {
            Ok(inv) => inv,
            Err(Error::UnknownTool(_)) => return "[unknown tool]".to_string(),
            Err(Error::ToolArgs { reason, .. }) => {
                if let Some(key) = reason
                    .strip_prefix("missing required argument `")
                    .and_then(|r| r.strip_suffix('`'))
                {
                    return missing_arg_message(name, key, &map);
                }
                return format!("[tool error: {name}: {reason}]");
            }
            Err(e) => return format!("[tool error: {e}]"),
        };
        if let Some(blocked) = self.gate.check(&inv, &self.workdir) {
            return blocked;
        }
        self.dispatch(inv)
    }

    fn dispatch(&mut self, inv: ToolInvocation) -> String {
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
            ToolInvocation::EditFile { path, old, new } => self.edit_file(&path, &old, &new),
            ToolInvocation::AppendFile { path, content } => self.append_file(&path, &content),
            ToolInvocation::DeleteFile { path } => self.delete_file(&path),
            ToolInvocation::ListDir { path } => self.list_dir(&path),
            ToolInvocation::SearchFiles { path, pattern } => self.search_files(&path, &pattern),
            ToolInvocation::RunBash { command } => self.run_bash(&command),
            ToolInvocation::Todo(args) => run_todo(&self.workdir, &args),
            ToolInvocation::Skills { action, name } => self.skills(action, name.as_deref()),
            ToolInvocation::EnableTools { tools } => self.enable_tools(&tools),
            ToolInvocation::Finish { summary } => format!("[TASK_COMPLETE] {summary}"),
            ToolInvocation::BatchTools { calls } => self.batch_tools(&calls),
            ToolInvocation::WebSearch { .. }
            | ToolInvocation::WebFetch { .. }
            | ToolInvocation::WebSearchMulti { .. }
            | ToolInvocation::MemoryStore { .. }
            | ToolInvocation::MemoryRecall { .. }
            | ToolInvocation::McpCall { .. } => {
                format!("[tool {}: not yet ported in Boubou]", inv.name())
            }
        }
    }

    // ---- files ----

    fn create_file(&mut self, path: &str, content: &str, replace: bool) -> String {
        if path.starts_with('~') {
            return "[安全垫:路径以 ~ 开头会按字面创建到家目录外,已拒绝。请在当前工作目录内用相对路径(如 outputs/data.json)。]".to_string();
        }
        let p = self.workdir.join(path);
        if !replace {
            if let Ok(meta) = std::fs::metadata(&p) {
                let old = meta.len();
                let new = clen(content) as u64;
                if old > CLOBBER_MIN_OLD && (new as f64) < CLOBBER_RATIO * old as f64 {
                    return format!(
                        "[覆盖防护: '{path}' 已有 {old} 字节内容,你的新内容只有 {new} 字节(疑似把大文件覆盖成了小片段)。确要整体替换:加参数 replace=true 重试;要补充内容:用 append_file;要局部修改:用 edit_file。]"
                    );
                }
            }
        }
        if let Some(dir) = p.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                return format!("[tool error: {e}]");
            }
        }
        if let Err(e) = std::fs::write(&p, content) {
            return format!("[tool error: {e}]");
        }
        let tag = if auto_repair(&p) {
            "已自动修复转义".to_string()
        } else {
            format!("{} bytes", clen(content))
        };
        format!("[created {path} ({tag})]")
    }

    fn read_file(&mut self, path: &str, start_line: Option<u64>, end_line: Option<u64>) -> String {
        let p = self.workdir.join(path);
        if !p.exists() {
            return format!("[not found: {path}]");
        }
        let low = path.to_lowercase();
        if IMG_EXTS.iter().any(|e| low.ends_with(e)) {
            let (real, inside) = safe_path(&self.workdir, path, &self.gate.policy.allow_dirs);
            if !inside {
                return "[blocked: 路径越界(工作目录边界守护)]".to_string();
            }
            return self.read_image(path, &real);
        }
        let key = self.crawl_key(path);
        let cfg = self.crawl_cfg;
        let entry = self.crawl.entry(key).or_default();
        entry.reads += 1;

        let full = match text::read_text(&p) {
            Ok(t) => t,
            Err(e) => return format!("[tool error: {e}]"),
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
                "\n[文件共 {} 行,已读取 {}-{} 行。需要其他部分用 read_file(start_line=.., end_line=..)。]",
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
                    "[crawl-guard: '{path}' 累计已读取 {} 字节,超出本文件预算 {budget}(按 {}×文件大小)。读取不会被拒绝,但本文件后续每次最多返回 {} 字符。要高效处理此文件,用 run_bash 跑 python 脚本一次完成统计/搜索/聚合,例如:\n  python -c \"import collections; ls=open(r'{path}',encoding='utf-8').readlines(); print(collections.Counter(w for l in ls for w in l.split()).most_common(10))\"\n把结果写进产物文件,需要细节时再精确回看。]",
                    entry.served_bytes, cfg.budget_mult, cfg.throttle
                )
            } else {
                format!(
                    "[crawl-guard: '{path}' 读取限速中(每次最多 {} 字符,累计 {}/{budget})。用 run_bash 脚本可一次取全。]",
                    cfg.throttle, entry.served_bytes
                )
            };
            return format!("{note}\n{}{meta}", head(&body, cfg.throttle));
        }
        if cfg.nudge_at.contains(&entry.reads) && entry.nudges < 2 {
            entry.nudges += 1;
            return format!(
                "[提示: 这是第 {} 次读取 '{path}'。若在做统计/搜索/聚合,用 run_bash 跑 python 脚本一次完成更快;单纯回看可忽略本提示。]\n{body}{meta}",
                entry.reads
            );
        }
        format!("{body}{meta}")
    }

    fn pending_images_path(&self) -> PathBuf {
        self.workdir.join(".hb_pending_images.json")
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
        let mut pending: Vec<Value> = std::fs::read_to_string(self.pending_images_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if pending.len() >= IMG_PER_TURN {
            return format!(
                "[image queued: {display} ({} KB)。待附队列已满({IMG_PER_TURN} 张),请先分析已附载的图片,再读取下一张。]",
                size / 1024
            );
        }
        pending.push(serde_json::json!({"path": display, "b64": text::base64_encode(&bytes)}));
        if let Ok(s) = serde_json::to_string(&pending) {
            let _ = std::fs::write(self.pending_images_path(), s);
        }
        format!("[image attached: {display} ({} KB)]——图片已随本轮请求载入,你可以直接描述、读取文字或分析它的内容。", size / 1024)
    }

    fn edit_file(&mut self, path: &str, old: &str, new: &str) -> String {
        let p = self.workdir.join(path);
        let s = match text::read_text(&p) {
            Ok(s) => s,
            Err(e) => return format!("[tool error: {e}]"),
        };
        let _ = std::fs::copy(&p, bak_path(&p));
        let unescape = |t: &str| {
            t.replace("\\n", "\n")
                .replace("\\t", "\t")
                .replace("\\\"", "\"")
        };
        let (old, new) = (unescape(old), unescape(new));
        if !s.contains(&old) {
            let preview = head(&s, 200).replace('\n', "↵");
            return format!(
                "[edit failed: old text not found. 文件开头预览: {preview}...] 请 read_file 查看全文,构造精确匹配的 old 文本,或改用 create_file 整体重写。"
            );
        }
        if let Err(e) = std::fs::write(&p, s.replacen(&old, &new, 1)) {
            return format!("[tool error: {e}]");
        }
        let tag = if auto_repair(&p) {
            "已备份+自动修复转义"
        } else {
            "已备份"
        };
        format!("[edited {path} ({tag}),.bak 备份可用]")
    }

    fn append_file(&mut self, path: &str, content: &str) -> String {
        use std::io::Write;
        let p = self.workdir.join(path);
        let res = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&p)
            .and_then(|mut f| f.write_all(content.as_bytes()));
        if let Err(e) = res {
            return format!("[tool error: {e}]");
        }
        auto_repair(&p);
        format!("[appended {path}]")
    }

    fn delete_file(&mut self, path: &str) -> String {
        let p = self.workdir.join(path);
        if !p.exists() {
            return format!("[not found: {path}]");
        }
        let trash = self.workdir.join(TRASH_DIR);
        let base = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dst = trash.join(format!("{base}.{}.trash", hhmmss()));
        if std::fs::create_dir_all(&trash)
            .and_then(|_| std::fs::rename(&p, &dst))
            .is_ok()
        {
            return format!("[deleted {path} → 已移入 {TRASH_DIR}/(可恢复)]");
        }
        match std::fs::remove_file(&p) {
            Ok(()) => format!("[deleted {path}]"),
            Err(e) => format!("[tool error: {e}]"),
        }
    }

    fn list_dir(&self, path: &str) -> String {
        let d = self.workdir.join(path);
        match std::fs::read_dir(&d) {
            Ok(rd) => {
                let mut names: Vec<String> = rd
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names.join("\n")
            }
            Err(_) => format!("[not a dir: {}]", d.display()),
        }
    }

    fn search_files(&self, path: &str, pattern: &str) -> String {
        let d = self.workdir.join(path);
        let re = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => return format!("[tool error: {e}]"),
        };
        let mut out = Vec::new();
        fn walk(dir: &Path, root: &Path, re: &Regex, out: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for fp in entries.iter().filter(|p| p.is_file()).take(200) {
                let Ok(bytes) = std::fs::read(fp) else {
                    continue;
                };
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
            for sub in entries.iter().filter(|p| p.is_dir()) {
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

    fn run_bash(&mut self, command: &str) -> String {
        static NL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+n").expect("regex"));
        static TAB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+t").expect("regex"));
        static DQ: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\\+""#).expect("regex"));
        static SQ: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+'").expect("regex"));
        let cmd = NL.replace_all(command, "\n");
        let cmd = TAB.replace_all(&cmd, "\t");
        let cmd = DQ.replace_all(&cmd, "\"");
        let cmd = SQ.replace_all(&cmd, "'").into_owned();
        let snapped = bash_snapshot(&cmd, &self.workdir);
        let run = run_command(&cmd, &self.workdir, self.bash_timeout);
        if run.timed_out {
            let secs = self.bash_timeout.as_secs();
            let note = format!("\n[已超时:{secs} 秒上限,进程树已终止 · timed out after {secs}s]");
            return format_bash_out(
                if run.code == 0 { 1 } else { run.code },
                &format!("{}{note}", head(&run.stdout, 4000)),
                &run.stderr,
                self.slim_output,
            );
        }
        let mut out = format_bash_out(run.code, &run.stdout, &run.stderr, self.slim_output);
        if !snapped.is_empty() {
            out.push_str(&format!(
                "\n[已自动备份可回滚: {} → 同名.bak]",
                snapped.join("、")
            ));
        }
        if cfg!(windows) {
            static LINUX_PATH: LazyLock<Regex> = LazyLock::new(|| {
                Regex::new(r"(^|\s)(cd|mkdir|ls|rm|cat|touch)\s+/").expect("regex")
            });
            let linuxish = LINUX_PATH.captures(&cmd).is_some_and(|c| {
                let after = &cmd[c.get(0).map(|m| m.end()).unwrap_or(0)..];
                !(after.starts_with("Users")
                    || after.starts_with("home")
                    || after.chars().nth(1) == Some(':'))
            });
            if linuxish || cmd.contains("cd /workspace") {
                out.push_str("\n[提示: 这是 Windows,不要用 /data /tmp /workspace 等 Linux 路径。工作目录已设定,直接用相对路径或本机盘符路径。]");
            }
            if cmd.trim_start().starts_with("time ") {
                out.push_str("\n[提示: Windows 无 bash 的 time 命令。请在脚本内用 python time 模块计时,或用 python -c 执行计时。]");
            }
        }
        out
    }

    // ---- meta tools ----

    fn skills(&self, action: SkillsAction, name: Option<&str>) -> String {
        let set = SkillSet::standard(&self.workdir, &self.agent_home, &self.install_dir);
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
                    Ok(skill) => match text::read_text(&skill.path) {
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
            note.push_str(&format!(" MCP 工具已按需加载: {}", mcp_added.join(", ")));
        }
        if !unknown.is_empty() {
            note.push_str(&format!(
                " 未知的工具名(不存在,无法启用): {}。可启用的高级工具: {}。",
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
            note = " 无需再启用:你要的工具都已可用,直接调用它即可。".to_string();
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

    /// Sub-calls go through `run_tool`, so gates and the crawl guard are shared.
    /// Mingbird runs network sub-calls in parallel; here everything is sequential until
    /// the network tools exist.
    fn batch_tools(&mut self, calls: &[tools::BatchCall]) -> String {
        if calls.is_empty() {
            return "[batch_tools: 需提供 calls 数组,形如 [{tool, args}, ...]]".to_string();
        }
        let calls = &calls[..calls.len().min(8)];
        if calls.iter().any(|c| c.tool == "batch_tools") {
            return "[batch_tools: 禁止嵌套 batch_tools]".to_string();
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
                let r = self.run_tool(&c.tool, &c.args);
                format!("### [{}] {tool}\n{}", i + 1, head(&r, 2000))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// `_REQ_ARGS`-style actionable message for a missing argument, with a correct example
/// for the write tools (small models drop `path` three times in a row otherwise).
fn missing_arg_message(name: &str, key: &str, got: &serde_json::Map<String, Value>) -> String {
    let req: Vec<String> = tools::core_tools()
        .into_iter()
        .chain(tools::advanced_tools())
        .find(|t| t["function"]["name"] == name)
        .and_then(|t| t["function"]["parameters"]["required"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let got_keys: Vec<&String> = got.keys().collect();
    let missing: Vec<&String> = if got_keys.is_empty() {
        req.iter().collect()
    } else {
        req.iter().filter(|k| !got.contains_key(*k)).collect()
    };
    let example = match name {
        "create_file" => " 正确示例: create_file(path=\"app.py\", content=\"...文件内容...\")",
        "edit_file" => " 正确示例: edit_file(path=\"文件名\", old=\"旧文本\", new=\"新文本\")",
        "append_file" => " 正确示例: append_file(path=\"文件名\", content=\"追加内容\")",
        _ => "",
    };
    format!(
        "[tool error: {name} 缺少参数 '{key}'。必须提供参数: {missing:?}。你实际传了: {got_keys:?}。{example}请补全参数后原样重试(内容不变,只补上缺失的参数)。]"
    )
}

fn bak_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".bak");
    PathBuf::from(s)
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

/// `_auto_repair` for text files: three or more literal `\n`/`\t` sequences mean the model
/// escaped its newlines; fold them. Python files are left alone (Mingbird byte-compiles
/// them to decide; Boubou has no interpreter to ask).
pub fn auto_repair(path: &Path) -> bool {
    static N: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+n").expect("regex"));
    static T: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\+t").expect("regex"));
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_lowercase();
    if !matches!(ext.as_str(), "md" | "txt" | "csv") {
        return false;
    }
    let Ok(s) = std::fs::read_to_string(path) else {
        return false;
    };
    if !s.contains('\\') || N.find_iter(&s).count() + T.find_iter(&s).count() < 3 {
        return false;
    }
    let fixed = T.replace_all(&N.replace_all(&s, "\n"), "\t").into_owned();
    fixed != s && std::fs::write(path, fixed).is_ok()
}

/// `_bash_snapshot`: `sed -i`, redirections, `cp`/`mv` and friends rewrite files behind
/// the `.bak` net; back up every existing workspace file the command names first.
pub fn bash_snapshot(cmd: &str, workdir: &Path) -> Vec<String> {
    static TOK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[A-Za-z0-9_\-./\\]+\.[A-Za-z0-9]{1,8}").expect("regex"));
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
    ];
    if !suspects.iter().any(|s| low.contains(s)) {
        return Vec::new();
    }
    let wd = resolve_path(workdir);
    let mut out: Vec<String> = Vec::new();
    for tok in TOK.find_iter(cmd).map(|m| m.as_str()) {
        let base = tok.rsplit(['/', '\\']).next().unwrap_or(tok);
        if base.ends_with(".bak") || base.starts_with('.') {
            continue;
        }
        let p = if Path::new(tok).is_absolute() {
            PathBuf::from(tok)
        } else {
            workdir.join(tok)
        };
        let rp = resolve_path(&p);
        if !rp.starts_with(&wd) || rp == wd || !rp.is_file() {
            continue;
        }
        if std::fs::copy(&rp, bak_path(&rp)).is_ok() && !out.iter().any(|b| b == base) {
            out.push(base.to_string());
        }
        if out.len() >= 20 {
            break;
        }
    }
    out
}

#[derive(Debug)]
struct CommandRun {
    code: i32,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

/// Run through the platform shell with a wall-clock limit; on timeout the whole process
/// tree is killed (orphaned grandchildren would otherwise hold the pipes open forever).
fn run_command(cmd: &str, cwd: &Path, timeout: Duration) -> CommandRun {
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
            }
        }
    };
    let reader = |pipe: Option<std::process::ChildStdout>,
                  err: Option<std::process::ChildStderr>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            if let Some(mut e) = err {
                let _ = e.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_t = reader(child.stdout.take(), None);
    let err_t = reader(None, child.stderr.take());
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() >= timeout => {
                timed_out = true;
                kill_tree(child.id());
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let stdout = text::decode_console(&out_t.join().unwrap_or_default());
    let stderr = text::decode_console(&err_t.join().unwrap_or_default());
    let code = status.map(exit_code).unwrap_or(1);
    CommandRun {
        code,
        stdout,
        stderr,
        timed_out,
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

fn kill_tree(pid: u32) {
    use std::process::Command;
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    } else {
        let _ = Command::new("kill")
            .args(["-9", &format!("-{pid}")])
            .output();
    }
}
