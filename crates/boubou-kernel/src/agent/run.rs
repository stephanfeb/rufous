//! `agent_loop`: the harness around the model. Every guard here exists because a real
//! model session failed in a specific way; the message strings are pinned by the ported
//! tests, so change them together with the tests.
//!
//! Shape: up to `max_iterations` turns. Each turn: flush transcript → dedupe tail →
//! assemble the tool face → pre-call token estimate (compaction ladder) → model call
//! (with the 4xx/5xx triage) → post-call context ladder → either run the tool calls through
//! the guard stack, or handle a text-only turn (empty-turn ladder, repeat detection,
//! Q&A wrap-up). `finish` runs the gate sequence and returns.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::ablation;
use super::chat::DynBackend;
use super::chat::{ChatBackend, StreamPiece};
use super::corrector::{self, Verdict};
use super::face::{self, ToolFace, PRODUCTIVE, RESEARCH_ALL};
use super::history::{
    attach_pending_images, compact_history, dedupe_trailing_assistant, drop_images,
    estimate_messages_tokens, prune_history, workdir_files_text, PRUNE_AT,
};
use super::ladders::{
    CallLadders, ChatFailure, Compaction, CorrectorLedger, EmptyVerdict, Failure, Recall, Retry,
    RetryLadder, Streak, Talk, TalkVerdict, TextLadder,
};
use super::openai::OpenAiBackend;
use super::policy::{Gate, Prompt, Route};
use super::prompt;
use super::session::{self, Transcript};
use super::testguard::{self, FinishCheck, RerunOutcome};
use super::{AgentEvent, RunParams};
use crate::decider::{DeciderMode, LayeredDecider};
use crate::exec::Executor;
use crate::guards::budget::{self, BudgetMark};
use crate::guards::edit::EditConvergenceGuard;
use crate::guards::idle;
use crate::guards::qa::Intent;
use crate::guards::qa::DISPATCH_NOTE_MARK;
use crate::guards::{self, artifacts, integrity, leak, qa};
use crate::ollama::{Message, Role, ToolCall};
use crate::ollama_proc::OllamaProc;
use crate::parallel::{env_snapshot, DispatchHook, HookEvent, ParallelConfig};
use crate::skills::{Skill, SkillSet};
use crate::todo::{format_todo_lines, load_todo, load_todo_str, todo_progress};
use crate::tools::{is_mcp_tool, is_well_formed_tool_name, required_args, resolve_alias};
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
    /// The profile's workdir-prefix rule ([`Executor::rebase_workdir_prefix`]), set once per run.
    fn set_rebase_workdir_prefix(&mut self, _on: bool) {}
    /// `edit_file`'s matcher ([`crate::edit_match`]), off under the `edit_match` ablation.
    fn set_edit_match(&mut self, _on: bool) {}
    /// The agent's stop flag: raised, a running command is terminated and the tool returns.
    fn set_stop(&mut self, _flag: Arc<AtomicBool>) {}
    /// The skills compiled into the product, for the `skills` tool's listing and loading.
    fn set_builtin_skills(&mut self, _skills: Vec<Skill>) {}
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
    fn set_rebase_workdir_prefix(&mut self, on: bool) {
        self.rebase_workdir_prefix = on;
    }
    fn set_edit_match(&mut self, on: bool) {
        self.edit_match = on;
    }
    fn set_stop(&mut self, flag: Arc<AtomicBool>) {
        self.stop = Some(flag);
    }
    fn set_builtin_skills(&mut self, skills: Vec<Skill>) {
        self.builtin_skills = skills;
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

/// A tool name as shown in refusal texts and log lines: whitespace collapsed, cut at 60
/// characters, so a leaked template fragment used as a name cannot sprawl over the note.
fn compact_tool_name(name: &str) -> String {
    let flat = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut it = flat.chars();
    let head: String = it.by_ref().take(60).collect();
    if it.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// `_ws_note`: the refusal texts always name the real absolute workdir, because small
/// models otherwise guess `/workspace` or the home directory and never find out.
pub fn ws_note(workdir: &Path) -> String {
    let abs = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    format!(
        "Your working directory (absolute path) is {}. Any file you write goes inside it; prefer relative paths (e.g. create_file(path=\"options.md\")).",
        abs.display()
    )
}

/// `_plan_note`: current plan as text for compaction re-injection; none without a plan
/// directory (the question route) or with an empty plan.
fn plan_note(plan_dir: Option<&Path>) -> Option<String> {
    let t = load_todo(plan_dir?);
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

/// The task system prompt for `task`: the skills its text triggers (capped by the
/// profile), the workdir's `SYSTEM.md`, the unattended line and the profile note. Shared
/// by [`Agent::task_messages`] (a fresh run) and the per-turn rebuild in [`Agent::run`]
/// (a continued session), so the prompt is a function of this turn's route, never of what
/// the previous turn happened to leave in the history.
fn task_system(
    base: &'static str,
    params: &RunParams,
    skill_dirs: &[PathBuf],
    builtin: &[Skill],
    sink: &mut Sink,
    task: &str,
) -> String {
    let set = SkillSet::discover(skill_dirs).with_builtin(builtin.iter().cloned());
    let mut skills = Vec::new();
    for s in set.for_task_capped(task, params.profile.max_prefill_skills) {
        match s.load() {
            Ok(body) => skills.push((s.name.clone(), body)),
            Err(e) => sink(AgentEvent::Log(format!(
                "[skill {} unreadable: {e}]",
                s.name
            ))),
        }
    }
    if !skills.is_empty() {
        let names: Vec<&str> = skills.iter().map(|(n, _)| n.as_str()).collect();
        sink(AgentEvent::Log(format!(
            "[skills in effect: {}]",
            names.join(", ")
        )));
    }
    let opts = prompt::PromptOptions {
        base: Some(base),
        workdir: Some(&params.workdir),
        system_file: params.system_file.as_deref(),
        unattended: params.unattended,
        skills,
        profile_note: params.profile.prompt_note.as_deref(),
    };
    prompt::system_prompt(&opts)
}

/// The system prompt the turn's [`Prompt`] names. `task` is the text skills are matched on
/// (the task prompt only).
fn system_for(
    which: Prompt,
    params: &RunParams,
    skill_dirs: &[PathBuf],
    builtin: &[Skill],
    sink: &mut Sink,
    task: &str,
) -> String {
    match which {
        Prompt::Task(base) => task_system(base, params, skill_dirs, builtin, sink, task),
        Prompt::Chat => prompt::chat_system(Some(&params.workdir)),
        Prompt::Child => prompt::child_system(Some(&params.workdir)),
    }
}

/// The loop plus everything it owns between runs: tool face, transcript cursor, backend.
pub struct Agent<B, T> {
    pub params: RunParams,
    pub backend: B,
    pub tools: T,
    pub face: ToolFace,
    pub transcript: Transcript,
    /// `~/.ollama_agent/sessions` unless relocated.
    pub sessions_dir: PathBuf,
    /// Where the live checkpoint (and its elapsed-time sidecar) is written. `None` is the
    /// CLI's `<workdir>/.agent_state.json`, which `boubou` resumes from after a kill.
    /// An embedding whose workdir is the user's own project (ACP) sets
    /// [`session::state_path`] so the model never reads the harness's history out of a
    /// directory listing and the project's git status stays clean (boubou-ll7).
    pub checkpoint: Option<PathBuf>,
    sink: Sink,
    /// User stop (web UI "stop" button): ends the run at the next check, saving the session.
    stop: Arc<AtomicBool>,
    /// `_maybe_parallel_dispatch`: set by [`Agent::standard`] (or `with_parallel`); `None`
    /// means the hook is absent (tests, embedding).
    pub parallel: Option<DispatchHook>,
    /// Human-readable engine label (`Ollama · host`, `cloud · model`, `OpenAI-compatible · url`).
    pub engine: String,
    /// Where skills are discovered for prefill, in precedence order (`<workdir>/skills`,
    /// `<agent_home>/skills`, `<install_dir>/skills`); tests narrow it.
    pub skill_dirs: Vec<PathBuf>,
    /// The skills compiled into the product ([`Agent::with_builtin_skills`]), below the
    /// directories; the same list goes to the tool runner for the `skills` tool.
    pub builtin_skills: Vec<Skill>,
    /// System One decider (`decider.rs`): heuristics only unless `config.json` says otherwise.
    pub decider: LayeredDecider,
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
    pub fn standard(mut params: RunParams, cfg: &crate::config::Config) -> Result<Self> {
        // The model's profile fills the think/temperature defaults before the backend is
        // built (the loop logs the resolved profile at run start), and the config's
        // `slim_output` mode is applied under the environment's override.
        params.apply_config(cfg);
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
        if let Some(h) = &params.harness_dir {
            tools.harness_dir = h.clone();
        }
        let pcfg = ParallelConfig::from_config(cfg).with_env(&env_snapshot());
        let child_mode = params.child_mode;
        let mut agent = Agent::new(params, backend, tools);
        agent.engine = engine;
        agent.face.offline = offline;
        if child_mode {
            let allowed = agent.tools.install_child_mode(&pcfg);
            agent.face.narrow_to(&allowed);
        }
        agent.decider = LayeredDecider::from_config(&cfg.decider, &host);
        agent.parallel = Some(
            DispatchHook::new(pcfg, host)
                .with_child_mode(child_mode)
                .with_abort(agent.stop.clone()),
        );
        Ok(agent)
    }
}

impl<B: ChatBackend, T: ToolRunner> Agent<B, T> {
    pub fn new(params: RunParams, mut backend: B, mut tools: T) -> Self {
        let home = crate::config::agent_home();
        // One flag for everything a stop must reach: the loop, the in-flight stream, a
        // running command and the dispatched children (`with_stop` swaps it for the
        // caller's).
        let stop = Arc::new(AtomicBool::new(false));
        backend.set_cancel(stop.clone());
        tools.set_stop(stop.clone());
        let skill_dirs = vec![
            params.workdir.join("skills"),
            home.join("skills"),
            crate::skills::install_dir().join("skills"),
        ];
        let mut face = ToolFace::new();
        for t in &params.profile.hide_tools {
            face.disable(t);
        }
        Self {
            params,
            backend,
            tools,
            face,
            transcript: Transcript::new(),
            sessions_dir: session::sessions_dir(&home),
            checkpoint: None,
            sink: Box::new(|_| {}),
            stop,
            parallel: None,
            engine: String::new(),
            skill_dirs,
            builtin_skills: Vec::new(),
            decider: LayeredDecider::off(),
        }
    }

    /// The skills built into the product (each product crate's `harness::skills()`): listed
    /// and loadable by the `skills` tool and matched for prefill like a directory's, below
    /// every directory so a user's file of the same name wins.
    pub fn with_builtin_skills(mut self, skills: Vec<Skill>) -> Self {
        self.tools.set_builtin_skills(skills.clone());
        self.builtin_skills = skills;
        self
    }

    /// Replace the decider (tests; embedding with a scripted decision model).
    pub fn with_decider(mut self, decider: LayeredDecider) -> Self {
        self.decider = decider;
        self
    }

    /// Restrict skill discovery to `dirs` (tests; embedding without a home directory).
    pub fn with_skill_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.skill_dirs = dirs;
        self
    }

    /// Attach the parallel-dispatch hook (tests inject a probe and spawner through it).
    pub fn with_parallel(mut self, hook: DispatchHook) -> Self {
        self.parallel = Some(hook.with_abort(self.stop.clone()));
        self
    }

    /// Share a stop flag with the caller: the backend abandons an in-flight stream when it
    /// is set, a running command and the dispatched children are terminated, and the loop
    /// ends before the next turn, saving the session.
    pub fn with_stop(mut self, flag: Arc<AtomicBool>) -> Self {
        self.backend.set_cancel(flag.clone());
        self.tools.set_stop(flag.clone());
        if let Some(h) = self.parallel.as_mut() {
            h.abort = flag.clone();
        }
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

    /// Build the opening message list for a fresh task. Skills whose triggers match the
    /// task (or that autoload) and the workdir's `SYSTEM.md` enter the system prompt here.
    /// A continued session rebuilds the prompt per turn in [`Agent::run`] from the route.
    pub fn task_messages(&mut self, task: &str) -> Vec<Message> {
        // The opening turn of a fresh run is a task; the policy says which prompt frames it
        // (a dispatched child gets its sandbox prompt).
        let which = self.params.turn_policy().turn(Intent::Task, false).prompt;
        let sys = system_for(
            which,
            &self.params,
            &self.skill_dirs,
            &self.builtin_skills,
            &mut self.sink,
            task,
        );
        vec![Message::system(sys), Message::user(task)]
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
            checkpoint,
            sink,
            stop,
            parallel,
            engine: _,
            skill_dirs,
            builtin_skills,
            decider,
        } = self;
        let model = params.model.as_str();
        let workdir = params.workdir.as_path();
        // The harness's own files (plan, backups, trash, image queue): the workdir unless
        // relocated out of the user's project (`RunParams::harness_dir`).
        let hdir: &Path = params.harness_dir.as_deref().unwrap_or(workdir);
        let bak_dir: PathBuf = if hdir == workdir {
            workdir.to_path_buf()
        } else {
            hdir.join(crate::exec::BACKUP_DIR)
        };
        let ckpt_path = checkpoint
            .clone()
            .unwrap_or_else(|| session::checkpoint_path(workdir));
        let elapsed_path = budget::elapsed_meta_for(&ckpt_path);
        let ctx_budget = f64::from(params.num_ctx);
        let policy = params.turn_policy();
        // The policy the session last ran under (`<name>.meta.json`): a session resumed on
        // another surface says it changed harness rather than doing so silently.
        let previous_policy = session.and_then(|s| session::recorded_policy(sessions_dir, s));
        let gates_ablated = params.ablated(ablation::FINISH_GATE)
            || params.ablated(ablation::FINISH_GATE_TEXT_ONLY);

        macro_rules! log {
            ($($arg:tt)*) => { sink(AgentEvent::Log(format!($($arg)*))) };
        }
        {
            let prof = &params.profile;
            let effective = format!(
                "think={} temp={}",
                match params.think {
                    Some(true) => "on",
                    Some(false) => "off",
                    None => "engine default",
                },
                params
                    .temperature
                    .map_or_else(|| "engine default".to_string(), |t| t.to_string())
            );
            if prof.note.is_empty() {
                log!(
                    "[profile] {model} → {}: {} ({effective})",
                    prof.name,
                    prof.describe()
                );
            } else {
                log!(
                    "[profile] {model} → {}: {} ({effective}). Evidence: {}",
                    prof.name,
                    prof.describe(),
                    prof.note
                );
            }
            if !params.ablation.is_empty() {
                let (off, unknown) = ablation::describe(&params.ablation);
                log!(
                    "[ablation] off: {}{}",
                    if off.is_empty() {
                        "nothing".to_string()
                    } else {
                        off.join(", ")
                    },
                    if unknown.is_empty() {
                        String::new()
                    } else {
                        format!(" (not a switch, ignored: {})", unknown.join(", "))
                    }
                );
            }
        }
        macro_rules! save {
            () => {
                session::save_session(
                    sessions_dir,
                    session,
                    &messages,
                    Some(workdir),
                    Some(hdir),
                    policy.name,
                    transcript,
                );
                if session.is_some() {
                    log!("[session saved: {}]", session.unwrap_or_default());
                }
            };
        }

        // Per-run ladders (agent/ladders.rs) and counters.
        let mut retry = RetryLadder::default();
        let mut compaction = Compaction::default();
        let mut calls = CallLadders::default();
        let mut productive_used = false;
        let mut fake_finish_warns = 0;
        let mut finish_claim_warns = 0;
        // Integrity gate (Boubou, 2026-10-28): what ran this turn; claim refusals (2, like the
        // artefact gate) and the deliverable refusal (1).
        let mut cmd_records: Vec<integrity::CommandRecord> = Vec::new();
        let mut claim_gate_warns = 0;
        let mut ledger = CorrectorLedger::default();
        let mut named_gate_warns = 0;
        let mut finish_reread_used = false;
        let mut plan_gate_warns = 0;
        let mut text_ladder = TextLadder::default();
        let mut truncated_call_warns = 0;
        let mut test_guard_warns = 0;
        let mut casual_warns = 0;
        let mut casual_force = false;
        let mut todo_stale = 0;
        let mut todo_nudge_warns = 0;
        let mut todo_gate_warns = 0;
        const TODO_STALE_NUDGE: usize = 4;
        let mut idle_notes: u32 = 0;
        let mut qa_cap_blocks: u32 = 0;

        if params.child_mode {
            log!(
                "[child-mode] sandbox installed: tools={:?} workdir={}",
                face.active,
                workdir.display()
            );
        }
        if let Some(prev) = previous_policy
            .as_deref()
            .filter(|prev| *prev != policy.name)
        {
            log!(
                "[policy] session {} ran under {prev}; this turn runs under {}",
                session.unwrap_or_default(),
                policy.name
            );
        }
        if !policy.is_unattended() {
            log!(
                "[policy] {} (context {} tokens)",
                policy.describe(),
                params.num_ctx
            );
        }
        // Q&A or task? A session with real progress treats short follow-ups as task steps.
        // The decider is consulted when the policy says so; its verdict routes the turn only
        // under a policy with intent routing, and is logged and recorded either way.
        let intent = if policy.consult_decider {
            let d = decider.intent_of(&messages);
            log!("[decider] {}", d.describe());
            // Shadow and on modes leave a labelling trace per session (boubou-acy.3): the log
            // line alone reached no file on any surface. A run without a session name shares
            // one sidecar.
            if decider.mode() != DeciderMode::Off {
                let name = session.unwrap_or("_unnamed");
                session::append_decision(sessions_dir, name, &d.record(decider.mode(), name));
            }
            d.intent
        } else {
            Intent::Task
        };
        // What this turn does around the model, derived once from the policy, the intent
        // and the ablations; read by name from here on.
        let turn = policy.turn(intent, gates_ablated);
        let plan_dir_for_notes: Option<&Path> = turn.working.then_some(hdir);
        let mut acted_this_turn = false;
        let mut follow_up_nudged = false;
        // The system prompt follows the turn (`Turn::prompt`), rebuilt when the policy says
        // so (`Prompt::rebuild`): a continued session (the web UI appends the new user turn
        // to the saved history) would otherwise keep whatever the previous turn left.
        let continued = messages.iter().any(|m| m.role == Role::Assistant);
        if turn.prompt.rebuild(continued) {
            // Skills are matched on the opening task and on this turn's input, so a skill
            // prefilled for the original brief stays and a new trigger can add one.
            let skill_text = format!(
                "{}\n{}",
                first_user_text(&messages),
                qa::task_routing_text(&messages)
            );
            let sys = system_for(
                turn.prompt,
                params,
                skill_dirs,
                builtin_skills,
                sink,
                &skill_text,
            );
            let mut m = vec![Message::system(sys)];
            m.extend(messages.into_iter().filter(|m| m.role != Role::System));
            messages = m;
            log!(
                "[prompt] {} prompt rebuilt for this turn",
                turn.prompt.name()
            );
        }
        // The tool face: the chat tool set on the question route, category routing on the
        // task's text otherwise.
        let cats: Option<BTreeSet<String>> = (turn.route != Route::Question)
            .then(|| face::route_categories(&qa::task_routing_text(&messages)));
        // Parallel dispatch hook (harness layer; Q&A, children and editor turns skip it, the
        // hook itself re-checks the child, kill-switch and receipt gates). A fresh task with a
        // plan already on disk dispatches here; a resumed run (tool results in the history)
        // is left alone. The usual case, the model writing its plan on its first turn, is
        // handled below: the hook runs once more after the first successful todo(create).
        let dispatch_wanted = parallel.is_some() && turn.dispatch;
        let mut dispatch_after_plan = dispatch_wanted;
        if dispatch_wanted && !messages.iter().any(|m| m.role == Role::Tool) {
            if let Some(hook) = parallel.as_ref() {
                messages = hook.maybe_dispatch(messages, workdir, hdir, &mut |ev| match ev {
                    HookEvent::Log(l) => sink(AgentEvent::Log(l)),
                    HookEvent::Dispatch(v) => sink(AgentEvent::Dispatch(v)),
                });
                // Dispatched already: the receipt keeps a second run idempotent anyway.
                dispatch_after_plan = !messages
                    .iter()
                    .any(|m| m.content.starts_with(DISPATCH_NOTE_MARK));
            }
        }
        // No tool is withheld by parameter count: Mingbird's `_small_model` hid edit_file
        // from every ≤4B model and measured as a net loss (bead boubou-6uj, changelog
        // 2026-10-31). A model that needs a tool withheld says so in its profile
        // (`hide_tools`); one that fails a tool loses it through the failure ladder.
        // The call-pattern ladders below, all off under one switch.
        let anti_loop = !params.ablated(ablation::ANTI_LOOP);
        let salvage_on = !params.ablated(ablation::SALVAGE);
        tools.set_slim_output(params.slim_output);
        tools.set_rebase_workdir_prefix(
            params.profile.rebase_workdir_prefix && !params.ablated(ablation::PATH_REBASE),
        );
        tools.set_edit_match(!params.ablated(ablation::EDIT_MATCH));
        let mut edit_guard = EditConvergenceGuard::default();

        // Time budget: explicit parameter beats the env fallback; resume continues the clock.
        let budget_total = budget_sec
            .or(params.budget_env_sec)
            .filter(|b| b.is_finite())
            .unwrap_or(0.0)
            .max(0.0);
        let t0 = Instant::now();
        let prev_elapsed = if budget_total > 0.0 {
            budget::load_elapsed_at(&elapsed_path)
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
            let ct: Vec<Value> = match (&cats, turn.route == Route::Question) {
                (_, true) => face.chat_tool_defs(),
                (Some(c), false) if !params.ablated(ablation::FLAT_PREFILL) => {
                    let mut defs = face.tools_for_categories(c);
                    defs.extend(tools.mcp_tool_defs(c, &face.extra, face.offline));
                    defs
                }
                _ => face.active_defs(),
            };
            // Pre-call estimate: prune, then compact before the server has to refuse.
            // Recency pruning (boubou-65d.6) runs first, from half the window: deterministic,
            // keeps every step's outcome as a fact line, so the ladder has less to summarise
            // and the model keeps knowing what it has already done.
            let mut est = estimate_messages_tokens(&messages, &ct) as f64;
            if params.prune_keep > 0 && est > ctx_budget * PRUNE_AT {
                let pruned = prune_history(&mut messages, params.prune_keep);
                if pruned > 0 {
                    let was = est;
                    est = estimate_messages_tokens(&messages, &ct) as f64;
                    log!(
                        "[context pruning: {pruned} older tool results → fact lines, ~{was:.0} → ~{est:.0} tokens]"
                    );
                }
            }
            if let Some(level) = compaction.due(est, ctx_budget) {
                let (todo, files) = if level >= 2 {
                    (
                        plan_note(plan_dir_for_notes),
                        Some(workdir_files_text(workdir, 30)),
                    )
                } else {
                    (None, None)
                };
                messages = compact(
                    backend, model, messages, level, todo, files, transcript, sink,
                );
                compaction.compacted(level);
                log!("[{i}] pre-flight estimate {est:.0} tokens over the limit, compacted (L{level})");
            }
            let n_img = attach_pending_images(&mut messages, hdir);
            if n_img > 0 {
                log!("[{i}] 🖼 attached {n_img} image(s) (multimodal input)");
            }

            let r = match backend.chat(model, &messages, &ct, &mut |p| match p {
                StreamPiece::Token(t) => sink(AgentEvent::Text(t.to_string())),
                StreamPiece::Think(t) => sink(AgentEvent::Thinking(t.to_string())),
            }) {
                Ok(r) => {
                    retry.on_ok();
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
                    // What a repaired 4xx had wrong, for the retry log line.
                    let mut repair = "";
                    let kind = if e.is_4xx() {
                        // Two 400s the harness itself caused are repaired and retried: the
                        // structural "consecutive assistant turns" error, and an attached image
                        // the server cannot decode (a 0-byte or corrupt file read as an image
                        // ended tier7 CH-03 unattended, 2026-10-30, bd boubou-am3).
                        let structural = low.contains("assistant messages at the end")
                            || low.contains("consecutive");
                        let image = low.contains("image")
                            && (low.contains("decode")
                                || low.contains("invalid format")
                                || low.contains("unsupported"));
                        let before = messages.len();
                        if structural {
                            dedupe_trailing_assistant(&mut messages);
                            if messages.len() != before {
                                repair = "conversation structure (consecutive assistant turns)";
                            }
                        }
                        if image
                            && drop_images(&mut messages, super::history::IMAGE_DROPPED_NOTE) > 0
                        {
                            repair = "request (the server could not decode an attached image; removed it)";
                        }
                        ChatFailure::Http4xx {
                            repaired: !repair.is_empty(),
                        }
                    } else if CTX_OVERFLOW_KEYS.iter().any(|k| low.contains(k)) {
                        ChatFailure::ContextOverflow
                    } else if e.is_500() {
                        ChatFailure::Server
                    } else {
                        ChatFailure::Other
                    };
                    let mut verdict = retry.on_error(kind);
                    if verdict == Retry::RestartEngine {
                        // ★ Root cause, not symptom: ≥4 consecutive 500s = a degraded
                        // ollama process (temp=0 replays the same bad path; compaction and
                        // waiting cannot escape). A restart recovers; the checkpoint keeps
                        // the task. OpenAI-compatible engines are not ours to restart.
                        let fails = retry.fails();
                        match backend.restart_engine() {
                            Some(ok) => {
                                retry.restarted();
                                log!(
                                    "[{i}] {fails} consecutive 500s → ollama process judged degraded, restarting automatically ({}/3)...",
                                    retry.restarts()
                                );
                                if ok {
                                    log!("[{i}] ✓ ollama restarted, resuming the task");
                                } else {
                                    log!("[{i}] ⚠ ollama not ready after restart, still retrying");
                                }
                                nap(3, params.sleep_scale);
                                continue;
                            }
                            None => {
                                log!("[{i}] {fails} consecutive 500s (this engine cannot be restarted by Boubou; compacting and retrying)");
                                verdict = Retry::ServerCompact;
                            }
                        }
                    }
                    match verdict {
                        Retry::Again { nap_secs } => {
                            log!("[{i}] repaired {repair}, retrying");
                            nap(nap_secs, params.sleep_scale);
                            continue;
                        }
                        Retry::GiveUp => {
                            if e.is_4xx() {
                                log!("[{i}] API 4xx error (retrying is pointless, stopped): {e}");
                            }
                            save!();
                            return Err(Error::Ollama(e.to_string()));
                        }
                        Retry::Overflow => {
                            log!(
                                "[{i}] ⚠️ context-overflow error → forcing L3 compaction and retrying"
                            );
                            let todo = plan_note(plan_dir_for_notes);
                            let files = Some(workdir_files_text(workdir, 30));
                            messages =
                                compact(backend, model, messages, 3, todo, files, transcript, sink);
                            compaction.compacted(3);
                            nap(1, params.sleep_scale);
                            continue;
                        }
                        Retry::RestartEngine => unreachable!("resolved above"),
                        Retry::ServerCompact => {
                            if let Some(lvl) = compaction.bump() {
                                let todo = plan_note(plan_dir_for_notes);
                                let files = Some(workdir_files_text(workdir, 30));
                                messages = compact(
                                    backend, model, messages, lvl, todo, files, transcript, sink,
                                );
                                log!(
                                    "[{i}] 500 error, compacted the context (L{lvl}) and retrying"
                                );
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
                        Retry::Backoff { nap_secs, give_up } => {
                            log!(
                                "[{i}] API error: {e} (retry {}, wait {nap_secs}s)",
                                retry.fails()
                            );
                            nap(nap_secs, params.sleep_scale);
                            if give_up {
                                log!("===== FAILED: repeated ollama errors (still failing after restart), stopped. Fix the engine and resume to continue from the checkpoint =====");
                                save!();
                                return Err(Error::Ollama(e.to_string()));
                            }
                            continue;
                        }
                    }
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
                if let Some(level) = compaction.observe(pt as f64, ctx_budget) {
                    let todo = if level >= 2 {
                        plan_note(plan_dir_for_notes)
                    } else {
                        None
                    };
                    messages = compact(
                        backend, model, messages, level, todo, None, transcript, sink,
                    );
                    compaction.compacted(level);
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
                text_ladder.note_truncated();
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
            if salvage_on && tcs.is_empty() && turn.route != Route::Question {
                // Format-leak salvage (guards/leak.rs): the model wrote its call as text.
                if let Some(s) = leak::salvage(&content, params.profile.finish_prose_rescue) {
                    let names: Vec<&str> =
                        s.calls.iter().map(|c| c.function.name.as_str()).collect();
                    log!(
                        "[{i}] ⚡ salvaged tool call(s) from text{}: {}",
                        if s.rescued {
                            " (profile finish-prose rescue)"
                        } else {
                            ""
                        },
                        names.join(", ")
                    );
                    tcs = s.calls;
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
            let mut reply = Message::assistant(content.clone());
            reply.tool_calls = tcs.clone();
            messages.push(reply);
            session::write_checkpoint_to(&ckpt_path, &messages);

            if tcs.is_empty() {
                // ---- text-only turn ----
                // The ladder says what the reply was; the policy says what ends a turn.
                if guards::semantically_empty(&content) {
                    let verdict = text_ladder.on_empty();
                    if verdict == EmptyVerdict::TruncationDebris {
                        messages.push(Message::user(TextLadder::truncation_note(
                            params.num_predict,
                        )));
                        continue;
                    }
                    if policy.text_ends_turn && text_ladder.empty_turns() >= 2 {
                        // One silent retry is enough in a conversation; a context reset
                        // would wipe it.
                        log!(
                            "[{i}] two empty replies in a row; the turn ends ({})",
                            policy.name
                        );
                        exhausted = false;
                        break;
                    }
                    match verdict {
                        EmptyVerdict::Reset { resets, total } => {
                            let task0 = first_user_text(&messages);
                            let files = workdir_files_text(workdir, 30);
                            let head = messages
                                .first()
                                .cloned()
                                .unwrap_or_else(|| Message::system(""));
                            messages = vec![
                                head,
                                Message::user(TextLadder::reset_note(
                                    &task0,
                                    &load_todo_str(hdir),
                                    &files,
                                )),
                            ];
                            log!("[{i}] ⚠️ 6 consecutive empty turns, hard-resetting the context ({resets}/3, {total} empty turns in total)");
                            transcript.mark_compacted();
                            continue;
                        }
                        EmptyVerdict::GiveUp { total } => {
                            log!("[{i}] {total} empty turns in total and 3 resets had no effect; exiting gracefully");
                            save!();
                            return Err(Error::Aborted(format!(
                                "{total} empty turns after 3 context resets"
                            )));
                        }
                        EmptyVerdict::TruncationDebris => unreachable!(),
                        EmptyVerdict::Nudge | EmptyVerdict::Quiet => {}
                    }
                    if turn.route == Route::FollowUp && acted_this_turn && !follow_up_nudged {
                        // The instruction may already be done; ask for the report, not for work.
                        follow_up_nudged = true;
                        messages.push(Message::user(qa::FOLLOW_UP_DONE_NOTE));
                        continue;
                    }
                    if verdict == EmptyVerdict::Nudge {
                        messages.push(Message::user(TextLadder::EMPTY_NUDGE));
                    }
                    continue;
                }
                let verdict = text_ladder.on_text(&content);
                if policy.text_ends_turn {
                    // The reply is the turn's answer: no note, no forced finish, nothing
                    // added to the history the next turn will see.
                    log!("[{i}] text reply ends the turn ({})", policy.name);
                    exhausted = false;
                    break;
                }
                match verdict {
                    TalkVerdict::Repeated { count, dead_loop } => {
                        log!("[{i}] ⚠️ repeated output detected ({count})");
                        if dead_loop {
                            casual_force = true;
                            messages.push(Message::user(TextLadder::DEAD_LOOP_NOTE));
                        } else {
                            messages.push(Message::user(CONTINUE_NOTE));
                        }
                    }
                    TalkVerdict::Fresh { .. } if turn.route == Route::Question => {
                        // The answer to a question is the chat: end the turn.
                        casual_force = true;
                        messages.push(Message::user(
                            "Your answer is already sufficient; the user is asking a question, not setting a task. Call finish now to end this turn. Do not repeat the output.",
                        ));
                    }
                    TalkVerdict::Fresh { .. }
                        if turn.route == Route::FollowUp && acted_this_turn =>
                    {
                        // The follow-up was acted on and this text is the report.
                        casual_force = true;
                        log!("[{i}] follow-up acted on and reported; ending the turn");
                        messages.push(Message::user(qa::FOLLOW_UP_REPORT_NOTE));
                    }
                    TalkVerdict::Fresh { streak, talk } => match talk {
                        Talk::End => {
                            // The model is talking, not working: bound the loop instead of
                            // nagging until max_iterations.
                            log!("[{i}] ⚠️ {streak} consecutive text-only turns with no tool call; ending the turn");
                            casual_force = true;
                            messages.push(Message::user(TextLadder::TALK_END_NOTE));
                        }
                        Talk::Sharp => {
                            messages.push(Message::user(TextLadder::TALK_SHARP_NOTE));
                        }
                        Talk::Continue => messages.push(Message::user(CONTINUE_NOTE)),
                    },
                }
                continue;
            }

            // ---- tool calls ----
            text_ladder.on_tool_call();
            for tc in &tcs {
                let name = tc.function.name.clone();
                let args = args_map(&tc.function.arguments);
                let args_v = Value::Object(args.clone());
                let sig_json = args_v.to_string(); // keys sorted: serde_json Map is a BTreeMap
                let res: String;
                // A note that must follow this call's tool output (call → error → note).
                let mut after_output: Option<String> = None;
                let n = name.as_str();

                let malformed = !is_well_formed_tool_name(n);
                if malformed || ((face.is_disabled(n) || !face.is_active(n)) && !is_mcp_tool(n)) {
                    // A disabled/absent tool called again. The refusal itself feeds the
                    // loop: strong note at 3, hard reset at 10. A malformed name (template
                    // text leaked into the call) takes the same path with its own text.
                    let shown = compact_tool_name(&name);
                    let recall = calls.on_recall(&name);
                    if recall == Recall::Reset {
                        let task0 = first_user_text(&messages);
                        let head = messages
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Message::system(""));
                        messages = vec![
                            head,
                            Message::user(format!(
                                "{task0}\n\n⚠️ [system] The context has been reset: the tool {shown} is disabled, calling it again will not run, and it will no longer appear in your tool list. Progress snapshot:\nPlan:\n{}\nFiles already in the working directory:\n{}\nSwitch to create_file now and write the output files to move the task forward.",
                                load_todo_str(hdir),
                                workdir_files_text(workdir, 30)
                            )),
                        ];
                        log!("[{i}] ⚠️ {shown} called 10 times after being disabled, hard-resetting the context");
                        transcript.mark_compacted();
                        continue;
                    }
                    res = if malformed {
                        format!("[invalid tool name {shown:?}: it contains whitespace or markup, which is chat-template text that leaked into the call's name. The name must be exactly one entry from your tool list; a shell command belongs in the `command` argument of run_bash.]")
                    } else if face.is_disabled(n) {
                        format!("[tool {shown} is disabled for this run. Do not try to read the same content through another tool: write the output file with create_file, or call finish if you already have enough.]")
                    } else {
                        format!("[tool {shown} is not available (not in the tool list assembled for this run: either its category was not loaded, or the name does not exist). Do not invent tool names: create_file for output files, edit_file/create_file to change files, read_file to read; the full list is in your tool definitions.]")
                    };
                    if recall == (Recall::Refuse { note: true }) {
                        messages.push(Message::user(format!(
                            "⚠️ The tool {shown} is unavailable (disabled or not assembled) and you have tried it several times. Calling it again will not run and it will no longer appear in your tool list. Change action now: write output files with create_file; use read_file/list_dir to inspect what exists."
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
                } else if let Some(reason) = ledger.blocked_reason(&name, &sig_json) {
                    // The corrector judged this exact call unfixable from here: it does not
                    // run again this turn.
                    let res = format!("[blocked: the corrector judged this exact {name} call cannot succeed here ({reason}). Do not retry it. Report the failure in the deliverable and finish honestly.]");
                    log!("[{i}] ⚠️ blocked by corrector verdict: {name}");
                    sink(AgentEvent::ToolResult {
                        name: name.clone(),
                        args: args_v.clone(),
                        output: res.clone(),
                        blocked: true,
                    });
                    messages.push(Message::tool_output(res));
                    continue;
                } else if anti_loop && calls.is_duplicate(&name, &sig_json) {
                    // Exactly the same call as last time: the result will not change.
                    let res = format!("[blocked: you just made this exact {name} call (identical arguments); the result will not change. Change strategy: read_file to see the real state, or try a different approach.]");
                    log!("[{i}] ⚠️ duplicate call blocked: {name}");
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
                } else if turn.route == Route::Question
                    && n != "finish"
                    && calls.idle_streak() >= idle::QA_IDLE_CAP
                {
                    // Idle cap on the question route (boubou-9c5): the reads have had their
                    // chance; the answer is due. Two refusals, then the turn ends as a text
                    // answer would.
                    qa_cap_blocks += 1;
                    if qa_cap_blocks >= 2 {
                        casual_force = true;
                    }
                    let idle_streak = calls.idle_streak();
                    let res = idle::qa_cap_result(&name, idle_streak);
                    log!("[{i}] ⚠️ idle streak: {idle_streak} read-only calls on a question, {name} refused ({qa_cap_blocks})");
                    sink(AgentEvent::ToolResult {
                        name: name.clone(),
                        args: args_v.clone(),
                        output: res.clone(),
                        blocked: true,
                    });
                    messages.push(Message::tool_output(res));
                    continue;
                } else if turn.route == Route::Question
                    && face::QA_BLOCKED.contains(&n)
                    && casual_warns < 3
                {
                    // Q&A guard: a question never needs files written. Commands are allowed
                    // (bead boubou-o5a): "what should we do next?" is `bd ready`, "does it
                    // build?" is the build command; the safety gate vets them as always.
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
                    calls.ran(&name, &sig_json);
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
                if n == "run_bash" {
                    let command = args.get("command").and_then(Value::as_str).unwrap_or("");
                    cmd_records.push(integrity::CommandRecord::from_result(command, &res));
                } else if matches!(
                    n,
                    "create_file" | "edit_file" | "append_file" | "delete_file"
                ) {
                    let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                    cmd_records.push(integrity::CommandRecord::file_write(n, path, &res));
                }
                // A successful change to the world makes every earlier verdict about *other*
                // calls stale (same rule as the integrity gate's stale-evidence branch): the
                // corrector's failure counts and blocks for other signatures are lifted, so a
                // verification command that failed before an edit is a fresh attempt after it.
                if cmd_records
                    .last()
                    .is_some_and(|r| r.succeeded() && r.mutates())
                {
                    ledger.lift_others(&name, &sig_json);
                }
                log!(
                    "[{i}|+{}s] ⚙ {name} {} -> {}",
                    t0.elapsed().as_secs(),
                    text::head(&sig_json, 60),
                    text::head(&res, 90)
                );
                if let Some((from, to)) = crate::exec::rebased_path(&res) {
                    log!("[{i}] 📁 path rebased: {from} → {to}");
                }
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
                    if turn.gates.has(Gate::Delivery)
                        && !productive_used
                        && guards::semantically_empty(&content)
                        && guards::semantically_empty(&summary)
                        && fake_finish_warns < 2
                    {
                        fake_finish_warns += 1;
                        log!("[{i}] ⚠️ rejected a hollow finish: nothing delivered (no file, no command, no answer text)");
                        messages.push(Message::user(format!(
                            "⚠️ Your finish was rejected: nothing has been delivered yet (no file created or changed, no command run, no answer in the text). Pick one of three next steps:\n1. If the deliverable is a file: write it with create_file first (when the task asks for answer.txt that is: create_file(path=\"answer.txt\", content=\"<your final answer>\")), then finish again.\n2. If the work is not done: keep going with tools (read, search, compute, write) and call finish when it is complete.\n3. If this is an answer, analysis or explanation task that should not produce files: give the conclusion to the user as body text (not inside summary), then finish again.{}",
                            ws_note(workdir)
                        )));
                        messages.push(Message::tool_output(
                            "[finish rejected: no deliverable detected. Take one of the three actions above, then finish.]",
                        ));
                        continue;
                    }
                    // Gate 2: the turn's own verification must hold at finish (testguard.rs).
                    // A check that passed before later file changes is re-executed through
                    // the tool path and recorded; a turn that changed files in a tested
                    // workdir without running any check is asked for one. The gate names no
                    // runner, and a summary that admits failure is the claims gate's business
                    // (3 refusals, then let through).
                    if turn.gates.has(Gate::Tests)
                        && test_guard_warns < 3
                        && !integrity::admits_failure_about(&summary, "")
                    {
                        match testguard::finish_check(workdir, &cmd_records) {
                            FinishCheck::Nothing => {}
                            FinishCheck::NotRerunnable(c) => {
                                log!(
                                    "[{i}] test guard: `{}` passed before later changes but is not safe to re-run (compound or writing command); the claims gate decides",
                                    crate::text::head(&c, 60)
                                );
                            }
                            FinishCheck::NoCheckRan => {
                                test_guard_warns += 1;
                                log!("[{i}] ⚠️ files changed in a tested workdir and no check ran; finish rejected ({test_guard_warns}/3; let through after 3)");
                                messages.push(Message::user(format!(
                                    "⚠️ Your finish was rejected: this turn changed files and the directory has tests, but no test or check command ran. Run the project's tests with run_bash and finish with what they printed; if they cannot be run, say so in the summary (rejection {test_guard_warns}/3; after 3 rejections finish will be let through)."
                                )));
                                messages.push(Message::tool_output(
                                    "[finish rejected: no check ran after the changes. Run the tests, then finish.]",
                                ));
                                continue;
                            }
                            FinishCheck::Rerun(command) => {
                                let short = crate::text::head(&command, 60).to_string();
                                let res = tools.run_tool("run_bash", &json!({"command": command}));
                                match testguard::rerun_outcome(&res) {
                                    RerunOutcome::CouldNotRun => {
                                        log!("[{i}] test guard: re-ran `{short}` after later changes but it could not run here; the claims gate decides");
                                    }
                                    RerunOutcome::Passed => {
                                        cmd_records.push(integrity::CommandRecord::from_result(
                                            &command, &res,
                                        ));
                                        log!("[{i}] test guard: re-ran `{short}` after later changes: still passes");
                                    }
                                    RerunOutcome::Failed { exit, hint } => {
                                        cmd_records.push(integrity::CommandRecord::from_result(
                                            &command, &res,
                                        ));
                                        test_guard_warns += 1;
                                        let h = if params.ablated(ablation::VERIFY_FEEDBACK) {
                                            "last line of the output".to_string()
                                        } else {
                                            hint
                                        };
                                        let mut bak_hint = String::new();
                                        if h.contains("SyntaxError")
                                            || h.contains("IndentationError")
                                        {
                                            if let Some(b) = first_py_bak(&bak_dir) {
                                                bak_hint = format!("Syntax error detected. A backup is available to roll back: restore the matching .bak (e.g. copy {b} over the original), or rebuild the file.\n");
                                            }
                                        }
                                        log!("[{i}] ⚠️ re-ran `{short}` after later changes: exit {exit}; finish rejected ({test_guard_warns}/3; let through after 3)");
                                        messages.push(Message::user(format!(
                                            "⚠️ Your finish was rejected: your own check `{short}` passed earlier, but after the later changes it fails now (exit {exit}).\nExact failure: {h}\n{bak_hint}Make the implementation and the tests agree, run `{short}` again, and finish once it passes; if it cannot pass, say so in the summary (rejection {test_guard_warns}/3; after 3 rejections finish will be let through)."
                                        )));
                                        messages.push(Message::tool_output(
                                            "[finish rejected: the check that passed earlier fails after the later changes. Fix, re-run it, then finish.]",
                                        ));
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    // Gate 2b (integrity): the summary must not claim a verified outcome the
                    // turn's commands do not back, nor report success over a failed check.
                    if turn.gates.has(Gate::Claims) && claim_gate_warns < 2 {
                        if let Some((log_line, msg)) =
                            integrity::claim_contradiction(&summary, &cmd_records)
                        {
                            claim_gate_warns += 1;
                            log!("[{i}] ⚠️ {log_line}");
                            messages.push(Message::user(msg));
                            messages.push(Message::tool_output(
                                "[finish rejected: the summary contradicts this turn's command results. Verify for real or report the failure, then finish.]",
                            ));
                            continue;
                        }
                    }
                    // Gate 3: files the summary claims must exist. A file the summary reports
                    // as failed ("dataset.csv could not be downloaded") is not a claim.
                    if turn.gates.has(Gate::ClaimedFiles) && finish_claim_warns < 2 {
                        let missing: Vec<String> =
                            artifacts::claimed_missing_files(&summary, workdir)
                                .into_iter()
                                .filter(|f| !integrity::admits_failure_about(&summary, f))
                                .collect();
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
                    // Gate 3b (integrity): files the TASK names must exist, unless the summary
                    // reports them as failed. LRAB WF-04: "Task complete" with fix_summary.md
                    // never written; the summary did not name it, so gate 3 never saw it.
                    if turn.gates.has(Gate::NamedFiles) && named_gate_warns < 1 {
                        let task_text = qa::task_original_text(&messages, 100_000);
                        let t_missing =
                            integrity::task_named_missing(&task_text, &summary, workdir);
                        if !t_missing.is_empty() {
                            named_gate_warns += 1;
                            log!("[{i}] ⚠️ deliverable check: the task names {t_missing:?} but they do not exist, finish rejected");
                            messages.push(Message::user(format!(
                                "⚠️ Your finish was rejected: the task names these files, but they do not exist in the working directory:\n{}\nTwo options: (1) produce each one now with create_file (or the command that writes it) and finish again; (2) if one cannot be produced, say so in the summary — name the file and why — and finish again.{}",
                                bullet(&t_missing),
                                ws_note(workdir)
                            )));
                            messages.push(Message::tool_output(
                                "[finish rejected: deliverables named in the task are missing. Produce them or report why not, then finish.]",
                            ));
                            continue;
                        }
                    }
                    // Gate 4: files the plan names must exist (existence, not checkmarks).
                    // Children are exempt from gates 4–6: one mechanical task, no plan to
                    // maintain; their exit contract (artefacts + finish) is checked by the
                    // dispatcher, and a blocked child only burns its budget into a timeout.
                    if turn.gates.has(Gate::PlanFiles) && plan_gate_warns < 2 {
                        let p_missing: Vec<String> = artifacts::plan_named_missing(hdir, workdir)
                            .into_iter()
                            .filter(|f| !integrity::admits_failure_about(&summary, f))
                            .collect();
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
                    // Gate 5: the plan must be synced before the end (2 refusals max). Not
                    // when the turn's last command was a passing check: probes showed the
                    // model giving up on finish after two "plan out of sync" refusals with the
                    // correct, verified result already on disk. Bookkeeping never outranks a
                    // verified deliverable; the todo is synced for it instead.
                    if turn.gates.has(Gate::PlanSync) && todo_gate_warns < 2 {
                        let (t_done, t_all) = todo_progress(&load_todo(hdir));
                        if t_all > 0 && t_done < t_all && integrity::verified_at_end(&cmd_records) {
                            // A passing check says the work is done, not that every planned
                            // step was taken: the plan is the model's record and stays as it
                            // wrote it (the harness used to tick everything, falsifying it).
                            log!("[{i}] plan out of sync ({t_done}/{t_all}) but the last command was a passing check; finish allowed, plan left as is");
                            todo_gate_warns = 2;
                        } else if t_all > 0
                            && t_done < t_all
                            && integrity::admits_failure_about(&summary, "")
                        {
                            // An honest failure report must not be bounced into ticking steps
                            // that did not happen (loop probe t02/t03: four refusals, then
                            // the dead-loop guard). The plan stays as it is.
                            log!("[{i}] plan out of sync ({t_done}/{t_all}) but the summary reports a failure; finish allowed, plan left as is");
                            todo_gate_warns = 2;
                        } else if t_all > 0 && t_done < t_all {
                            todo_gate_warns += 1;
                            log!("[{i}] ⚠️ plan out of sync ({t_done}/{t_all}), finish rejected");
                            messages.push(Message::user(format!(
                                "⚠️ Your finish was rejected: the plan (todo) still has {} item(s) not marked done ({t_done}/{t_all}).\nCurrent plan:\n{}\nTick off the completed steps first: todo(action=update, index=step number); if everything is done, sync in one go: todo(action=update, all=true). If steps remain undone, carry on and finish afterwards. When you finish again, summarise the outcome for the user in two or three sentences in summary (what was done, what the artefacts are).",
                                t_all - t_done,
                                load_todo_str(hdir)
                            )));
                            messages.push(Message::tool_output(
                                "[finish deferred: sync the plan with actual progress (todo update) first, then finish.]",
                            ));
                            continue;
                        }
                    }
                    // Gate 6: once, re-inject the original task for a self-check. Not when a
                    // verification command passed with nothing changed since: the check is
                    // the self-check, and probes_integrity2 t00/t04 showed the model answering
                    // the re-injected task in prose instead of calling finish again, so the
                    // dead-loop guard ended a run whose verified result was already on disk.
                    if turn.gates.has(Gate::ReRead) && !finish_reread_used {
                        finish_reread_used = true;
                        let orig = if integrity::verified_at_end(&cmd_records) {
                            log!("[{i}] pre-delivery check skipped: the last command was a passing check");
                            String::new()
                        } else {
                            qa::task_original_text(&messages, 2000)
                        };
                        if !orig.is_empty() {
                            log!(
                                "[{i}] ⚠️ pre-delivery check: re-injected the original task ({} chars), asked for a self-check before finishing again",
                                text::clen(&orig)
                            );
                            messages.push(Message::user(format!(
                                "⚠️ Pre-delivery check: before finishing, go through the ORIGINAL TASK INSTRUCTIONS below point by point and confirm your final deliverable meets every one of them, especially the format of answers/artefacts (units, decimal places, rounding), naming, value ranges, and every sub-item the task names explicitly. Details of the brief are easily forgotten by the end of a long task.\nIf anything is not met: fix the deliverable first, then finish again. A step that failed because of the environment (network, service, missing input) is not something to fix here: do not obtain or create that input another way; keep the failure stated in the deliverable and in the summary, and finish again. If everything is met: finish again and summarise the outcome for the user in two or three sentences in summary (what was done, what the artefact/answer is).\n--- original task instructions ---\n{orig}\n--- end of original task instructions ---"
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
                        && turn.gates.has(Gate::WrapUp)
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

                // Isolated corrector: the second identical failure of one signature goes to
                // the same model with a fresh context (task, call, error only). A corrected
                // call runs now; a give-up blocks the signature and spells out the honest
                // exit. Missing-argument errors have their own ladder below.
                let failed_call = n != "finish"
                    && (is_tool_error(&res)
                        || (n == "run_bash"
                            && integrity::exit_code_of(&res).is_some_and(|c| c != 0)));
                if failed_call {
                    let k = ledger.on_failed(&name, &sig_json);
                    if turn.working
                        && !params.ablated(ablation::CORRECTOR)
                        && missing_arg_key(&res).is_none()
                        && ledger.consult(&name, &sig_json, k)
                    {
                        let task_text = qa::task_original_text(&messages, 100_000);
                        let verdict = corrector::consult(
                            &mut *backend,
                            model,
                            &task_text,
                            corrector::FailedCall {
                                name: n,
                                args: &args_v,
                                result: &res,
                                fails: k,
                            },
                            &ct,
                        );
                        match verdict {
                            Verdict::Retry {
                                name: cname,
                                args: cargs,
                                reason: _,
                            } if ledger.blocked_reason(&cname, &cargs.to_string()).is_some() => {
                                // probes_corrector2 t02: the only alternative offered was a
                                // call an earlier give-up had already blocked; that is no fix.
                                let reason = format!("the only alternative proposed was `{cname}`, which an earlier verdict already blocked");
                                log!(
                                    "[{i}] ⚠️ corrector consulted: {name} failed {k}× with identical arguments; verdict: give up: {reason}"
                                );
                                ledger.block(&name, &sig_json, reason.clone());
                                let missing =
                                    integrity::task_named_missing(&task_text, "", workdir);
                                messages.push(Message::user(corrector::give_up_note(
                                    n, &args_v, k, &reason, &missing,
                                )));
                            }
                            Verdict::Retry {
                                name: cname,
                                args: cargs,
                                reason,
                            } => {
                                let cmsg = Message::assistant_call(cname.clone(), cargs.clone());
                                if let Some(tc) = cmsg.tool_calls.first() {
                                    sink(AgentEvent::ToolCall(tc.clone()));
                                }
                                let cres = tools.run_tool(&cname, &cargs);
                                let cfailed = is_tool_error(&cres)
                                    || (cname == "run_bash"
                                        && integrity::exit_code_of(&cres).is_some_and(|c| c != 0));
                                sink(AgentEvent::ToolResult {
                                    name: cname.clone(),
                                    args: cargs.clone(),
                                    output: cres.clone(),
                                    blocked: false,
                                });
                                if cname == "run_bash" {
                                    let command =
                                        cargs.get("command").and_then(Value::as_str).unwrap_or("");
                                    cmd_records.push(integrity::CommandRecord::from_result(
                                        command, &cres,
                                    ));
                                } else if matches!(
                                    cname.as_str(),
                                    "create_file" | "edit_file" | "append_file" | "delete_file"
                                ) {
                                    let path =
                                        cargs.get("path").and_then(Value::as_str).unwrap_or("");
                                    cmd_records.push(integrity::CommandRecord::file_write(
                                        &cname, path, &cres,
                                    ));
                                }
                                if !cfailed && cmd_records.last().is_some_and(|r| r.mutates()) {
                                    // The corrected call changed the world: other signatures'
                                    // counts and blocks are stale now (see the main record site).
                                    ledger.lift_others(&cname, &cargs.to_string());
                                }
                                calls.ran(&cname, &cargs.to_string());
                                let outcome = if cfailed {
                                    "fix failed too"
                                } else {
                                    "fix ran ok"
                                };
                                log!(
                                    "[{i}] ⚠️ corrector consulted: {name} failed {k}× with identical arguments; verdict: retry {cname} {} → {outcome}",
                                    text::head(&cargs.to_string(), 60)
                                );
                                log!(
                                    "[{i}|+{}s] ⚙ {cname} {} -> {}",
                                    t0.elapsed().as_secs(),
                                    text::head(&cargs.to_string(), 60),
                                    text::head(&cres, 90)
                                );
                                messages.push(cmsg);
                                messages.push(Message::tool_output(cres));
                                let why = if reason.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({})", text::head(&reason, 120))
                                };
                                if cfailed {
                                    ledger.block(&name, &sig_json, "two identical failures and a corrected attempt also failed".to_string());
                                    messages.push(Message::user(format!(
                                        "⚠️ Corrector: your `{name}` call had failed {k} times with the same arguments, so an isolated retry tried `{cname}` instead{why}; it failed as well (result above). Treat this step as blocked: do not repeat either call, do not invent the missing input. State the failure in the deliverable and in the finish summary, then finish."
                                    )));
                                } else {
                                    messages.push(Message::user(format!(
                                        "⚠️ Corrector: your `{name}` call had failed {k} times with the same arguments, so an isolated retry ran `{cname}` instead{why}; its result is above. Continue from that result and do not repeat the failed call."
                                    )));
                                }
                            }
                            Verdict::GiveUp { reason } => {
                                log!(
                                    "[{i}] ⚠️ corrector consulted: {name} failed {k}× with identical arguments; verdict: give up: {}",
                                    text::head(&reason, 100)
                                );
                                ledger.block(
                                    &name,
                                    &sig_json,
                                    text::head(&reason, 160).to_string(),
                                );
                                let missing =
                                    integrity::task_named_missing(&task_text, "", workdir);
                                messages.push(Message::user(corrector::give_up_note(
                                    n, &args_v, k, &reason, &missing,
                                )));
                            }
                            Verdict::Unclear => {
                                log!(
                                    "[{i}] ⚠️ corrector consulted: {name} failed {k}× with identical arguments; verdict: unclear"
                                );
                            }
                        }
                    }
                } else {
                    ledger.on_ok(&name, &sig_json);
                }

                // Failure ladder: same tool failing repeatedly → change strategy / disable.
                if is_tool_error(&res) {
                    let failure = if anti_loop {
                        calls.on_failure(&name, missing_arg_key(&res))
                    } else {
                        Failure::Quiet
                    };
                    match failure {
                        Failure::MissingArg { key, streak: st } => {
                            // Missing-argument ladder: gemma-class models drop `path` about one
                            // call in eight and then copy their own failed call from the
                            // history (WF-08: 37 path-less create_file calls in a row). From
                            // the second consecutive miss on, remove the earlier failed pairs
                            // and prescribe a different call shape instead of "retry verbatim".
                            let pruned = prune_missing_arg_calls(&mut messages, n, &key);
                            if pruned > 0 {
                                save!();
                            }
                            let planned = artifacts::plan_named_missing(hdir, workdir);
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
                        Failure::DisableEditFile => {
                            face.disable("edit_file");
                            messages.push(Message::user(
                                "⚠️ edit_file has failed 3 times in a row and is now disabled. From now on change files by writing the complete content with create_file.",
                            ));
                        }
                        Failure::Redirect { count } => {
                            let path_refused = res.contains("outside the working directory")
                                || res.contains("out of bounds");
                            let hint = if path_refused {
                                "⚠️ The rejected path is outside the working directory. All of the task's input files are already in your working directory: run list_dir(path='.') now, read the files it lists, and do not touch any path outside the working directory again."
                            } else if n == "run_bash" {
                                "⚠️ Do not cd to absolute paths like /workspace. The working directory is already set: run commands directly or use relative paths; check the directory with list_dir and the real state with read_file/pwd first."
                            } else {
                                "⚠️ Check the real state with list_dir / read_file first and rebuild the arguments, or try a different approach."
                            };
                            messages.push(Message::user(format!(
                                "⚠️ Your last {count} {name} calls all failed. Stop retrying now and change strategy: {hint}"
                            )));
                        }
                        Failure::Quiet => {}
                    }
                } else {
                    calls.on_success(&name);
                }

                // Repeat-success streaks.
                match calls.on_streak(&name, &sig_json, anti_loop) {
                    Streak::Note { count } => {
                        messages.push(Message::user(format!(
                            "⚠️ You have called {name} {count} times in a row. If the results are similar or nothing new is coming out, stop repeating: combine what you have and move to the next step (write a file / use another tool / call finish). If you are doing a batch of steps with different arguments each time, that is fine; carry on."
                        )));
                    }
                    Streak::Disable { count } => {
                        // Only a true loop: same tool 8×, same arguments 4× in a row.
                        face.disable(n);
                        messages.push(Message::user(format!(
                            "⚠️ {name} has been called {count} times in a row and is now disabled. Stop repeating it and get on with the real work (write files / run / call finish)."
                        )));
                    }
                    Streak::Quiet => {}
                }
                // Idle streak (boubou-9c5): reads, listings, searches, the plan tool and
                // read-only commands; anything that changes the world resets it.
                let idle = if is_mcp_tool(n) {
                    !mcp_producing
                } else {
                    idle::is_idle_call(n, &args)
                };
                let idle_streak = calls.on_idle(idle);
                if idle {
                    let note = if turn.route == Route::Question {
                        (idle_streak == idle::QA_IDLE_NUDGE).then(|| idle::qa_nudge(idle_streak))
                    } else {
                        (idle_streak == idle::TASK_IDLE_NUDGE
                            || idle_streak == idle::TASK_IDLE_NUDGE_2)
                            .then(|| idle::task_nudge(idle_streak))
                    };
                    if let Some(note) = note.filter(|_| anti_loop) {
                        idle_notes += 1;
                        log!("[{i}] ⚠️ idle streak: {idle_streak} read-only calls without progress, nudge {idle_notes}");
                        messages.push(Message::user(note));
                    }
                } else {
                    acted_this_turn = true;
                }
                // Web research: two searches in a row with nothing relevant (the web is the
                // wrong place), and the same query over and over (a new query is progress).
                let research =
                    calls.on_research(&name, &sig_json, res.starts_with(idle::NO_RESULTS_MARK));
                if anti_loop && research.no_results_twice {
                    log!("[{i}] ⚠️ two web searches in a row found nothing");
                    messages.push(Message::user(idle::NO_RESULTS_NOTE));
                }
                if let Some(streak) = research.disable.filter(|_| anti_loop) {
                    for t in RESEARCH_ALL {
                        face.disable(t);
                    }
                    messages.push(Message::user(format!(
                        "⚠️ You have repeated the exact same web query {streak} times without producing any file. The web tools are now disabled: take a genuinely different angle, or write what you have found to a file with create_file and then call finish. To search again, rephrase the query and re-enable the tools with enable_tools."
                    )));
                }
                // Plan-tool loop: same (action,index) 6× in a row, or 15 todo calls without output.
                if let Some((same, count)) = calls.on_todo(&name).filter(|_| anti_loop) {
                    face.disable("todo");
                    messages.push(Message::user(format!(
                        "⚠️ todo is now disabled (the same call {same} times in a row / {count} calls with no real work). Switch to producing tools now: create_file for output files, run_bash to verify; if you need to record a plan, write it into an output file."
                    )));
                }
                // Shotgun-edit guard counts only landed edits.
                if anti_loop && n == "edit_file" && res.starts_with("[edited") {
                    let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                    if let Some(hint) = edit_guard.observe(path) {
                        log!("[{i}] ⚠️ shotgun-edit hint: {path}");
                        messages.push(Message::user(hint));
                    }
                }
                // Time budget pacing (task mode only).
                if budget_total > 0.0 && turn.working {
                    let el = prev_elapsed + t0.elapsed().as_secs_f64();
                    budget::save_elapsed_at(&elapsed_path, el);
                    if let Some(bn) = budget::budget_nudge(budget_total, el, &mut budget_fired) {
                        log!("[{i}] ⏱ budget nudge injected ({:.0}min used)", el / 60.0);
                        messages.push(Message::user(bn));
                    }
                }
                let plan_just_written = dispatch_after_plan
                    && n == "todo"
                    && args.get("action").and_then(Value::as_str) == Some("create")
                    && todo_progress(&load_todo(hdir)).1 > 0;
                messages.push(Message::tool_output(res));
                if let Some(note) = after_output.take() {
                    messages.push(Message::user(note));
                }
                // The plan exists now: the one chance to farm its mechanical items out.
                if plan_just_written {
                    dispatch_after_plan = false;
                    if let Some(hook) = parallel.as_ref() {
                        messages =
                            hook.maybe_dispatch(messages, workdir, hdir, &mut |ev| match ev {
                                HookEvent::Log(l) => sink(AgentEvent::Log(l)),
                                HookEvent::Dispatch(v) => sink(AgentEvent::Dispatch(v)),
                            });
                    }
                }
                // Stale-plan reminder (never a block, twice at most).
                if anti_loop
                    && turn.working
                    && todo_nudge_warns < 2
                    && todo_stale >= TODO_STALE_NUDGE
                {
                    let (t_done, t_all) = todo_progress(&load_todo(hdir));
                    if t_all > 0 && t_done < t_all {
                        let stale = todo_stale;
                        todo_nudge_warns += 1;
                        todo_stale = 0;
                        messages.push(Message::user(format!(
                            "⚠️ Plan progress reminder: you have done several steps of real work but the plan is still at {t_done}/{t_all}. Sync after every step: todo(action=update, index=completed step number).\nCurrent plan:\n{}",
                            load_todo_str(hdir)
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

fn first_py_bak(bak_dir: &Path) -> Option<String> {
    let mut baks: Vec<String> = std::fs::read_dir(bak_dir)
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
