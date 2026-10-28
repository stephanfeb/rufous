//! `agent_loop`: the harness around the model. Every guard here exists because a real
//! model session failed in a specific way; the Chinese message strings are Mingbird's and
//! are pinned by the ported tests, so change them together with the tests.
//!
//! Shape: up to `max_iterations` turns. Each turn: flush transcript → dedupe tail →
//! assemble the tool face → pre-call token estimate (compaction ladder) → model call
//! (with the 4xx/5xx triage) → post-call context ladder → either run the tool calls through
//! the guard stack, or handle a text-only turn (empty-turn ladder, repeat detection,
//! Q&A wrap-up). `finish` runs the gate sequence and returns.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::chat::DynBackend;
use super::chat::{ChatBackend, StreamPiece};
use super::face::{self, ToolFace, MUTATE, PRODUCTIVE, RESEARCH_ALL};
use super::history::{
    attach_pending_images, compact_history, dedupe_trailing_assistant, estimate_messages_tokens,
    similar, workdir_files_text,
};
use super::openai::OpenAiBackend;
use super::prompt;
use super::session::{self, Transcript};
use super::testguard::{self, TestVerdict};
use super::{AgentEvent, RunParams};
use crate::exec::Executor;
use crate::guards::budget::{self, BudgetMark};
use crate::guards::edit::EditConvergenceGuard;
use crate::guards::{self, artifacts, leak, qa};
use crate::ollama::{FunctionCall, Message, Role, ToolCall};
use crate::ollama_proc::OllamaProc;
use crate::parallel::{env_snapshot, DispatchHook, HookEvent, ParallelConfig};
use crate::todo::{format_todo_lines, load_todo, load_todo_str, todo_progress};
use crate::tools::{is_mcp_tool, required_args, resolve_alias};
use crate::{text, Error, Result};

/// `_FAIL_MARKERS`: a tool result that means "the action did not happen". Safety-ring
/// refusals count too, otherwise a model that keeps changing paths never hits the ladder.
pub const FAIL_MARKERS: [&str; 24] = [
    "[edit failed",
    "[tool error",
    "[not found",
    "[web_search error",
    "[web_fetch error",
    "[mcp_call error",
    "not found in",
    "error:",
    "cannot find the path",
    "no such file",
    "is not recognized",
    "command not found",
    "not recognized as",
    "access is denied",
    "did not match",
    "fatal:",
    "unknown option",
    "[safety gate",
    "[safety net",
    "[child safety gate",
    "[overwrite guard",
    "[edit failed:",
    "[tool error:",
    "[not found:",
];

/// `_is_tool_error`: marker scan over the first 200 chars, case-insensitive.
pub fn is_tool_error(res: &str) -> bool {
    let r = text::head(res, 200).to_lowercase();
    FAIL_MARKERS.iter().any(|m| r.contains(m))
}

const CONTINUE_NOTE: &str =
    "Continue: keep making real progress with tools, or call finish only when fully verified/answered.";

/// How many recent text-only outputs the repeat detector compares against.
const TEXT_WINDOW: usize = 4;
/// The text-only task turn that draws the sharper "tool call or finish" note (once).
const TALK_SHARP_NOTE: usize = 2;
/// Consecutive text-only task turns after which the turn is ended.
const TALK_FORCE_END: usize = 4;

/// Prefix of the loop's own missing-argument notes, so pruning can recognise them.
const MISSING_ARG_NOTE_MARK: &str = "⚠️ [missing argument] ";

/// `'key'` named in an executor missing-argument error (`[tool error: X is missing argument 'key'. …`).
pub fn missing_arg_key(res: &str) -> Option<&str> {
    let head = text::head(res, 200);
    if !head.starts_with("[tool error:") {
        return None;
    }
    let after = head.split("is missing argument '").nth(1)?;
    let key = after.split('\'').next()?;
    (!key.is_empty()).then_some(key)
}

/// Required arguments of a built-in tool that `args` does not supply (MCP tools: none).
fn missing_required(name: &str, args: &Map<String, Value>) -> Vec<String> {
    if is_mcp_tool(name) {
        return Vec::new();
    }
    required_args(name)
        .into_iter()
        .filter(|k| !args.contains_key(k))
        .collect()
}

/// Remove earlier failed `tool` calls that lacked `key` — the assistant turn (one call), its
/// missing-argument tool error and the loop's own note after it — so the model is not
/// few-shotted by its own mistake. The last message (the call being handled now) is kept;
/// the system message and the first user turn are never touched. Returns the number of
/// turns removed.
fn prune_missing_arg_calls(messages: &mut Vec<Message>, tool: &str, key: &str) -> usize {
    let mut keep = vec![true; messages.len()];
    let mut removed = 0;
    let last = messages.len().saturating_sub(1);
    let mut i = 2.min(last);
    while i + 1 < last {
        let m = &messages[i];
        let failed_call = m.role == Role::Assistant
            && m.tool_calls.len() == 1
            && m.tool_calls[0].function.name == tool
            && !args_map(&m.tool_calls[0].function.arguments).contains_key(key);
        let next_is_error = messages[i + 1].role == Role::Tool
            && missing_arg_key(&messages[i + 1].content) == Some(key);
        if failed_call && next_is_error {
            keep[i] = false;
            keep[i + 1] = false;
            removed += 1;
            i += 2;
            if i < last
                && messages[i].role == Role::User
                && messages[i].content.starts_with(MISSING_ARG_NOTE_MARK)
            {
                keep[i] = false;
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    if removed > 0 {
        let mut k = keep.into_iter();
        messages.retain(|_| k.next().unwrap_or(true));
    }
    removed
}

const CTX_OVERFLOW_KEYS: [&str; 6] = [
    "prompt is too long",
    "too long",
    "exceeds context",
    "context length",
    "input is too long",
    "context window",
];

/// Executes tool calls for the loop. [`Executor`] is the real one; tests script results.
pub trait ToolRunner {
    fn run_tool(&mut self, name: &str, args: &Value) -> String;
    /// `_SLIM_OUTPUT_MODE`: trim long command output (set once per run).
    fn set_slim_output(&mut self, _on: bool) {}
    /// `mcp_tool_defs`: flattened `server.tool` definitions for the routed categories.
    fn mcp_tool_defs(
        &self,
        _cats: &BTreeSet<String>,
        _extra: &BTreeSet<String>,
        _offline: bool,
    ) -> Vec<Value> {
        Vec::new()
    }
    /// Child mode: the sandbox fuse has tripped; the loop must end with exit 53.
    fn child_terminated(&self) -> Option<String> {
        None
    }
}

impl ToolRunner for Executor {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        Executor::run_tool(self, name, args)
    }
    fn set_slim_output(&mut self, on: bool) {
        self.slim_output = on;
    }
    fn mcp_tool_defs(
        &self,
        cats: &BTreeSet<String>,
        extra: &BTreeSet<String>,
        offline: bool,
    ) -> Vec<Value> {
        self.mcp.tool_defs(cats, extra, offline)
    }
    fn child_terminated(&self) -> Option<String> {
        Executor::child_terminated(self)
    }
}

/// `_ws_note`: the refusal texts always name the real absolute workdir, because small
/// models otherwise guess `/workspace` or the home directory and never find out.
pub fn ws_note(workdir: &Path) -> String {
    let abs = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    format!(
        "Your working directory (absolute path) is {}. Every output must be written inside it; prefer relative paths (e.g. create_file(path=\"options.md\")).",
        abs.display()
    )
}

/// `_plan_note`: current plan as text for compaction re-injection; none in Q&A / no plan.
fn plan_note(qa: bool, workdir: &Path) -> Option<String> {
    if qa {
        return None;
    }
    let t = load_todo(workdir);
    if t.is_empty() {
        None
    } else {
        Some(format_todo_lines(&t))
    }
}

/// Tool-call arguments as an object: objects pass, JSON strings are parsed, anything else
/// becomes `{}` (Mingbird `json.loads` fallback).
fn args_map(v: &Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m.clone(),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        _ => Map::new(),
    }
}

fn first_user_text(messages: &[Message]) -> String {
    messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

type Sink = Box<dyn FnMut(AgentEvent) + Send>;

/// The loop plus everything it owns between runs: tool face, transcript cursor, backend.
pub struct Agent<B, T> {
    pub params: RunParams,
    pub backend: B,
    pub tools: T,
    pub face: ToolFace,
    pub transcript: Transcript,
    /// `~/.ollama_agent/sessions` unless relocated.
    pub sessions_dir: PathBuf,
    sink: Sink,
    /// User stop (web UI "stop" button): ends the run at the next check, saving the session.
    stop: Arc<AtomicBool>,
    /// `_maybe_parallel_dispatch`: set by [`Agent::standard`] (or `with_parallel`); `None`
    /// means the hook is absent (tests, embedding).
    pub parallel: Option<DispatchHook>,
    /// Human-readable engine label (`Ollama · host`, `cloud · model`, `OpenAI-compatible · url`).
    pub engine: String,
}

impl<B, T> std::fmt::Debug for Agent<B, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("params", &self.params)
            .field("face", &self.face)
            .field("sessions_dir", &self.sessions_dir)
            .field("engine", &self.engine)
            .finish()
    }
}

impl Agent<DynBackend, Executor> {
    /// The production wiring (`call_chat` dispatch): the cloud provider when enabled and
    /// online, else the configured local engine (Ollama, restartable, or an OpenAI-compatible
    /// server), plus the real executor in the workdir.
    pub fn standard(params: RunParams, cfg: &crate::config::Config) -> Result<Self> {
        let host = cfg.ollama_host();
        let offline = cfg.offline();
        let (backend, engine): (DynBackend, String) = if let Some(cp) = cfg.cloud_provider() {
            let label = format!("cloud · {} ({})", cp.model, cp.base_url);
            (Box::new(OpenAiBackend::cloud(&cp, &params)?), label)
        } else if let Some(eng) = cfg.backend_openai() {
            let label = format!("OpenAI-compatible · {}", eng.base_url);
            (Box::new(OpenAiBackend::local(&eng, &params)?), label)
        } else {
            let b = super::OllamaBackend::new(host.clone(), &params)?
                .with_proc(OllamaProc::from_config(cfg));
            (Box::new(b), format!("Ollama · {host}"))
        };
        let mut tools = Executor::new(params.workdir.clone()).with_configured_mcp();
        tools.slim_output = params.slim_output;
        let pcfg = ParallelConfig::from_config(cfg).with_env(&env_snapshot());
        let child_mode = params.child_mode;
        let mut agent = Agent::new(params, backend, tools);
        agent.engine = engine;
        agent.face.offline = offline;
        if child_mode {
            let allowed = agent.tools.install_child_mode(&pcfg);
            agent.face.narrow_to(&allowed);
        }
        agent.parallel = Some(DispatchHook::new(pcfg, host).with_child_mode(child_mode));
        Ok(agent)
    }
}

impl<B: ChatBackend, T: ToolRunner> Agent<B, T> {
    pub fn new(params: RunParams, backend: B, tools: T) -> Self {
        let home = crate::config::agent_home();
        Self {
            params,
            backend,
            tools,
            face: ToolFace::new(),
            transcript: Transcript::new(),
            sessions_dir: session::sessions_dir(&home),
            sink: Box::new(|_| {}),
            stop: Arc::new(AtomicBool::new(false)),
            parallel: None,
            engine: String::new(),
        }
    }

    /// Attach the parallel-dispatch hook (tests inject a probe and spawner through it).
    pub fn with_parallel(mut self, hook: DispatchHook) -> Self {
        self.parallel = Some(hook);
        self
    }

    /// Share a stop flag with the caller; the backend abandons an in-flight stream when it
    /// is set, and the loop ends before the next turn.
    pub fn with_stop(mut self, flag: Arc<AtomicBool>) -> Self {
        self.backend.set_cancel(flag.clone());
        self.stop = flag;
        self
    }

    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// Receive every [`AgentEvent`] (streamed text, tool receipts, harness log lines).
    pub fn with_sink(mut self, sink: impl FnMut(AgentEvent) + Send + 'static) -> Self {
        self.sink = Box::new(sink);
        self
    }

    pub fn set_sink(&mut self, sink: impl FnMut(AgentEvent) + Send + 'static) {
        self.sink = Box::new(sink);
    }

    /// Build the opening message list for a fresh task.
    pub fn task_messages(&self, task: &str) -> Vec<Message> {
        if self.params.child_mode {
            // A dispatched child: the minimal prompt, no platform block.
            return vec![
                Message::system(prompt::child_system(Some(&self.params.workdir))),
                Message::user(task),
            ];
        }
        let opts = prompt::PromptOptions {
            workdir: Some(&self.params.workdir),
            system_file: self.params.system_file.as_deref(),
            unattended: self.params.unattended,
        };
        vec![
            Message::system(prompt::system_prompt(&opts)),
            Message::user(task),
        ]
    }

    /// `agent_loop(model, messages, workdir, session, budget_sec)`. Returns the final
    /// message list; fatal API errors and the exhausted empty-turn ladder return `Err`
    /// after the session has been saved, so a rerun resumes from the checkpoint.
    pub fn run(
        &mut self,
        mut messages: Vec<Message>,
        session: Option<&str>,
        budget_sec: Option<f64>,
    ) -> Result<Vec<Message>> {
        let Agent {
            params,
            backend,
            tools,
            face,
            transcript,
            sessions_dir,
            sink,
            stop,
            parallel,
            engine: _,
        } = self;
        let child = params.child_mode;
        let model = params.model.as_str();
        let workdir = params.workdir.as_path();
        let ctx_budget = f64::from(params.num_ctx);
        let fg_off = params.ablated("finish_gate") || params.ablated("finish_gate_text_only");

        macro_rules! log {
            ($($arg:tt)*) => { sink(AgentEvent::Log(format!($($arg)*))) };
        }
        macro_rules! save {
            () => {
                session::save_session(sessions_dir, session, &messages, Some(workdir), transcript);
                if session.is_some() {
                    log!("[session saved: {}]", session.unwrap_or_default());
                }
            };
        }

        // Per-run counters (names follow the Python locals).
        let mut fails = 0usize;
        // `_restarts`: engine restarts this run (≤3), the 500-degradation self-heal.
        let mut restarts = 0usize;
        let mut fail_count: HashMap<String, usize> = HashMap::new();
        let mut redirect_warns = 0;
        let mut last_tool: Option<String> = None;
        let mut last_sig: Option<(String, String)> = None;
        let mut same_sig_streak = 0;
        let mut last_sig_now: Option<(String, String)> = None;
        let mut dup_warns = 0;
        let mut disabled_recall: HashMap<String, usize> = HashMap::new();
        let mut tool_streak = 0;
        let mut streak_warns = 0;
        let mut research_streak = 0;
        let mut research_warns = 0;
        let mut research_seen: BTreeSet<String> = BTreeSet::new();
        let mut todo_streak = 0;
        let mut productive_used = false;
        let mut fake_finish_warns = 0;
        let mut finish_claim_warns = 0;
        let mut finish_reread_used = false;
        let mut plan_gate_warns = 0;
        let mut bash_teach_warns = 0;
        let mut empty_turns = 0;
        let mut len_truncated = false;
        let mut missing_arg_streak: HashMap<String, usize> = HashMap::new();
        let mut missing_arg_warns = 0;
        let mut truncated_call_warns = 0;
        let mut empty_resets = 0;
        let mut empty_total = 0;
        let mut test_guard_warns = 0;
        let mut casual_warns = 0;
        let mut casual_force = false;
        // Recent text-only outputs (newest last, at most TEXT_WINDOW): a repeat is a new
        // text resembling any of them, so A/B/C cycles count, not only A/A.
        let mut recent_texts: Vec<String> = Vec::new();
        let mut repeat_count = 0;
        // Consecutive non-empty text-only turns in task mode (no tool call at all).
        let mut talk_streak = 0;
        let mut compact_lvl: u8 = 0;
        let mut todo_stale = 0;
        let mut todo_nudge_warns = 0;
        let mut todo_gate_warns = 0;
        const TODO_STALE_NUDGE: usize = 4;

        if child {
            log!(
                "[child-mode] sandbox installed: tools={:?} workdir={}",
                face.active,
                workdir.display()
            );
        }
        // Q&A or task? A session with real progress treats short follow-ups as task steps.
        // A child never counts as Q&A: its task text is mechanical by construction, and the
        // Q&A path would swap out the sandbox prompt and drop the producing tools.
        let qa = !child && qa::is_qa(&messages, qa::has_task_progress(&messages));
        let cats: Option<BTreeSet<String>> = if qa {
            let cs = prompt::chat_system(Some(workdir));
            let mut m = vec![Message::system(cs)];
            m.extend(messages.into_iter().filter(|m| m.role != Role::System));
            messages = m;
            None
        } else {
            Some(face::route_categories(&qa::task_routing_text(&messages)))
        };
        // Parallel dispatch hook (harness layer; Q&A, children and resumed runs skip it —
        // the hook itself re-checks the child, kill-switch, no-tool-message and receipt gates).
        if !qa && !child {
            if let Some(hook) = parallel.as_ref() {
                messages = hook.maybe_dispatch(messages, workdir, &mut |ev| match ev {
                    HookEvent::Log(l) => sink(AgentEvent::Log(l)),
                    HookEvent::Dispatch(v) => sink(AgentEvent::Dispatch(v)),
                });
            }
        }
        let small_model = !qa && guards::is_small_model(model);
        tools.set_slim_output(params.slim_output);
        if small_model {
            // ≤4B: edit_file's exact-match protocol is the top failure mode; rewrite instead.
            face.disabled.insert("edit_file".to_string());
        }
        let mut edit_guard = EditConvergenceGuard::default();

        // Time budget: explicit parameter beats the env fallback; resume continues the clock.
        let budget_total = budget_sec
            .or(params.budget_env_sec)
            .filter(|b| b.is_finite())
            .unwrap_or(0.0)
            .max(0.0);
        let t0 = Instant::now();
        let prev_elapsed = if budget_total > 0.0 {
            budget::load_elapsed(workdir)
        } else {
            0.0
        };
        if prev_elapsed > 0.0 {
            log!(
                "[budget: resumed with {:.0}min used / {:.0}min budget]",
                prev_elapsed / 60.0,
                budget_total / 60.0
            );
        }
        let mut budget_fired: BTreeSet<BudgetMark> = BTreeSet::new();

        let mut exhausted = true;
        for i in 0..params.max_iterations {
            transcript.flush(sessions_dir, session, &messages);
            if stop.load(Ordering::Relaxed) {
                log!("[stopped] cancelled by user");
                exhausted = false;
                break;
            }
            if casual_force {
                log!("[wrap-up] this turn is sufficient, forcing an end (Q&A repetition / dead-loop guard)");
                messages.push(Message::tool_output(
                    "[TASK_COMPLETE] answered, ending this turn",
                ));
                exhausted = false;
                break;
            }
            dedupe_trailing_assistant(&mut messages);
            let ct: Vec<Value> = match (&cats, qa) {
                (_, true) => face.chat_tool_defs(),
                (Some(c), false) if !params.ablated("flat_prefill") => {
                    let mut defs = face.tools_for_categories(c);
                    defs.extend(tools.mcp_tool_defs(c, &face.extra, face.offline));
                    defs
                }
                _ => face.active_defs(),
            };
            // Pre-call estimate: compact before the server has to refuse.
            let est = estimate_messages_tokens(&messages, &ct) as f64;
            let pre = if est > ctx_budget * 0.90 && compact_lvl < 3 {
                Some(3)
            } else if est > ctx_budget * 0.75 && compact_lvl < 2 {
                Some(2)
            } else if est > ctx_budget * 0.60 && compact_lvl < 1 {
                Some(1)
            } else {
                None
            };
            if let Some(level) = pre {
                let (todo, files) = if level >= 2 {
                    (
                        plan_note(qa, workdir),
                        Some(workdir_files_text(workdir, 30)),
                    )
                } else {
                    (None, None)
                };
                messages = compact(
                    backend, model, messages, level, todo, files, transcript, sink,
                );
                compact_lvl = level;
                log!("[{i}] pre-flight estimate {est:.0} tokens over the limit, compacted (L{level})");
            }
            let n_img = attach_pending_images(&mut messages, workdir);
            if n_img > 0 {
                log!("[{i}] 🖼 attached {n_img} image(s) (multimodal input)");
            }

            let r = match backend.chat(model, &messages, &ct, &mut |p| match p {
                StreamPiece::Token(t) => sink(AgentEvent::Text(t.to_string())),
                StreamPiece::Think(t) => sink(AgentEvent::Thinking(t.to_string())),
            }) {
                Ok(r) => {
                    fails = 0;
                    r
                }
                Err(e) => {
                    if stop.load(Ordering::Relaxed) {
                        log!("[stopped] cancelled by user (stream interrupted)");
                        dedupe_trailing_assistant(&mut messages);
                        save!();
                        return Ok(messages);
                    }
                    let low = e.lower_text();
                    if e.is_4xx() {
                        // Structural 400 (consecutive assistant turns): clean and retry.
                        if low.contains("assistant messages at the end")
                            || low.contains("consecutive")
                        {
                            let before = messages.len();
                            dedupe_trailing_assistant(&mut messages);
                            if messages.len() != before {
                                log!("[{i}] repaired conversation structure (consecutive assistant turns), cleaned up and retrying");
                                fails += 1;
                                if fails >= 6 {
                                    save!();
                                    return Err(Error::Ollama(e.to_string()));
                                }
                                nap(1, params.sleep_scale);
                                continue;
                            }
                        }
                        log!("[{i}] API 4xx error (retrying is pointless, stopped): {e}");
                        save!();
                        return Err(Error::Ollama(e.to_string()));
                    }
                    if CTX_OVERFLOW_KEYS.iter().any(|k| low.contains(k)) {
                        log!(
                            "[{i}] ⚠️ context-overflow error → forcing L3 compaction and retrying"
                        );
                        let todo = plan_note(qa, workdir);
                        let files = Some(workdir_files_text(workdir, 30));
                        messages =
                            compact(backend, model, messages, 3, todo, files, transcript, sink);
                        compact_lvl = 3;
                        fails = 0;
                        nap(1, params.sleep_scale);
                        continue;
                    }
                    fails += 1;
                    if e.is_500() && fails >= 4 && restarts < 3 {
                        // ★ Root cause, not symptom: ≥4 consecutive 500s = a degraded
                        // ollama process (temp=0 replays the same bad path; compaction and
                        // waiting cannot escape). A restart recovers; the checkpoint keeps
                        // the task. OpenAI-compatible engines are not ours to restart.
                        match backend.restart_engine() {
                            Some(ok) => {
                                restarts += 1;
                                log!(
                                    "[{i}] {fails} consecutive 500s → ollama process judged degraded, restarting automatically ({restarts}/3)..."
                                );
                                if ok {
                                    log!("[{i}] ✓ ollama restarted, resuming the task");
                                } else {
                                    log!("[{i}] ⚠ ollama not ready after restart, still retrying");
                                }
                                fails = 0;
                                nap(3, params.sleep_scale);
                                continue;
                            }
                            None => {
                                log!("[{i}] {fails} consecutive 500s (this engine cannot be restarted by Boubou; compacting and retrying)");
                            }
                        }
                    }
                    if e.is_500() && fails >= 2 {
                        if compact_lvl < 3 {
                            let lvl = (compact_lvl + 1).min(3);
                            compact_lvl = lvl;
                            let todo = plan_note(qa, workdir);
                            let files = Some(workdir_files_text(workdir, 30));
                            messages = compact(
                                backend, model, messages, lvl, todo, files, transcript, sink,
                            );
                            log!("[{i}] 500 error, compacted the context (L{lvl}) and retrying");
                        }
                        let last_user_is_note = messages
                            .iter()
                            .rev()
                            .find(|m| m.role == Role::User)
                            .is_some_and(|m| m.content.starts_with("⚠️"));
                        if !last_user_is_note {
                            messages.push(Message::user(
                                "⚠️ System note: the previous request hit a server error. Carry on from the progress you already have; try a different formulation or move on to the next step.",
                            ));
                            log!("[{i}] 500 error, injected a continue note (breaks the dead-loop path)");
                        }
                        nap(2, params.sleep_scale);
                        continue;
                    }
                    let wait = (3u64 << (fails - 1).min(10)).min(60);
                    log!("[{i}] API error: {e} (retry {fails}, wait {wait}s)");
                    nap(wait, params.sleep_scale);
                    if fails >= 8 {
                        log!("===== FAILED: repeated ollama errors (still failing after restart), stopped. Fix the engine and resume to continue from the checkpoint =====");
                        save!();
                        return Err(Error::Ollama(e.to_string()));
                    }
                    continue;
                }
            };

            // Post-call context ladder from the real prompt token count.
            if let Some(pt) = r.prompt_eval_count.filter(|&p| p > 0) {
                let pct = pt * 100 / u64::from(params.num_ctx.max(1));
                log!("[ctx: {pt}/{} = {pct}%]", params.num_ctx);
                sink(AgentEvent::ContextUsage {
                    used: pt,
                    limit: params.num_ctx,
                });
                let ptf = pt as f64;
                if ptf < ctx_budget * 0.60 {
                    compact_lvl = 0; // context fell back: re-arm the ladder
                }
                let post = if ptf > ctx_budget * 0.90 && compact_lvl < 3 {
                    Some(3)
                } else if ptf > ctx_budget * 0.75 && compact_lvl < 2 {
                    Some(2)
                } else if ptf > ctx_budget * 0.60 && compact_lvl < 1 {
                    Some(1)
                } else {
                    None
                };
                if let Some(level) = post {
                    let todo = if level >= 2 {
                        plan_note(qa, workdir)
                    } else {
                        None
                    };
                    messages = compact(
                        backend, model, messages, level, todo, None, transcript, sink,
                    );
                    compact_lvl = level;
                }
            }

            let content = r.message.content.clone();
            let mut tcs: Vec<ToolCall> = r.message.tool_calls.clone();
            if r.done_reason.as_deref() == Some("length")
                && guards::semantically_empty(&content)
                && tcs.is_empty()
            {
                log!(
                    "[{i}] ⚠️ done_reason=length with no text and no calls (eval={:?}): the generation budget was probably burnt",
                    r.eval_count
                );
                len_truncated = true;
            }
            if let Some(th) = r.message.thinking.as_deref().filter(|t| !t.is_empty()) {
                if tcs.is_empty() && guards::semantically_empty(&content) {
                    log!(
                        "[{i}] 💭 thinking {} chars, empty body. Excerpt: {}",
                        text::clen(th),
                        text::head(th, 600)
                    );
                }
            }
            if tcs.is_empty() && !qa {
                // Format leak salvage: the model wrote the call as text.
                if let Some(salvaged) = leak::try_parse_tool_calls(&content) {
                    let names: Vec<&str> = salvaged.iter().map(|(n, _)| n.as_str()).collect();
                    log!(
                        "[{i}] ⚡ salvaged tool call(s) from text: {}",
                        names.join(", ")
                    );
                    tcs = salvaged
                        .into_iter()
                        .map(|(name, args)| ToolCall {
                            function: FunctionCall {
                                name,
                                arguments: Value::Object(args),
                            },
                        })
                        .collect();
                }
            }
            for tc in &mut tcs {
                let n = resolve_alias(&tc.function.name);
                if n != tc.function.name {
                    log!("[{i}] ⚡ alias mapped: {} -> {}", tc.function.name, n);
                    tc.function.name = n.to_string();
                }
            }
            // A call cut off by the generation cap: Ollama still returns it, but only with
            // the leading arguments (`content` before `path` for gemma), so it must not be
            // executed as a plain missing-argument mistake — the model would be told to
            // resend the same oversized content verbatim.
            let truncated_calls: Vec<(String, Vec<String>)> = if r.done_reason.as_deref()
                == Some("length")
            {
                tcs.iter()
                    .filter_map(|tc| {
                        let missing =
                            missing_required(&tc.function.name, &args_map(&tc.function.arguments));
                        (!missing.is_empty()).then(|| (tc.function.name.clone(), missing))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut turn = Message::assistant(content.clone());
            turn.tool_calls = tcs.clone();
            messages.push(turn);
            session::write_checkpoint(workdir, &messages);

            if tcs.is_empty() {
                // ---- text-only turn ----
                if guards::semantically_empty(&content) {
                    empty_turns += 1;
                    empty_total += 1;
                    if len_truncated && empty_turns == 1 {
                        // Truncation debris: the model is not idle, one shot cannot hold it.
                        len_truncated = false;
                        messages.push(Message::user(format!(
                            "⚠️ Your previous output hit the generation token limit ({}) and was truncated without producing a valid tool call. Deliver large content in chunks: write the skeleton or opening part with create_file, then add the rest with append_file in pieces of a few hundred lines at most; or simplify the implementation. Do not try to output a whole large file in one go.",
                            params.num_predict
                        )));
                        continue;
                    }
                    if empty_turns >= 6 && empty_resets < 3 {
                        empty_resets += 1;
                        empty_turns = 0;
                        let task0 = first_user_text(&messages);
                        let files = workdir_files_text(workdir, 30);
                        let head = messages
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Message::system(""));
                        messages = vec![
                            head,
                            Message::user(format!(
                                "{task0}\n\n⚠️ [system] Several consecutive empty outputs; the context has been reset. Progress snapshot:\nPlan:\n{}\nFiles already in the working directory:\n{files}\nContinue from the first unfinished step and make progress with tools immediately. Do not output empty text.",
                                load_todo_str(workdir)
                            )),
                        ];
                        log!("[{i}] ⚠️ 6 consecutive empty turns, hard-resetting the context ({empty_resets}/3, {empty_total} empty turns in total)");
                        transcript.mark_compacted();
                        continue;
                    }
                    if empty_turns >= 3 && empty_resets >= 3 {
                        log!("[{i}] {empty_total} empty turns in total and 3 resets had no effect; exiting gracefully");
                        save!();
                        return Err(Error::Aborted(format!(
                            "{empty_total} empty turns after 3 context resets"
                        )));
                    }
                    if empty_turns == 3 {
                        messages.push(Message::user(
                            "⚠️ You have output empty text repeatedly. Call a tool and do real work now: read_file to read, run_bash to run a command, create_file to write. Do not output empty text.",
                        ));
                        continue;
                    }
                } else {
                    empty_turns = 0;
                    len_truncated = false;
                    talk_streak += 1;
                    let repeated = recent_texts.iter().any(|t| similar(t, &content, 0.75));
                    recent_texts.push(content.clone());
                    if recent_texts.len() > TEXT_WINDOW {
                        recent_texts.remove(0);
                    }
                    if repeated {
                        repeat_count += 1;
                        log!("[{i}] ⚠️ repeated output detected ({repeat_count})");
                        if repeat_count >= 2 {
                            casual_force = true;
                            messages.push(Message::user(
                                "⚠️ You have output nearly identical content repeatedly; this counts as a dead loop. Call finish now to end this turn. Do not repeat yourself.",
                            ));
                        } else {
                            messages.push(Message::user(CONTINUE_NOTE));
                        }
                    } else {
                        repeat_count = 0;
                        if qa || params.chat_mode {
                            // A conversational reply is the answer: end the turn.
                            casual_force = true;
                            messages.push(Message::user(
                                "Your answer is already sufficient; the user is asking a question, not setting a task. Call finish now to end this turn. Do not repeat the output.",
                            ));
                        } else if talk_streak >= TALK_FORCE_END {
                            // The model is talking, not working: bound the loop instead of
                            // nagging until max_iterations.
                            log!("[{i}] ⚠️ {talk_streak} consecutive text-only turns with no tool call; ending the turn");
                            casual_force = true;
                            messages.push(Message::user(
                                "⚠️ You have replied in text several times without calling a tool. The turn ends here; your earlier text stands as the answer.",
                            ));
                        } else if talk_streak == TALK_SHARP_NOTE {
                            messages.push(Message::user(
                                "⚠️ You have replied in text without calling any tool. Reply with a tool call (create_file / run_bash / read_file …) to do the work, or call finish(summary=your answer) if the text already answers the user. Do not reply with more text.",
                            ));
                        } else {
                            messages.push(Message::user(CONTINUE_NOTE));
                        }
                    }
                }
                continue;
            }

            // ---- tool calls ----
            talk_streak = 0;
            for tc in &tcs {
                let name = tc.function.name.clone();
                let args = args_map(&tc.function.arguments);
                let args_v = Value::Object(args.clone());
                let sig_json = args_v.to_string(); // keys sorted: serde_json Map is a BTreeMap
                let res: String;
                // A note that must follow this call's tool output (call → error → note).
                let mut after_output: Option<String> = None;
                let n = name.as_str();

                if (face.is_disabled(n) || !face.is_active(n)) && !is_mcp_tool(n) {
                    // A disabled/absent tool called again. The refusal itself feeds the
                    // loop: strong note at 3, hard reset at 10.
                    let cnt = disabled_recall.entry(name.clone()).or_insert(0);
                    *cnt += 1;
                    let cnt = *cnt;
                    if cnt >= 10 {
                        let task0 = first_user_text(&messages);
                        let head = messages
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Message::system(""));
                        messages = vec![
                            head,
                            Message::user(format!(
                                "{task0}\n\n⚠️ [system] The context has been reset: the tool {name} is disabled, calling it again will not run, and it will no longer appear in your tool list. Progress snapshot:\nPlan:\n{}\nFiles already in the working directory:\n{}\nSwitch to create_file now and write the output files to move the task forward.",
                                load_todo_str(workdir),
                                workdir_files_text(workdir, 30)
                            )),
                        ];
                        disabled_recall.insert(name.clone(), 0);
                        log!("[{i}] ⚠️ {name} called 10 times after being disabled, hard-resetting the context");
                        transcript.mark_compacted();
                        continue;
                    }
                    res = if face.is_disabled(n) {
                        format!("[tool {name} is disabled (repeated-call protection). Do not try to read the same content through another tool: write the output file with create_file, or call finish if you already have enough.]")
                    } else {
                        format!("[tool {name} is not available (not in the tool list assembled for this run: either its category was not loaded, or the name does not exist). Do not invent tool names: create_file for output files, edit_file/create_file to change files, read_file to read; the full list is in your tool definitions.]")
                    };
                    if cnt == 3 {
                        messages.push(Message::user(format!(
                            "⚠️ The tool {name} is unavailable (disabled or not assembled) and you have tried it several times. Calling it again will not run and it will no longer appear in your tool list. Change action now: write output files with create_file; use read_file/list_dir to inspect what exists."
                        )));
                    }
                } else if let Some((_, missing)) =
                    truncated_calls.iter().find(|(nm, _)| *nm == name)
                {
                    // Truncated call (done_reason=length, required args missing): chunking
                    // advice instead of the generic "retry with the same content".
                    truncated_call_warns += 1;
                    let res = format!(
                        "[tool error: the {name} call was truncated by the generation token limit ({}) (done_reason=length, eval={}); arguments incomplete, missing {missing:?}. The arguments were not wrong: the output did not fit in one turn. Do not resend the same content unchanged.]",
                        params.num_predict,
                        r.eval_count.map_or("?".to_string(), |v| v.to_string())
                    );
                    log!(
                        "[{i}] ⚠️ tool call truncated: {name} missing {missing:?} (eval={:?})",
                        r.eval_count
                    );
                    sink(AgentEvent::ToolResult {
                        name: name.clone(),
                        args: args_v.clone(),
                        output: res.clone(),
                        blocked: true,
                    });
                    messages.push(Message::tool_output(res));
                    if truncated_call_warns <= 3 {
                        messages.push(Message::user(format!(
                            "⚠️ Your previous {name} call hit the generation token limit ({}) and was truncated; the trailing arguments {missing:?} were lost. Deliver large content in chunks: first create_file(path=\"<filename>\", content=\"<opening part>\") for the skeleton or first half, then append_file(path=\"<filename>\", content=\"<next part>\") piece by piece (a few hundred lines each at most); or simplify the implementation. Give path before content in every call.",
                            params.num_predict
                        )));
                    }
                    continue;
                } else if n != "finish"
                    && last_sig.as_ref() == Some(&(name.clone(), sig_json.clone()))
                    && dup_warns < 3
                {
                    // Exactly the same call as last time: the result will not change.
                    dup_warns += 1;
                    let res = format!("[blocked: you just made this exact {name} call (identical arguments); the result will not change. Change strategy: read_file to see the real state, or try a different approach.]");
                    log!("[{i}] ⚠️ duplicate call blocked: {name} ({dup_warns})");
                    sink(AgentEvent::ToolResult {
                        name: name.clone(),
                        args: args_v.clone(),
                        output: res.clone(),
                        blocked: true,
                    });
                    messages.push(Message::tool_output(res));
                    messages.push(Message::user(
                        "⚠️ The last call was judged a duplicate (identical to an earlier one); the result will not change. Do not retry it unchanged: check the current state with read_file/list_dir first, or move forward with a different approach.",
                    ));
                    continue;
                } else if qa && MUTATE.contains(&n) && casual_warns < 3 {
                    // Q&A guard: a question never needs files written or commands run.
                    casual_warns += 1;
                    if casual_warns >= 2 {
                        casual_force = true;
                    }
                    let res = format!("[blocked: the user is asking a question; {name} is not needed. Answer the user directly in text.]");
                    log!("[{i}] ⚠️ Q&A guard: blocked {name} (question, not a task)");
                    sink(AgentEvent::ToolResult {
                        name: name.clone(),
                        args: args_v.clone(),
                        output: res.clone(),
                        blocked: true,
                    });
                    messages.push(Message::tool_output(res));
                    messages.push(Message::user(format!(
                        "⚠️ Reminder ({casual_warns}): the user is asking a question or making small talk, not setting a task. Your text answer is already sufficient. CALL finish(summary=your answer) NOW to end this turn. Do not repeat the same text and do not call any tool."
                    )));
                    continue;
                } else {
                    sink(AgentEvent::ToolCall(tc.clone()));
                    res = tools.run_tool(n, &args_v);
                    last_sig = Some((name.clone(), sig_json.clone()));
                    if let Some(reason) = tools.child_terminated() {
                        // ≥1 severe refusal (system-area write, irreversible command): end
                        // the child now; the dispatcher sees exit 53 and does not retry.
                        log!(
                            "[child-mode] serious violation, circuit broken (exit 53): {}",
                            text::head(&reason, 160)
                        );
                        save!();
                        return Err(Error::ChildTerminated(reason));
                    }
                }

                if n == "enable_tools" {
                    if let Some(list) = args.get("tools").and_then(Value::as_array) {
                        for t in list.iter().filter_map(Value::as_str) {
                            face.enable(t);
                        }
                    }
                }
                let mcp_producing = is_mcp_tool(n) && face::is_mcp_producing(n);
                if matches!(n, "create_file" | "edit_file" | "append_file" | "run_bash")
                    || mcp_producing
                {
                    productive_used = true;
                }
                if PRODUCTIVE.contains(&n) || mcp_producing {
                    todo_stale += 1;
                } else if n == "todo" {
                    todo_stale = 0;
                }
                log!(
                    "[{i}|+{}s] ⚙ {name} {} -> {}",
                    t0.elapsed().as_secs(),
                    text::head(&sig_json, 60),
                    text::head(&res, 90)
                );
                sink(AgentEvent::ToolResult {
                    name: name.clone(),
                    args: args_v.clone(),
                    output: res.clone(),
                    blocked: false,
                });

                if n == "finish" {
                    let summary = args
                        .get("summary")
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            Value::Null => String::new(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    // Gate 1: a delivery must exist — file/command, body text, or a
                    // substantive summary. Q&A is exempt (the answer is the chat).
                    if !fg_off
                        && !qa
                        && !productive_used
                        && guards::semantically_empty(&content)
                        && guards::semantically_empty(&summary)
                        && fake_finish_warns < 2
                    {
                        fake_finish_warns += 1;
                        log!("[{i}] ⚠️ rejected a hollow finish: no producing tool used (create_file/edit_file/run_bash)");
                        messages.push(Message::user(format!(
                            "⚠️ Your finish was rejected: you have not done any real work yet (no file created or changed, no command run). Pick one of three next steps:\n1. If the final answer or deliverable is settled: write it to a file with create_file first (when the task asks for answer.txt that is: create_file(path=\"answer.txt\", content=\"<your final answer>\")), then finish again.\n2. If the work is not done: keep going with tools (read/write files, run commands, search) and call finish when it is complete.\n3. If this is an analysis or explanation task that should not produce files: give the conclusion to the user as body text (not inside summary), then finish again.{}",
                            ws_note(workdir)
                        )));
                        messages.push(Message::tool_output(
                            "[finish rejected: no deliverable detected. Take one of the three actions above, then finish.]",
                        ));
                        continue;
                    }
                    // Gate 2: tests in the workdir must pass (3 refusals, then let through).
                    if !qa
                        && !fg_off
                        && testguard::has_python_tests(workdir)
                        && test_guard_warns < 3
                    {
                        let (verdict, thint) = testguard::run_test_guard(workdir);
                        match verdict {
                            TestVerdict::Skip => {
                                log!("[{i}] test guard skipped: no pytest/python in this environment, tests cannot be verified automatically");
                            }
                            TestVerdict::Fail => {
                                test_guard_warns += 1;
                                let h = if params.ablated("verify_feedback") {
                                    "last line of pytest output".to_string()
                                } else {
                                    thint
                                };
                                let mut bak_hint = String::new();
                                if h.contains("SyntaxError") || h.contains("IndentationError") {
                                    if let Some(b) = first_py_bak(workdir) {
                                        bak_hint = format!("Syntax error detected. A backup is available to roll back: restore the matching .bak (e.g. copy {b} over the original), or rebuild the file.\n");
                                    }
                                }
                                log!("[{i}] ⚠️ tests failing, finish rejected ({test_guard_warns}/3; let through after 3)");
                                messages.push(Message::user(format!(
                                    "⚠️ Your finish was rejected: the tests in the directory are failing.\nExact failure: {h}\n{bak_hint}Use read_file to look at the failing code and make the tests and implementation agree. Run them with whatever the environment has: python -m pytest -q; without pytest use python -m unittest or python test_xxx.py directly. Finish once everything is green (rejection {test_guard_warns}/3; after 3 rejections finish will be let through)."
                                )));
                                messages.push(Message::tool_output(
                                    "[finish rejected: tests not all green. Fix the implementation and tests, then finish.]",
                                ));
                                continue;
                            }
                            TestVerdict::Pass => {}
                        }
                    }
                    // Gate 3: files the summary claims must exist.
                    if !qa && !fg_off && finish_claim_warns < 2 {
                        let missing = artifacts::claimed_missing_files(&summary, workdir);
                        if !missing.is_empty() {
                            finish_claim_warns += 1;
                            log!("[{i}] ⚠️ artefact check: summary claims {missing:?} but they are missing, finish rejected");
                            messages.push(Message::user(format!(
                                "⚠️ Your finish was rejected: these artefacts named in your summary do not exist in the working directory:\n{}\nActually produce them with create_file/run_bash, or change the summary to name only files that really exist, then finish again.{}",
                                bullet(&missing),
                                ws_note(workdir)
                            )));
                            messages.push(Message::tool_output(
                                "[finish rejected: files named in the summary are missing. Produce them or correct the summary, then finish.]",
                            ));
                            continue;
                        }
                    }
                    // Gate 4: files the plan names must exist (existence, not checkmarks).
                    // Children are exempt from gates 4–6: one mechanical task, no plan to
                    // maintain; their exit contract (artefacts + finish) is checked by the
                    // dispatcher, and a blocked child only burns its budget into a timeout.
                    if !qa && !child && !fg_off && plan_gate_warns < 2 {
                        let p_missing = artifacts::plan_named_missing(workdir);
                        if !p_missing.is_empty() {
                            plan_gate_warns += 1;
                            log!("[{i}] ⚠️ plan check: plan names {p_missing:?} but they are missing, finish rejected");
                            messages.push(Message::user(format!(
                                "⚠️ Your finish was rejected: your plan (todo) names these output files, but they do not exist in the working directory:\n{}\nTwo options: (1) actually produce them with create_file; (2) if the plan no longer matches reality, rebuild it with todo(action=create, items=[a new plan that matches what you did]) and finish again.{}",
                                bullet(&p_missing),
                                ws_note(workdir)
                            )));
                            messages.push(Message::tool_output(
                                "[finish rejected: artefacts named in the plan are missing. Produce them or rebuild the plan, then finish.]",
                            ));
                            continue;
                        }
                    }
                    // Gate 5: the plan must be synced before the end (2 refusals max).
                    if !qa
                        && !child
                        && !params.ablated("finish_gate_text_only")
                        && todo_gate_warns < 2
                    {
                        let (t_done, t_all) = todo_progress(&load_todo(workdir));
                        if t_all > 0 && t_done < t_all {
                            todo_gate_warns += 1;
                            log!("[{i}] ⚠️ plan out of sync ({t_done}/{t_all}), finish rejected");
                            messages.push(Message::user(format!(
                                "⚠️ Your finish was rejected: the plan (todo) still has {} item(s) not marked done ({t_done}/{t_all}).\nCurrent plan:\n{}\nTick off the completed steps first: todo(action=update, index=step number); if everything is done, sync in one go: todo(action=update, all=true). If steps remain undone, carry on and finish afterwards. When you finish again, summarise the outcome for the user in two or three sentences in summary (what was done, what the artefacts are).",
                                t_all - t_done,
                                load_todo_str(workdir)
                            )));
                            messages.push(Message::tool_output(
                                "[finish deferred: sync the plan with actual progress (todo update) first, then finish.]",
                            ));
                            continue;
                        }
                    }
                    // Gate 6: once, re-inject the original task for a self-check.
                    if !qa && !child && !finish_reread_used {
                        finish_reread_used = true;
                        let orig = qa::task_original_text(&messages, 2000);
                        if !orig.is_empty() {
                            log!(
                                "[{i}] ⚠️ pre-delivery check: re-injected the original task ({} chars), asked for a self-check before finishing again",
                                text::clen(&orig)
                            );
                            messages.push(Message::user(format!(
                                "⚠️ Pre-delivery check: before finishing, go through the ORIGINAL TASK INSTRUCTIONS below point by point and confirm your final deliverable meets every one of them, especially the format of answers/artefacts (units, decimal places, rounding), naming, value ranges, and every sub-item the task names explicitly. Details of the brief are easily forgotten by the end of a long task.\nIf anything is not met: fix the deliverable first, then finish again. If everything is met: finish again and summarise the outcome for the user in two or three sentences in summary (what was done, what the artefact/answer is).\n--- original task instructions ---\n{orig}\n--- end of original task instructions ---"
                            )));
                            messages.push(Message::tool_output(
                                "[finish pending self-check: verify against the original task instructions above, then finish again.]",
                            ));
                            continue;
                        }
                    }
                    // Closing visibility: a bare finish (no body, no summary) gets one
                    // tool-less call so the user sees a wrap-up.
                    if guards::semantically_empty(&content)
                        && summary.trim().is_empty()
                        && !qa
                        && !fg_off
                    {
                        let mut wm = messages.clone();
                        wm.push(Message::tool_output(res.clone()));
                        wm.push(Message::user(
                            "The task is wrapped up. Summarise for the user in plain text (do not call any tool): what was done and what the key artefacts or conclusions are. Five sentences at most.",
                        ));
                        if let Ok(wr) = backend.chat(model, &wm, &[], &mut |p| {
                            if let StreamPiece::Token(t) = p {
                                sink(AgentEvent::Text(t.to_string()));
                            }
                        }) {
                            let wt = wr.message.content.trim().to_string();
                            if !wt.is_empty() {
                                messages.push(Message::assistant(wt));
                            }
                        }
                    }
                    log!("\n===== TASK COMPLETE =====");
                    log!("{res}");
                    sink(AgentEvent::Finished {
                        summary: if summary.trim().is_empty() {
                            content.clone()
                        } else {
                            summary.clone()
                        },
                    });
                    save!();
                    return Ok(messages);
                }

                // Failure ladder: same tool failing repeatedly → change strategy / disable.
                if is_tool_error(&res) {
                    if small_model
                        && n == "run_bash"
                        && res.to_lowercase().contains("not recognized")
                        && bash_teach_warns < 2
                    {
                        bash_teach_warns += 1;
                        messages.push(Message::user(
                            "Hint: run_bash runs Windows shell (cmd) commands and cannot execute Python code directly. Do it this way: write the code to a .py file with create_file, then run it with run_bash: python filename.py",
                        ));
                    }
                    if let Some(key) = missing_arg_key(&res).map(str::to_string) {
                        // Missing-argument ladder: gemma-class models drop `path` about one
                        // call in eight and then copy their own failed call from the history
                        // (WF-08: 37 path-less create_file calls in a row). From the second
                        // consecutive miss on, remove the earlier failed pairs and prescribe
                        // a different call shape instead of "retry verbatim".
                        let st = missing_arg_streak.entry(name.clone()).or_insert(0);
                        *st += 1;
                        let st = *st;
                        if st >= 2 && missing_arg_warns < 4 {
                            missing_arg_warns += 1;
                            let pruned = prune_missing_arg_calls(&mut messages, n, &key);
                            if pruned > 0 {
                                save!();
                            }
                            let planned = artifacts::plan_named_missing(workdir);
                            let plan_hint = if planned.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "Files in the plan not yet produced: {}.",
                                    planned.join(", ")
                                )
                            };
                            log!(
                                "[{i}] ⚠️ {name} missing argument '{key}' {st} times in a row; removed {pruned} failed call group(s), prescribing the chunked shape"
                            );
                            after_output = Some(format!(
                                "{MISSING_ARG_NOTE_MARK}You have called {name} {st} times in a row without the argument '{key}' (the failed calls have been removed from the conversation; do not copy them). Do not resend the same content in one piece. Use a different call shape: first {name}({key}=\"<filename>\", content=\"<the first few lines>\") to create the file, then append_file(path=\"<filename>\", content=\"<the rest>\") piece by piece. Every call must carry both '{key}' and content, with {key} first.{plan_hint}"
                            ));
                        }
                    } else {
                        let fc = fail_count.entry(name.clone()).or_insert(0);
                        *fc += 1;
                        if *fc >= 3 {
                            if n == "edit_file" {
                                face.disable("edit_file");
                                messages.push(Message::user(
                                "⚠️ edit_file has failed 3 times in a row and is now disabled. From now on change files by writing the complete content with create_file.",
                            ));
                                *fc = 0;
                            } else if redirect_warns < 4 {
                                redirect_warns += 1;
                                let path_refused = res.contains("outside the working directory")
                                    || res.contains("out of bounds");
                                let hint = if path_refused {
                                    "⚠️ The rejected path is outside the working directory. All of the task's input files are already in your working directory: run list_dir(path='.') now, read the files it lists, and do not touch any path outside the working directory again."
                                } else if n == "run_bash" {
                                    "⚠️ Do not cd to absolute paths like /workspace. The working directory is already set: run commands directly or use relative paths; check the directory with list_dir and the real state with read_file/pwd first."
                                } else {
                                    "⚠️ Check the real state with list_dir / read_file first and rebuild the arguments, or try a different approach."
                                };
                                let count = *fc;
                                messages.push(Message::user(format!(
                                "⚠️ Your last {count} {name} calls all failed. Stop retrying now and change strategy: {hint}"
                            )));
                                *fc = 0;
                            }
                        }
                    }
                } else {
                    // Only a success of the same tool resets its ladders.
                    if fail_count.get(n).is_some_and(|c| *c > 0) {
                        fail_count.insert(name.clone(), 0);
                    }
                    missing_arg_streak.insert(name.clone(), 0);
                }

                // Repeat-success streaks.
                let sig_now = (name.clone(), sig_json.clone());
                if last_sig_now.as_ref() == Some(&sig_now) {
                    same_sig_streak += 1;
                } else {
                    same_sig_streak = 1;
                }
                last_sig_now = Some(sig_now);
                if last_tool.as_deref() == Some(n) {
                    tool_streak += 1;
                } else {
                    last_tool = Some(name.clone());
                    tool_streak = 1;
                }
                if tool_streak >= 5 && streak_warns < 3 {
                    streak_warns += 1;
                    messages.push(Message::user(format!(
                        "⚠️ You have called {name} {tool_streak} times in a row. If the results are similar or nothing new is coming out, stop repeating: combine what you have and move to the next step (write a file / use another tool / call finish). If you are doing a batch of steps with different arguments each time, that is fine; carry on."
                    )));
                    tool_streak = 1;
                }
                if !params.ablated("anti_loop")
                    && tool_streak >= 8
                    && same_sig_streak >= 4
                    && n != "finish"
                {
                    // Only a true loop: same tool 8×, same arguments 4× in a row.
                    face.disable(n);
                    messages.push(Message::user(format!(
                        "⚠️ {name} has been called {tool_streak} times in a row and is now disabled. Stop repeating it and get on with the real work (write files / run / call finish)."
                    )));
                    tool_streak = 1;
                }
                // Research streak, fingerprint level: a new query is progress.
                if RESEARCH_ALL.contains(&n) {
                    let fp = text::head(&sig_json, 256).to_string();
                    if research_seen.contains(&fp) {
                        research_streak += 1;
                    } else {
                        research_seen.insert(fp);
                        research_streak = 0;
                    }
                    if research_streak >= 6 && research_warns < 3 {
                        research_warns += 1;
                        for t in RESEARCH_ALL {
                            face.disable(t);
                        }
                        messages.push(Message::user(format!(
                            "⚠️ You have repeated the exact same web query {research_streak} times without producing any file. The web tools are now disabled: take a genuinely different angle, or write what you have found to a file with create_file and then call finish. To search again, rephrase the query and re-enable the tools with enable_tools."
                        )));
                        research_streak = 0;
                        research_seen.clear();
                    }
                } else if matches!(
                    n,
                    "create_file" | "edit_file" | "append_file" | "finish" | "run_bash"
                ) {
                    research_streak = 0;
                }
                // Plan-tool loop: same (action,index) 6× in a row, or 15 todo calls without output.
                if n == "todo" {
                    todo_streak += 1;
                    if same_sig_streak >= 6 || todo_streak >= 15 {
                        face.disable("todo");
                        messages.push(Message::user(format!(
                            "⚠️ todo is now disabled (the same call {same_sig_streak} times in a row / {todo_streak} calls with no real work). Switch to producing tools now: create_file for output files, run_bash to verify; if you need to record a plan, write it into an output file."
                        )));
                        todo_streak = 0;
                    }
                } else if PRODUCTIVE.contains(&n) {
                    todo_streak = 0;
                }
                // Shotgun-edit guard counts only landed edits.
                if n == "edit_file" && res.starts_with("[edited") {
                    let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                    if let Some(hint) = edit_guard.observe(path) {
                        log!("[{i}] ⚠️ shotgun-edit hint: {path}");
                        messages.push(Message::user(hint));
                    }
                }
                // Time budget pacing (task mode only).
                if budget_total > 0.0 && !qa {
                    let el = prev_elapsed + t0.elapsed().as_secs_f64();
                    budget::save_elapsed(workdir, el);
                    if let Some(bn) = budget::budget_nudge(budget_total, el, &mut budget_fired) {
                        log!("[{i}] ⏱ budget nudge injected ({:.0}min used)", el / 60.0);
                        messages.push(Message::user(bn));
                    }
                }
                messages.push(Message::tool_output(res));
                if let Some(note) = after_output.take() {
                    messages.push(Message::user(note));
                }
                // Stale-plan reminder (never a block, twice at most).
                if !qa && todo_nudge_warns < 2 && todo_stale >= TODO_STALE_NUDGE {
                    let (t_done, t_all) = todo_progress(&load_todo(workdir));
                    if t_all > 0 && t_done < t_all {
                        let stale = todo_stale;
                        todo_nudge_warns += 1;
                        todo_stale = 0;
                        messages.push(Message::user(format!(
                            "⚠️ Plan progress reminder: you have done several steps of real work but the plan is still at {t_done}/{t_all}. Sync after every step: todo(action=update, index=completed step number).\nCurrent plan:\n{}",
                            load_todo_str(workdir)
                        )));
                        log!("[{i}] 📌 stale-plan reminder ({t_done}/{t_all}, {stale} steps without an update)");
                    }
                }
            }
        }
        if exhausted {
            log!("===== MAX ITERATIONS =====");
            sink(AgentEvent::MaxIterations);
        }
        save!();
        Ok(messages)
    }
}

fn bullet(items: &[String]) -> String {
    items
        .iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_py_bak(workdir: &Path) -> Option<String> {
    let mut baks: Vec<String> = std::fs::read_dir(workdir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".py.bak"))
        .collect();
    baks.sort();
    baks.into_iter().next()
}

/// A retry/back-off sleep scaled by `RunParams::sleep_scale` (0 in tests).
fn nap(secs: u64, scale: f32) {
    let d = Duration::from_secs_f64(secs as f64 * f64::from(scale.max(0.0)));
    if !d.is_zero() {
        std::thread::sleep(d);
    }
}

#[allow(clippy::too_many_arguments)]
fn compact<B: ChatBackend>(
    backend: &mut B,
    model: &str,
    messages: Vec<Message>,
    level: u8,
    todo: Option<String>,
    files: Option<String>,
    transcript: &mut Transcript,
    sink: &mut dyn FnMut(AgentEvent),
) -> Vec<Message> {
    let before = messages.len();
    let c = compact_history(
        backend,
        model,
        messages,
        level,
        todo.as_deref(),
        files.as_deref(),
        &mut |s| sink(AgentEvent::Log(s)),
    );
    if c.summarised {
        transcript.mark_compacted();
    }
    if c.messages.len() != before {
        sink(AgentEvent::Compacted {
            level,
            before,
            after: c.messages.len(),
        });
    }
    c.messages
}
