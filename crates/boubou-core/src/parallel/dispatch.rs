//! Dispatcher and integrator (`parallel_dispatch.py`).
//!
//! Dispatch: spawn N headless `boubou run` children (small model), each in its own
//! partition directory, depth-locked (`AGENT_CHILD_SANDBOX=1` + `AGENT_PARALLEL=0` +
//! `MINGBIRD_DEPTH+1`), with a hard timeout, abort support and audit sampling.
//! Integrate: deterministic checks (exit code / TASK COMPLETE / claimed artefacts / audit
//! recheck) → copy back to the parent directory (never overwriting by default) → one retry
//! → fall back to the parent model.
//!
//! Prefill budget: nothing here touches the parent's system prompt. After a dispatch the
//! parent gets one injected user message (dynamic cost, not a prefill tax).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::config::ParallelConfig;
use super::probe::CapacityPlanner;
use super::sandbox::{audit_summary, read_audit, AuditSummary, RuleSet, SEVERE_EXIT_CODE};
use super::trigger::{DispatchItem, TodoProvider};
use crate::text;

/// Harness files in a child partition that are never copied back as artefacts.
pub const HARNESS_FILES: [&str; 8] = [
    ".agent_state.json",
    ".agent_state.meta.json",
    "todo.json",
    "task.md",
    "agent.log",
    "audit.log",
    ".hb_pending_images.json",
    "task_input.txt",
];

/// The child's task file: the item text plus explicit boundaries (the partition directory
/// is the child's whole world).
pub fn task_file_text(text: &str) -> String {
    format!(
        "[simple] {text}\n\nConstraints:\n- Create the output file in THIS directory only (relative path, no drive letters, no ..).\n- One input/output scope, no cross-file dependencies, do not delete or overwrite anything.\n- If you cannot complete it deterministically, call finish(summary=\"FAILED: reason\").\n"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildStatus {
    Pending,
    Running,
    Ok,
    Failed,
    Timeout,
    Severe,
    Aborted,
    LoadFailure,
}

impl ChildStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ChildStatus::Pending => "pending",
            ChildStatus::Running => "running",
            ChildStatus::Ok => "ok",
            ChildStatus::Failed => "failed",
            ChildStatus::Timeout => "timeout",
            ChildStatus::Severe => "severe",
            ChildStatus::Aborted => "aborted",
            ChildStatus::LoadFailure => "load_failure",
        }
    }
}

/// One dispatched sub-task.
#[derive(Debug, Clone)]
pub struct ChildTask {
    pub item: DispatchItem,
    pub index: usize,
    pub child_dir: PathBuf,
    pub attempt: usize,
    pub status: ChildStatus,
    pub exit_code: Option<i32>,
    pub reason: String,
    /// Artefact files (relative to `child_dir`) found after a successful run.
    pub files: Vec<String>,
    pub audit: AuditSummary,
    pub elapsed_s: f64,
}

impl ChildTask {
    pub fn new(item: DispatchItem, index: usize) -> Self {
        Self {
            item,
            index,
            child_dir: PathBuf::new(),
            attempt: 0,
            status: ChildStatus::Pending,
            exit_code: None,
            reason: String::new(),
            files: Vec::new(),
            audit: AuditSummary::default(),
            elapsed_s: 0.0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DispatchResult {
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    /// Items handed back to the parent model, with the reason.
    pub fallback: Vec<(DispatchItem, String)>,
    pub severe_violations: usize,
    pub aborted: bool,
    pub elapsed_s: f64,
    pub children: Vec<ChildTask>,
}

impl DispatchResult {
    pub fn ok_items(&self) -> impl Iterator<Item = &ChildTask> {
        self.children.iter().filter(|c| c.status == ChildStatus::Ok)
    }
}

// ---------------- processes (injectable) ----------------

/// A running child as the dispatcher sees it.
pub trait ChildProcess: Send {
    /// Exit code when finished (`-9`-style negative for a signal), `None` while running.
    fn poll(&mut self) -> Option<i32>;
    /// Kill the whole process tree.
    fn kill(&mut self) -> bool;
    fn pid(&self) -> u32;
}

#[derive(Debug)]
pub struct SpawnRequest<'a> {
    pub cmd: &'a [String],
    pub env: &'a HashMap<String, String>,
    pub cwd: &'a Path,
    pub log_path: &'a Path,
}

/// Starts children. The production spawner runs real processes; tests script one.
pub trait Spawner: Send + Sync {
    fn spawn(&self, req: &SpawnRequest<'_>) -> Result<Box<dyn ChildProcess>, String>;
}

/// Real processes: stdout+stderr into the partition's `agent.log`, stdin closed, own
/// process group so a hung child's subprocesses die with it.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessSpawner;

struct OsChild(std::process::Child);

impl ChildProcess for OsChild {
    fn poll(&mut self) -> Option<i32> {
        match self.0.try_wait() {
            Ok(Some(st)) => Some(st.code().unwrap_or(-9)),
            Ok(None) => None,
            Err(_) => Some(-1),
        }
    }

    fn kill(&mut self) -> bool {
        if self.poll().is_some() {
            return true;
        }
        let pid = self.0.id();
        if cfg!(windows) {
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        } else {
            // The child was started as a process-group leader (pgid == pid).
            let _ = Command::new("kill")
                .args(["-9", &format!("-{pid}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
        true
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Spawner for ProcessSpawner {
    fn spawn(&self, req: &SpawnRequest<'_>) -> Result<Box<dyn ChildProcess>, String> {
        let Some((exe, args)) = req.cmd.split_first() else {
            return Err("empty command".to_string());
        };
        let log = File::create(req.log_path).map_err(|e| format!("agent.log: {e}"))?;
        let err = log.try_clone().map_err(|e| format!("agent.log: {e}"))?;
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .current_dir(req.cwd)
            .env_clear()
            .envs(req.env)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        let child = cmd.spawn().map_err(|e| e.to_string())?;
        Ok(Box::new(OsChild(child)))
    }
}

// ---------------- the dispatcher ----------------

pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Sleeper = Arc<dyn Fn(f64) + Send + Sync>;
/// Progress callback; events are JSON objects with a `phase` (`start` / `progress` / `done`).
pub type Progress = Arc<dyn Fn(Value) + Send + Sync>;

fn wall_clock() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Runs N headless children in parallel over the simple items. Abortable, nothing hard-coded.
pub struct ParallelDispatcher {
    pub cfg: ParallelConfig,
    spawner: Arc<dyn Spawner>,
    progress: Progress,
    clock: Clock,
    sleep: Sleeper,
    env: HashMap<String, String>,
    abort: Arc<AtomicBool>,
    /// The agent binary the children run (`boubou`), default: this executable.
    pub agent_exe: PathBuf,
}

impl fmt::Debug for ParallelDispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParallelDispatcher")
            .field("cfg", &self.cfg)
            .field("agent_exe", &self.agent_exe)
            .field("aborted", &self.aborted())
            .finish_non_exhaustive()
    }
}

impl ParallelDispatcher {
    pub fn new(cfg: ParallelConfig) -> Self {
        Self {
            cfg,
            spawner: Arc::new(ProcessSpawner),
            progress: Arc::new(|_| {}),
            clock: Arc::new(wall_clock),
            sleep: Arc::new(|s: f64| std::thread::sleep(std::time::Duration::from_secs_f64(s))),
            env: super::config::env_snapshot(),
            abort: Arc::new(AtomicBool::new(false)),
            agent_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("boubou")),
        }
    }

    pub fn with_spawner(mut self, s: Arc<dyn Spawner>) -> Self {
        self.spawner = s;
        self
    }

    pub fn with_progress(mut self, p: Progress) -> Self {
        self.progress = p;
        self
    }

    pub fn with_clock(mut self, c: Clock, s: Sleeper) -> Self {
        self.clock = c;
        self.sleep = s;
        self
    }

    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    pub fn with_agent_exe(mut self, exe: impl Into<PathBuf>) -> Self {
        self.agent_exe = exe.into();
        self
    }

    /// Abort on the caller's flag: the agent's stop flag, so a user stop (Ctrl-C in the
    /// CLI, the web page's stop button) reaches the children.
    pub fn with_abort(mut self, flag: Arc<AtomicBool>) -> Self {
        self.abort = flag;
        self
    }

    /// Share the abort flag (a GUI stop button sets it).
    pub fn abort_flag(&self) -> Arc<AtomicBool> {
        self.abort.clone()
    }

    /// Abortable throughout: set it and every worker kills its live child at the next poll.
    pub fn abort(&self) {
        self.abort.store(true, Ordering::SeqCst);
    }

    pub fn aborted(&self) -> bool {
        self.abort.load(Ordering::SeqCst)
    }

    // ---- directories, task files, env, command ----

    fn child_dir(run_dir: &Path, index: usize, attempt: usize) -> PathBuf {
        let mut name = format!("task{:02}", index + 1);
        if attempt > 0 {
            name.push_str(&format!("_retry{attempt}"));
        }
        run_dir.join(name)
    }

    /// The child's environment: the parent's, plus the depth locks and the planned context
    /// window, minus the knobs that only make sense for the parent (streaming, the
    /// parent's time budget).
    pub fn child_env(&self, depth: usize) -> HashMap<String, String> {
        let mut env = self.env.clone();
        env.insert("AGENT_CHILD_SANDBOX".to_string(), "1".to_string());
        env.insert("AGENT_PARALLEL".to_string(), "0".to_string());
        env.insert("MINGBIRD_DEPTH".to_string(), (depth + 1).to_string());
        // The capacity planner costs each child's KV cache at `child_ctx`; the child must
        // actually run there. Left unset it would inherit the parent's `AGENT_CTX` or the
        // 128K default, 16x the budgeted memory for a 2B model (review 2026-10-29).
        // Mingbird never set it either.
        env.insert("AGENT_CTX".to_string(), self.cfg.child_ctx.to_string());
        env.remove("HUMMINGBIRD_DEPTH");
        env.remove("AGENT_STREAM");
        // The parent's 50/75/90% pacing is for the parent's budget; the child's real limit
        // is `per_task_timeout_s`.
        env.remove("AGENT_TIME_BUDGET_SEC");
        env
    }

    /// `boubou run <model> <task.md> <child_dir> --session <s> --new`.
    pub fn child_cmd(
        &self,
        model: &str,
        task_path: &Path,
        child_dir: &Path,
        session: &str,
    ) -> Vec<String> {
        vec![
            self.agent_exe.to_string_lossy().into_owned(),
            "run".to_string(),
            model.to_string(),
            task_path.to_string_lossy().into_owned(),
            child_dir.to_string_lossy().into_owned(),
            "--session".to_string(),
            session.to_string(),
            "--new".to_string(),
        ]
    }

    fn emit(&self, ev: Value) {
        (self.progress)(ev);
    }

    // ---- main entry ----

    /// Dispatch `items` (already classified as dispatchable). Any dispatcher-level fault
    /// degrades to "hand everything back to the parent"; the main task never fails here.
    pub fn run(
        &self,
        items: Vec<DispatchItem>,
        parent_workdir: &Path,
        probe: &dyn CapacityPlanner,
        child_model: Option<&str>,
        run_id: Option<&str>,
        depth: usize,
    ) -> DispatchResult {
        let t0 = (self.clock)();
        let cfg = &self.cfg;
        let model = child_model
            .map(String::from)
            .unwrap_or_else(|| cfg.child_model.clone());
        let run_id = run_id
            .map(String::from)
            .unwrap_or_else(|| chrono::Local::now().format("%Y%m%d_%H%M%S").to_string());
        let mut result = DispatchResult {
            total: items.len(),
            ..Default::default()
        };
        let max_par = probe.plan_capacity(&model, cfg).max_parallel;
        if max_par < 1 {
            // Capacity re-checked as zero after the trigger (resources moved on).
            for it in items {
                result
                    .fallback
                    .push((it, "capacity_lost_before_spawn".to_string()));
                result.failed += 1;
            }
            result.elapsed_s = (self.clock)() - t0;
            return result;
        }
        let cap = max_par.min(cfg.hard_cap).max(1);
        let run_dir = parent_workdir.join(&cfg.dispatch_dirname).join(&run_id);
        if let Err(e) = std::fs::create_dir_all(&run_dir) {
            for it in items {
                result.fallback.push((it, format!("dispatcher_error({e})")));
                result.failed += 1;
            }
            result.elapsed_s = (self.clock)() - t0;
            return result;
        }
        self.emit(json!({
            "phase": "start", "total": result.total, "model": model,
            "max_parallel": cap, "run_id": run_id,
        }));

        let total = result.total;
        let queue: Mutex<VecDeque<ChildTask>> = Mutex::new(
            items
                .into_iter()
                .enumerate()
                .map(|(i, it)| ChildTask::new(it, i))
                .collect(),
        );
        let done: Mutex<Vec<ChildTask>> = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..cap {
                s.spawn(|| loop {
                    if self.aborted() {
                        break;
                    }
                    let Some(mut ct) = queue.lock().ok().and_then(|mut q| q.pop_front()) else {
                        break;
                    };
                    ct.status = ChildStatus::Running;
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.run_one(&mut ct, &run_dir, &model, depth, total);
                    }));
                    if outcome.is_err() {
                        ct.status = ChildStatus::Failed;
                        ct.reason = "child_error(panic in dispatcher)".to_string();
                    }
                    if let Ok(mut d) = done.lock() {
                        d.push(ct);
                    }
                });
            }
        });
        let mut children = done.into_inner().unwrap_or_default();
        for mut ct in queue.into_inner().unwrap_or_default() {
            ct.status = ChildStatus::Aborted;
            ct.reason = "aborted_by_user".to_string();
            children.push(ct);
        }
        children.sort_by_key(|c| c.index);
        result.aborted = self.aborted();
        for ct in &children {
            if ct.status == ChildStatus::Ok {
                result.ok += 1;
                continue;
            }
            if ct.status == ChildStatus::Severe {
                result.severe_violations += 1;
            }
            let reason = if ct.reason.is_empty() {
                ct.status.as_str().to_string()
            } else {
                ct.reason.clone()
            };
            result.fallback.push((ct.item.clone(), reason));
            result.failed += 1;
        }
        result.children = children;
        result.elapsed_s = (self.clock)() - t0;
        self.emit(json!({
            "phase": "done", "total": result.total, "ok": result.ok,
            "failed": result.failed, "fallback": result.fallback.len(),
            "severe_violations": result.severe_violations, "aborted": result.aborted,
            "elapsed_s": (result.elapsed_s * 10.0).round() / 10.0,
        }));
        result
    }

    // ---- one sub-task, with retries ----
    fn run_one(&self, ct: &mut ChildTask, run_dir: &Path, model: &str, depth: usize, total: usize) {
        let t0 = (self.clock)();
        let run_name = run_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for attempt in 0..=self.cfg.max_retries {
            if self.aborted() {
                ct.status = ChildStatus::Aborted;
                ct.reason = "aborted_by_user".to_string();
                ct.elapsed_s = (self.clock)() - t0;
                return;
            }
            ct.attempt = attempt;
            let child_dir = Self::child_dir(run_dir, ct.index, attempt);
            ct.child_dir = child_dir.clone();
            let task_path = child_dir.join("task.md");
            let prepared = std::fs::create_dir_all(&child_dir)
                .and_then(|_| std::fs::write(&task_path, task_file_text(&ct.item.text)));
            if let Err(e) = prepared {
                ct.status = ChildStatus::Failed;
                ct.reason = format!("child_error({e})");
                break;
            }
            let session = format!(
                "disp_{run_name}_{:02}{}",
                ct.index + 1,
                if attempt > 0 {
                    format!("_r{attempt}")
                } else {
                    String::new()
                }
            );
            let cmd = self.child_cmd(model, &task_path, &child_dir, &session);
            let env = self.child_env(depth);
            let log_path = child_dir.join("agent.log");
            let req = SpawnRequest {
                cmd: &cmd,
                env: &env,
                cwd: &child_dir,
                log_path: &log_path,
            };
            let mut proc = match self.spawner.spawn(&req) {
                Ok(p) => p,
                Err(e) => {
                    ct.status = ChildStatus::Failed;
                    ct.reason = format!("spawn_error({e})");
                    break;
                }
            };
            let (status, reason, rc) = self.wait(&mut *proc, &child_dir, ct, total);
            ct.status = status;
            ct.reason = reason;
            ct.exit_code = rc;
            if status == ChildStatus::Ok {
                break;
            }
            // A severe violation is not retried: the model has shown dangerous intent.
            if status == ChildStatus::Severe && !self.cfg.retry_on_severe {
                break;
            }
            // Load failure / OOM: abort the whole batch, a retry would only OOM again.
            if status == ChildStatus::LoadFailure {
                self.abort();
                break;
            }
        }
        ct.elapsed_s = (self.clock)() - t0;
        self.finalize(ct, total);
    }

    /// Wait with hard timeout, abort and audit sampling. `(status, reason, exit_code)`.
    fn wait(
        &self,
        proc: &mut dyn ChildProcess,
        child_dir: &Path,
        ct: &mut ChildTask,
        total: usize,
    ) -> (ChildStatus, String, Option<i32>) {
        let deadline = (self.clock)() + self.cfg.per_task_timeout_s;
        let poll = self.cfg.poll_interval_s.max(0.001);
        let audit_every =
            ((self.cfg.audit_poll_interval_s / poll.max(0.05)).floor() as usize).max(1);
        let mut i = 0usize;
        loop {
            if let Some(rc) = proc.poll() {
                let (st, why) = self.judge(rc, child_dir, ct);
                return (st, why, Some(rc));
            }
            if self.aborted() {
                proc.kill();
                return (ChildStatus::Aborted, "aborted_by_user".to_string(), None);
            }
            if (self.clock)() > deadline {
                proc.kill();
                return (
                    ChildStatus::Timeout,
                    format!(
                        "hard_timeout({}s)",
                        text::fmt_g(self.cfg.per_task_timeout_s)
                    ),
                    None,
                );
            }
            i += 1;
            if i.is_multiple_of(audit_every) {
                let summ = audit_summary(&read_audit(&child_dir.join("audit.log")));
                ct.audit = summ.clone();
                if summ.severes > 0 {
                    // The audit shows a severe violation while the child is still running.
                    proc.kill();
                    return (
                        ChildStatus::Severe,
                        "severe_violation(audit found dangerous behaviour)".to_string(),
                        None,
                    );
                }
                self.emit(json!({
                    "phase": "progress", "index": ct.index, "total": total,
                    "task": text::head(&ct.item.text, 60), "status": "running",
                    "denies": summ.denies, "severes": summ.severes,
                }));
            }
            (self.sleep)(poll);
        }
    }

    /// Deterministic acceptance (first half of integration): exit code, TASK COMPLETE,
    /// audit recheck, artefacts.
    fn judge(&self, rc: i32, child_dir: &Path, ct: &mut ChildTask) -> (ChildStatus, String) {
        let audit_path = child_dir.join("audit.log");
        if rc == SEVERE_EXIT_CODE {
            ct.audit = audit_summary(&read_audit(&audit_path));
            return (
                ChildStatus::Severe,
                "severe_violation(sub-agent sandbox circuit broken, exit 53)".to_string(),
            );
        }
        if rc != 0 {
            ct.audit = audit_summary(&read_audit(&audit_path));
            if looks_like_load_failure(child_dir) {
                return (
                    ChildStatus::LoadFailure,
                    format!("child_model_load_failure(exit {rc})"),
                );
            }
            return (ChildStatus::Failed, format!("child_exit_{rc}"));
        }
        let log_text = read_head(&child_dir.join("agent.log"), 200_000);
        if !log_text.contains("TASK COMPLETE") {
            return (
                ChildStatus::Failed,
                "no_task_complete(the sub-agent did not finish cleanly)".to_string(),
            );
        }
        let summ = audit_summary(&read_audit(&audit_path));
        ct.audit = summ.clone();
        if summ.severes > 0 {
            return (
                ChildStatus::Severe,
                "severe_violation(audit re-check found dangerous behaviour)".to_string(),
            );
        }
        if summ.denies >= self.cfg.max_denies_per_child {
            return (
                ChildStatus::Failed,
                format!("too_many_denies({})", summ.denies),
            );
        }
        let summary = child_summary(child_dir);
        let missing = claimed_missing(&summary, child_dir);
        if !missing.is_empty() {
            return (
                ChildStatus::Failed,
                format!(
                    "missing_artifacts({})",
                    missing
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        let artifacts = artifact_files(child_dir);
        if artifacts.is_empty() {
            return (
                ChildStatus::Failed,
                "no_artifacts(the partition directory holds no output files)".to_string(),
            );
        }
        ct.files = artifacts;
        (ChildStatus::Ok, String::new())
    }

    fn finalize(&self, ct: &ChildTask, total: usize) {
        let mut ev = json!({
            "phase": "progress", "index": ct.index, "total": total,
            "task": text::head(&ct.item.text, 60), "status": ct.status.as_str(),
            "denies": ct.audit.denies, "severes": ct.audit.severes,
        });
        if ct.status != ChildStatus::Ok {
            ev["reason"] = Value::String(text::head(&ct.reason, 120).to_string());
        }
        self.emit(ev);
    }
}

/// Heuristic for a small model that failed to load / OOM: a loader error in the log, or a
/// quick exit with no artefacts and a short log.
fn looks_like_load_failure(child_dir: &Path) -> bool {
    let log = read_head(&child_dir.join("agent.log"), 200_000).to_lowercase();
    const SIGNALS: [&str; 8] = [
        "model not found",
        "does not exist",
        "failed to load",
        "out of memory",
        "oom",
        "llm runner",
        "unable to load",
        "no such model",
    ];
    if SIGNALS.iter().any(|s| log.contains(s)) {
        return true;
    }
    artifact_files(child_dir).is_empty() && log.chars().count() < 4000
}

fn read_head(path: &Path, limit: usize) -> String {
    std::fs::read(path)
        .map(|b| {
            let s = text::decode_lenient(&b);
            text::head(&s, limit).to_string()
        })
        .unwrap_or_default()
}

/// The child's finish summary from its checkpoint: the last assistant message carrying
/// `[TASK_COMPLETE]` or a `finish` tool call.
pub fn child_summary(child_dir: &Path) -> String {
    let Ok(raw) = std::fs::read_to_string(child_dir.join(".agent_state.json")) else {
        return String::new();
    };
    let Ok(Value::Array(msgs)) = serde_json::from_str::<Value>(&raw) else {
        return String::new();
    };
    for m in msgs.iter().rev() {
        if m.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let c = m.get("content").and_then(Value::as_str).unwrap_or("");
        if let Some((_, after)) = c.split_once("[TASK_COMPLETE]") {
            return after.trim().to_string();
        }
        for tc in m
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let f = tc.get("function");
            if f.and_then(|f| f.get("name")).and_then(Value::as_str) == Some("finish") {
                let args = f
                    .and_then(|f| f.get("arguments"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let args = match args {
                    Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
                    other => other,
                };
                return args
                    .get("summary")
                    .map(|s| match s {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
            }
        }
    }
    String::new()
}

static CLAIM_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"[A-Za-z0-9_\-\x{4e00}-\x{9fff}]+\.[A-Za-z0-9]{2,5}").expect("regex")
});
const CLAIM_EXTS: [&str; 18] = [
    ".md", ".txt", ".png", ".jpg", ".jpeg", ".csv", ".py", ".json", ".docx", ".xlsx", ".pdf",
    ".html", ".yaml", ".yml", ".svg", ".tex", ".tsv", ".log",
];

/// Files the child's summary claims but did not produce (same rule as the parent's gate).
pub fn claimed_missing(summary: &str, child_dir: &Path) -> Vec<String> {
    let mut toks: Vec<String> = Vec::new();
    for m in CLAIM_RE.find_iter(summary) {
        let t = m.as_str();
        let l = t.to_lowercase();
        if CLAIM_EXTS.iter().any(|e| l.ends_with(e)) && !toks.iter().any(|x| x == t) {
            toks.push(t.to_string());
        }
    }
    let present = artifact_files(child_dir);
    let mut missing = Vec::new();
    for tok in toks {
        let rel = tok.replace('\\', "/");
        let rel = rel.trim_start_matches(['.', '/']);
        let p = child_dir.join(rel);
        if p.is_file() && std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false) {
            continue;
        }
        let base = Path::new(rel)
            .file_name()
            .map(|b| b.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let found = present.iter().any(|b| {
            Path::new(b)
                .file_name()
                .map(|f| f.to_string_lossy().to_lowercase() == base)
                .unwrap_or(false)
        });
        if !found {
            missing.push(tok);
        }
    }
    missing
}

/// Non-harness, non-empty files in the partition (relative, `/`-separated, sorted, recursive).
pub fn artifact_files(child_dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                if !matches!(name.as_str(), "__pycache__" | ".git") {
                    walk(root, &p, out);
                }
                continue;
            }
            if HARNESS_FILES.contains(&name.as_str()) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&p) else {
                continue;
            };
            if meta.len() == 0 {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(child_dir, child_dir, &mut out);
    out.sort();
    out
}

// ---------------- the integrator ----------------

/// Acceptance + copy-back + todo bookkeeping. Deterministic checks only; the parent model
/// never eyeballs child output.
pub struct ResultIntegrator<'a> {
    pub cfg: &'a ParallelConfig,
    provider: Option<&'a mut dyn TodoProvider>,
    rules: RuleSet,
}

impl fmt::Debug for ResultIntegrator<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResultIntegrator")
            .field("has_provider", &self.provider.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> ResultIntegrator<'a> {
    pub fn new(cfg: &'a ParallelConfig) -> Self {
        Self {
            cfg,
            provider: None,
            rules: RuleSet::from_config(cfg),
        }
    }

    pub fn with_provider(mut self, p: &'a mut dyn TodoProvider) -> Self {
        self.provider = Some(p);
        self
    }

    /// A verify command comes from item text; the harness never runs it unchecked.
    pub fn verify_cmd_allowed(&self, cmd: &str) -> bool {
        self.rules.find(&norm_command_pub(cmd)).is_none()
    }

    /// Copy a child's artefacts into the parent directory. `(copied, conflicts)`; existing
    /// files are conflicts unless `allow_overwrite_existing`.
    pub fn collect(
        &self,
        child_dir: &Path,
        files: &[String],
        parent: &Path,
    ) -> (Vec<String>, Vec<String>) {
        let mut copied = Vec::new();
        let mut conflicts = Vec::new();
        for rel in files {
            let src = child_dir.join(rel);
            let dst = parent.join(rel);
            if dst.exists() && !self.cfg.allow_overwrite_existing {
                conflicts.push(rel.clone());
                continue;
            }
            if let Some(d) = dst.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            match std::fs::copy(&src, &dst) {
                Ok(_) => copied.push(rel.clone()),
                Err(_) => conflicts.push(rel.clone()),
            }
        }
        (copied, conflicts)
    }

    /// Integrate a whole result: copy back and mark todo items done.
    /// Returns `(integrated_files, conflicts, report_text)`.
    pub fn integrate(
        &mut self,
        result: &mut DispatchResult,
        parent: &Path,
    ) -> (Vec<String>, Vec<String>, String) {
        let mut integrated = Vec::new();
        let mut conflicts = Vec::new();
        let mut notes = Vec::new();
        for ct in result
            .children
            .iter_mut()
            .filter(|c| c.status == ChildStatus::Ok)
        {
            let (copied, conf) = self.collect(&ct.child_dir, &ct.files, parent);
            integrated.extend(copied.iter().cloned());
            conflicts.extend(conf.iter().cloned());
            if !conf.is_empty() {
                notes.push(format!(
                    "[{}] some artefacts conflicted and were not overwritten: {}",
                    ct.item.id,
                    conf.join(", ")
                ));
                ct.status = ChildStatus::Failed;
                ct.reason = "overwrite_conflict(the parent directory already has a file of that name; not overwritten by default)".to_string();
            } else {
                notes.push(format!(
                    "[{}] completed and copied back: {}",
                    ct.item.id,
                    copied.join(", ")
                ));
                if let Some(p) = self.provider.as_deref_mut() {
                    p.mark_done(&ct.item.id, "done-by-subagent");
                }
            }
        }
        for ct in result
            .children
            .iter()
            .filter(|c| c.status != ChildStatus::Ok)
        {
            notes.push(format!(
                "[{}] not completed ({}: {}); the main model takes over",
                ct.item.id,
                ct.status.as_str(),
                ct.reason
            ));
        }
        (integrated, conflicts, notes.join("\n"))
    }
}

fn norm_command_pub(cmd: &str) -> String {
    super::sandbox::norm_command(cmd)
}

/// `summary_for_parent`: the one user message the parent model gets after a dispatch — the
/// first time it hears that sub-agents exist. Empty when there is nothing to say.
pub fn summary_for_parent(result: &DispatchResult, integrated: &[String]) -> String {
    let mut lines = Vec::new();
    if result.aborted {
        lines.push("Parallel dispatch was cancelled.".to_string());
    }
    if !integrated.is_empty() {
        lines.push(
            "The following items were completed by parallel sub-tasks; their output files are in the current working directory (use them as they are, do not redo them):"
                .to_string(),
        );
        for rel in integrated {
            lines.push(format!("- {rel}"));
        }
    }
    if !result.fallback.is_empty() {
        lines.push("The following items were not completed by their sub-tasks; do them yourself as originally planned:".to_string());
        let mut seen = std::collections::BTreeSet::new();
        for (it, why) in &result.fallback {
            if !seen.insert(it.id.clone()) {
                continue;
            }
            lines.push(format!(
                "- ({}) {}  [reason: {why}]",
                it.id,
                text::head(&it.text, 120)
            ));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    lines.push("Check the artefacts above against your plan, then carry on.".to_string());
    lines.join("\n")
}

/// `@@DISPATCH@@{json}`: the stdout protocol line a GUI parses (older GUIs drop it as a
/// plain log line).
pub fn progress_line(ev: &Value) -> String {
    format!("@@DISPATCH@@{ev}")
}
