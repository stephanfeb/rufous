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
use super::chat::{ChatBackend, ChatError, Metered, StreamPiece};
use super::corrector::{self, Verdict};
use super::face::{self, ToolFace, PRODUCTIVE, RESEARCH_ALL};
use super::history::{
    attach_pending_images, compact_history, dedupe_trailing_assistant, drop_images,
    estimate_messages_tokens, prune_history, workdir_files_text, PRUNE_AT,
};
use super::ladders::{
    CallLadders, ChatFailure, Circling, CirclingLadder, Compaction, CorrectorLedger, EmptyVerdict,
    Failure, Recall, Retry, RetryLadder, Streak, Talk, TalkVerdict, TextLadder,
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
use crate::hooks::Hooks;
use crate::ollama::{Message, Role, ToolCall};
use crate::ollama_proc::OllamaProc;
use crate::parallel::{env_snapshot, DispatchHook, HookEvent, ParallelConfig};
use crate::skills::{Skill, SkillSet};
use crate::todo::{format_todo_lines, load_todo, load_todo_str, todo_progress, TodoItem};
use crate::tools::{is_mcp_tool, is_well_formed_tool_name, required_args, resolve_alias};
use crate::{text, Error, Result};

/// `_FAIL_MARKERS`: a tool result that means "the action did not happen". Safety-ring
/// refusals count too, otherwise a model that keeps changing paths never hits the ladder.
pub const FAIL_MARKERS: [&str; 28] = [
    "[edit failed",
    "[not run:",
    "[interrupted:",
    "[offline:",
    "[no access",
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
    /// The run's command hooks ([`crate::hooks`]); the executor fires the tool events.
    fn set_hooks(&mut self, _hooks: crate::hooks::Hooks) {}
    /// The turn's scope for the scope gate ([`crate::scope`]); `None` disarms it.
    fn set_scope(
        &mut self,
        _scope: Option<crate::scope::Scope>,
        _mode: crate::scope::ScopeGateMode,
    ) {
    }
    /// Shadow-mode notes of the scope gate since the last call, for the log.
    fn take_scope_notes(&mut self) -> Vec<String> {
        Vec::new()
    }
    /// What the last `run_tool` came to, as data ([`crate::exec::ToolStatus`]); `None` from
    /// a runner that only returns text, which the loop then classifies by the executor's rule.
    fn last_status(&self) -> Option<crate::exec::ToolStatus> {
        None
    }
    /// `mcp_tool_defs`: flattened `server.tool` definitions for the routed categories.
    fn mcp_tool_defs(
        &self,
        _cats: &BTreeSet<String>,
        _extra: &BTreeSet<String>,
        _offline: bool,
    ) -> Vec<Value> {
        Vec::new()
    }
    /// The session's host tools ([`crate::mcp::McpRegistry::host_face`]); none for a runner
    /// without MCP.
    fn host_face(&self, _offline: bool) -> crate::mcp::HostFace {
        crate::mcp::HostFace::default()
    }
    /// Child mode: the sandbox fuse has tripped; the loop must end with exit 53.
    fn child_terminated(&self) -> Option<String> {
        None
    }
    /// The gate's declined writes, for a host that builds a new runner per turn (bead
    /// boubou-vj3); a runner without a gate has none.
    fn take_declined(&mut self) -> Vec<crate::safety::DeclinedWrite> {
        Vec::new()
    }
    /// Seed the gate's declined writes from an earlier turn.
    fn set_declined(&mut self, _declined: Vec<crate::safety::DeclinedWrite>) {}
    /// The hosts contacted since the last call ([`crate::egress`]); none from a runner that
    /// does not reach the network.
    fn take_egress(&mut self) -> Vec<crate::egress::Egress> {
        Vec::new()
    }
}

impl ToolRunner for Executor {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        Executor::run_tool(self, name, args)
    }
    fn take_egress(&mut self) -> Vec<crate::egress::Egress> {
        Executor::take_egress(self)
    }
    fn last_status(&self) -> Option<crate::exec::ToolStatus> {
        Some(Executor::last_status(self))
    }
    fn set_hooks(&mut self, hooks: crate::hooks::Hooks) {
        self.hooks = hooks;
    }
    fn set_scope(&mut self, scope: Option<crate::scope::Scope>, mode: crate::scope::ScopeGateMode) {
        self.gate.set_scope(scope, mode);
    }
    fn take_declined(&mut self) -> Vec<crate::safety::DeclinedWrite> {
        self.gate.take_declined()
    }
    fn set_declined(&mut self, declined: Vec<crate::safety::DeclinedWrite>) {
        self.gate.set_declined(declined);
    }
    fn take_scope_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.gate.scope_notes)
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
    fn host_face(&self, offline: bool) -> crate::mcp::HostFace {
        self.mcp.host_face(offline, crate::mcp::host_tool_cap())
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
/// What a prompt build reads from the agent, beyond the turn's text.
#[derive(Clone, Copy)]
struct PromptInputs<'a> {
    params: &'a RunParams,
    skill_dirs: &'a [PathBuf],
    builtin: &'a [Skill],
    /// `Some` only for the one build that fires the `SessionStart` hooks.
    hooks: Option<&'a Hooks>,
    /// A rebuild's context from the previous system message ([`prompt::session_context`]),
    /// used as it is when `hooks` is `None`.
    carried_context: Option<&'a str>,
    environment: &'a prompt::Environment,
    /// [`face::host_note`]: the line naming the host's tools, on the task and chat prompts.
    host_note: Option<&'a str>,
}

fn task_system(
    base: &'static str,
    inputs: &PromptInputs<'_>,
    sink: &mut Sink,
    task: &str,
) -> String {
    let PromptInputs {
        params,
        skill_dirs,
        builtin,
        hooks,
        carried_context,
        environment,
        host_note: _,
    } = *inputs;
    // SessionStart hooks (a fresh session's first prompt build only): their context enters
    // the task prompt, capped, its size logged so the small-profile reading can see what a
    // project's hooks cost.
    let hook_context = if let Some(hooks) = hooks.filter(|h| !h.is_empty()) {
        let out = hooks.session_start("startup");
        for n in &out.notes {
            sink(AgentEvent::Log(format!("[hooks] {n}")));
        }
        let ctx = crate::hooks::prompt_context(&out.context);
        if let Some(c) = &ctx {
            sink(AgentEvent::Log(format!(
                "[hooks] SessionStart: {} chars of context into the task prompt",
                text::clen(c)
            )));
        }
        ctx
    } else {
        carried_context.map(str::to_string)
    };
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
    let in_effect: Vec<&str> = skills.iter().map(|(n, _)| n.as_str()).collect();
    let skill_catalogue = set.catalogue(
        &in_effect,
        &params.workdir,
        crate::skills::MAX_CATALOGUE_SKILLS,
    );
    let opts = prompt::PromptOptions {
        base: Some(base),
        workdir: Some(&params.workdir),
        system_file: params.system_file.as_deref(),
        unattended: params.unattended,
        skills,
        skill_catalogue,
        profile_note: params.profile.prompt_note.as_deref(),
        hook_context,
        environment: (!params.ablated(ablation::ENV_BLOCK)).then_some(environment),
    };
    prompt::system_prompt(&opts)
}

/// The system prompt the turn's [`Prompt`] names. `task` is the text skills are matched on
/// (the task prompt only).
fn system_for(which: Prompt, inputs: &PromptInputs<'_>, sink: &mut Sink, task: &str) -> String {
    let mut sys = match which {
        Prompt::Task(base) => task_system(base, inputs, sink, task),
        Prompt::Chat => prompt::chat_system(Some(&inputs.params.workdir)),
        Prompt::Child => return prompt::child_system(Some(&inputs.params.workdir)),
    };
    if let Some(note) = inputs.host_note {
        sys.push_str("\n\n");
        sys.push_str(note);
    }
    sys
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
    /// CLI's `<workdir>/.agent_state.json`, which `rufous` resumes from after a kill.
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
    /// Human-readable engine label (`Ollama · host`, `cloud · model`, `LM Studio · url`) and
    /// what chose it; logged at run start as `[engine]` when set (bead boubou-56g).
    pub engine: String,
    /// The engine is Ollama: the model-id check reads Ollama tags, not engine ids.
    pub engine_is_ollama: bool,
    /// Where skills are discovered for prefill, in precedence order (`<workdir>/skills`,
    /// `<agent_home>/skills`, `<install_dir>/skills`); tests narrow it.
    pub skill_dirs: Vec<PathBuf>,
    /// The skills compiled into the product ([`Agent::with_builtin_skills`]), below the
    /// directories; the same list goes to the tool runner for the `skills` tool.
    pub builtin_skills: Vec<Skill>,
    /// System One decider (`decider.rs`): heuristics only unless `config.json` says otherwise.
    pub decider: LayeredDecider,
    /// The run's command hooks ([`crate::hooks`]), loaded from the workdir and rufous's
    /// home; the loop fires `SessionStart` and `Stop`, the executor the tool events.
    pub hooks: Hooks,
    /// `SessionStart` has fired for this agent: once per fresh session, whichever prompt
    /// build comes first.
    hooks_started: bool,
    /// The environment line's facts ([`prompt::Environment`]), probed once here.
    pub environment: prompt::Environment,
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
        if let Some(problem) = cfg.backend_problem() {
            return Err(crate::Error::Config(problem));
        }
        let host = cfg.ollama_host();
        let offline = cfg.offline();
        let (backend, engine): (DynBackend, String) = if let Some(cp) = cfg.cloud_provider() {
            let label = format!("cloud · {} ({})", cp.model, cp.base_url);
            (Box::new(OpenAiBackend::cloud(&cp, &params)?), label)
        } else if let Some(eng) = cfg.backend_openai() {
            let label = format!("{} · {}", eng.label, eng.base_url);
            (Box::new(OpenAiBackend::local(&eng, &params)?), label)
        } else {
            let b = super::OllamaBackend::new(host.clone(), &params)?;
            // Under --no-start-engine the engine is the host's: no restarts from here.
            let b = if cfg.no_start_engine {
                b
            } else {
                b.with_proc(OllamaProc::from_config(cfg))
            };
            (Box::new(b), format!("Ollama · {host}"))
        };
        let engine = format!("{engine} (from {})", cfg.backend_source());
        let engine_is_ollama = cfg.cloud_provider().is_none() && cfg.backend_openai().is_none();
        let mut tools = Executor::new(params.workdir.clone()).with_configured_mcp();
        tools.set_offline(offline);
        tools.slim_output = params.slim_output;
        if let Some(h) = &params.harness_dir {
            tools.harness_dir = h.clone();
        }
        let pcfg = ParallelConfig::from_config(cfg).with_env(&env_snapshot());
        let child_mode = params.child_mode;
        let mut params = params;
        params.scope_gate = cfg.scope_gate;
        let mut agent = Agent::new(params, backend, tools);
        agent.engine = engine;
        agent.engine_is_ollama = engine_is_ollama;
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
        let skill_dirs =
            crate::skills::standard_dirs(&params.workdir, &home, &crate::skills::install_dir());
        let hooks = Hooks::load(&params.workdir, &home);
        tools.set_hooks(hooks.clone());
        let environment = prompt::Environment::probe(&params.workdir);
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
            engine_is_ollama: false,
            skill_dirs,
            builtin_skills: Vec::new(),
            decider: LayeredDecider::off(),
            hooks,
            hooks_started: false,
            environment,
        }
    }

    /// Replace the run's hooks (tests; an embedder with its own files).
    pub fn with_hooks(mut self, hooks: Hooks) -> Self {
        self.tools.set_hooks(hooks.clone());
        self.hooks = hooks;
        self
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

    /// Record the protocol lines in `log` ([`session::SessionLog`]) as well as sending them
    /// to the sink. Call after the sink is set: the log wraps whatever sink is in place.
    /// `None` (the switch is off, or no session is named) changes nothing.
    pub fn with_session_log(mut self, log: Option<session::SessionLog>) -> Self {
        self.set_session_log(log);
        self
    }

    pub fn set_session_log(&mut self, log: Option<session::SessionLog>) {
        if let Some(l) = log {
            let inner = std::mem::replace(&mut self.sink, Box::new(|_| {}));
            self.sink = Box::new(l.tee(inner));
        }
    }

    /// Build the opening message list for a fresh task. Skills whose triggers match the
    /// task (or that autoload) and the workdir's `SYSTEM.md` enter the system prompt here.
    /// A continued session rebuilds the prompt per turn in [`Agent::run`] from the route.
    pub fn task_messages(&mut self, task: &str) -> Vec<Message> {
        // The opening turn of a fresh run is a task; the policy says which prompt frames it
        // (a dispatched child gets its sandbox prompt).
        let which = self.params.turn_policy().turn(Intent::Task, false).prompt;
        let session_hooks = (!self.hooks_started).then_some(&self.hooks);
        self.hooks_started = true;
        let host_note = face::host_note(&self.tools.host_face(self.face.offline));
        let inputs = PromptInputs {
            params: &self.params,
            skill_dirs: &self.skill_dirs,
            builtin: &self.builtin_skills,
            hooks: session_hooks,
            carried_context: None,
            environment: &self.environment,
            host_note: host_note.as_deref(),
        };
        let sys = system_for(which, &inputs, &mut self.sink, task);
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
            engine,
            engine_is_ollama,
            skill_dirs,
            builtin_skills,
            decider,
            hooks,
            hooks_started,
            environment,
        } = self;
        if let Some(s) = session {
            hooks.session_id = s.to_string();
            tools.set_hooks(hooks.clone());
        }
        let model = params.model.as_str();
        let workdir = params.workdir.as_path();
        // The host's tools (docs/host-tools.md): fixed for the run, on every turn and route.
        let host = tools.host_face(face.offline);
        let host_note = face::host_note(&host);
        // Building the face may have probed MCP servers over the network: reported before
        // any call (docs/egress.md).
        let setup_egress = tools.take_egress();
        if !setup_egress.is_empty() {
            sink(AgentEvent::Log(format!(
                "[egress] {}",
                crate::egress::describe(&setup_egress)
            )));
            sink(AgentEvent::Egress(setup_egress));
        }
        if !host.is_empty() {
            sink(AgentEvent::Log(format!(
                "[face] host tools: {} {}: {}",
                host.names.len(),
                if host.inlined {
                    "inlined"
                } else {
                    "over the cap, by mcp_call"
                },
                host.names.join(", ")
            )));
        }
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
        if !hooks.is_empty() {
            log!("[hooks] {}", hooks.describe());
        }
        // A Stop hook may hold the turn open once; after that its block is logged and the
        // turn ends, so a hook that always blocks cannot keep the loop running.
        let mut stop_hook_blocks = 0u32;
        macro_rules! stop_hook {
            () => {{
                let out = hooks.stop(stop_hook_blocks > 0);
                for n in &out.notes {
                    log!("[hooks] {n}");
                }
                match out.blocked {
                    Some(reason) if stop_hook_blocks == 0 => {
                        stop_hook_blocks += 1;
                        Some(reason)
                    }
                    Some(_) => {
                        log!("[hooks] Stop blocked again; ignored after one block per turn");
                        None
                    }
                    None => None,
                }
            }};
        }
        if !engine.is_empty() {
            log!("[engine] {engine}");
            if let Some(w) = model_id_warning(model, *engine_is_ollama) {
                log!("[engine] ⚠️ {w}");
            }
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
        // How the loop ended, for the session meta; `None` while it runs.
        let mut session_end: Option<&str> = None;
        // Every model call of the run is metered here (docs/call-usage.md); `meter!` reports
        // the calls recorded since the last report.
        let mut metered = Metered::new(backend);
        let backend = &mut metered;
        let mut last_used: u64 = 0;
        macro_rules! meter {
            () => {
                for call in backend.take() {
                    sink(AgentEvent::ModelCall {
                        call,
                        used: last_used,
                        limit: params.num_ctx,
                    });
                }
            };
        }
        macro_rules! save {
            () => {
                meter!();
                session::save_session(
                    sessions_dir,
                    session,
                    &messages,
                    Some(workdir),
                    Some(hdir),
                    policy.name,
                    session_end,
                    transcript,
                );
                if session.is_some() {
                    log!("[session saved: {}]", session.unwrap_or_default());
                }
            };
        }

        // Per-run ladders (agent/ladders.rs) and counters.
        let mut retry = RetryLadder::default();
        retry.fail_fast_unreachable = params.engine_unmanaged;
        let mut compaction = Compaction::default();
        let mut calls = CallLadders::default();
        let mut productive_used = false;
        let mut fake_finish_warns = 0;
        let mut finish_claim_warns = 0;
        // Integrity gate (rufous, 2026-10-28): what ran this turn; claim refusals (2, like the
        // artefact gate) and the deliverable refusal (1).
        let mut cmd_records: Vec<integrity::CommandRecord> = Vec::new();
        let mut claim_gate_warns = 0;
        let mut ledger = CorrectorLedger::default();
        let mut named_gate_warns = 0;
        let mut named_writes_warns = 0;
        let mut reattributed_logged = false;
        // The previous request as sent, for the prefix diff (bead boubou-xwi). A continued
        // session starts from the previous turn's last request, kept in its sidecar.
        let continued_from = session.filter(|_| messages.iter().any(|m| m.role == Role::Assistant));
        let mut last_request: Option<RequestDigest> =
            continued_from.and_then(|s| session::load_request(sessions_dir, s));
        let mut since_previous_turn = last_request.is_some();
        if continued_from.is_some() && last_request.is_none() {
            log!("[prefix] no request recorded for the previous turn");
        }
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
        // The plan as this turn found it, and as it last stood (bead boubou-ear): a plan
        // whose content changed this turn is this task's plan; a todo call that changes
        // nothing is not progress (bead boubou-uzl).
        let plan_at_start = load_todo(hdir);
        let mut plan_seen = plan_at_start.clone();
        let mut producing_calls = 0usize;
        let mut plan_holds = 0;
        // The claims hold (bead boubou-bcs): once per turn, at the reply or the finish that
        // ends a conversation turn.
        let claims_hold_on = policy.text_ends_turn && !params.ablated(ablation::CLAIMS_HOLD);
        let mut claims_holds = 0;
        let mut plan_hold_mark: Option<(Vec<TodoItem>, usize)> = None;
        // The circling ladder (bead boubou-m5d); `Some(refused calls)` once at a checkpoint.
        let circling_on = policy.checkpoint && !params.ablated(ablation::CHECKPOINT);
        let mut circling = CirclingLadder::default();
        let mut checkpoint_refusals: Option<usize> = None;
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
            // A fresh session whose opening prompt was not built here (a caller that
            // assembled the messages itself) gets its SessionStart hooks now, once.
            let session_hooks = (!continued && !*hooks_started).then_some(&*hooks);
            *hooks_started = true;
            let carried = session_hooks
                .is_none()
                .then(|| {
                    messages
                        .iter()
                        .find(|m| m.role == Role::System)
                        .and_then(|m| prompt::session_context(&m.content))
                })
                .flatten();
            let inputs = PromptInputs {
                params,
                skill_dirs,
                builtin: builtin_skills,
                hooks: session_hooks,
                carried_context: carried.as_deref(),
                environment,
                host_note: host_note.as_deref(),
            };
            let sys = system_for(turn.prompt, &inputs, sink, &skill_text);
            if let Some(old) = messages.iter().find(|m| m.role == Role::System) {
                log!("[prompt] {}", prompt_change(&old.content, &sys));
            }
            let mut m = vec![Message::system(sys)];
            m.extend(messages.into_iter().filter(|m| m.role != Role::System));
            messages = m;
            log!(
                "[prompt] {} prompt rebuilt for this turn",
                turn.prompt.name()
            );
        }
        // The tool face: the chat tool set on the question route, category routing otherwise,
        // over every instruction of the session so a follow-up keeps the tools the previous
        // turn was sent (bead boubou-g11).
        let cats: Option<BTreeSet<String>> = (turn.route != Route::Question).then(|| {
            let own = face::route_categories(&qa::task_routing_text(&messages));
            if params.ablated(ablation::SESSION_FACE) {
                return own;
            }
            let instructions = qa::instruction_texts(&messages);
            let earlier =
                face::session_categories(&instructions[..instructions.len().saturating_sub(1)]);
            // Files and code are offered whatever the routing, so only the routed rest is news.
            let routed = |a: &BTreeSet<String>, b: &BTreeSet<String>| -> String {
                a.difference(b)
                    .filter(|c| {
                        ![crate::mcp::CAT_FILES, crate::mcp::CAT_CODE].contains(&c.as_str())
                    })
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if !earlier.is_empty() {
                let kept = routed(&earlier, &own);
                if !kept.is_empty() {
                    log!("[face] tools kept from earlier instructions: {kept}");
                }
                let added = routed(&own, &earlier);
                if !added.is_empty() {
                    log!("[face] +{added} from this instruction");
                }
            }
            own.union(&earlier).cloned().collect()
        });
        // Host tools the instruction names (bead boubou-8c4): arm A points at them before the
        // first call, arm B holds a reply that ends the turn without running one.
        let instruction_now = qa::task_routing_text(&messages);
        let mentions = face::host_mentions(&host, &instruction_now);
        if !mentions.is_empty() {
            log!(
                "[host] mention: {}",
                mentions
                    .iter()
                    .map(|(n, t)| format!("{n} (\"{t}\")"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if !params.ablated(ablation::HOST_MENTION_NOTE) {
                messages.push(Message::user(face::host_mention_note(&mentions)));
            }
        }
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
        // The scope gate (bead boubou-6x3): the turn's instruction class, so a tracker or
        // docs turn that starts writing code is asked about. Harness state, not prompt.
        let scope_mode =
            if !policy.scope_gate || !turn.working || params.ablated(ablation::SCOPE_GATE) {
                crate::scope::ScopeGateMode::Off
            } else {
                params.scope_gate
            };
        let scope = (scope_mode != crate::scope::ScopeGateMode::Off)
            .then(|| crate::scope::Scope::of(&qa::task_routing_text(&messages)));
        if let Some(s) = &scope {
            log!("[scope] {} ({:?})", s.describe(), scope_mode);
        }
        tools.set_scope(scope, scope_mode);
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
            let mut ct: Vec<Value> = match (&cats, turn.route == Route::Question) {
                (_, true) => face.chat_tool_defs(),
                (Some(c), false) if !params.ablated(ablation::FLAT_PREFILL) => {
                    let mut defs = face.tools_for_categories(c);
                    defs.extend(tools.mcp_tool_defs(c, &face.extra, face.offline));
                    defs
                }
                _ => face.active_defs(),
            };
            if host.inlined {
                ct.extend(host.defs.iter().cloned());
            } else if !host.is_empty() {
                ct.extend(face.host_fallback(&ct));
            }
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

            // Bead boubou-alj arm A: an earlier turn's own refusal reply goes to the model as
            // a ledger line; the history kept and replayed is unchanged.
            let view = if params.ablated(ablation::REATTRIBUTE) {
                None
            } else {
                super::history::reattribute_declined(&messages)
            };
            if view.is_some() && !reattributed_logged {
                reattributed_logged = true;
                log!("[{i}] ledger: an earlier turn's reply after a declined write is sent as a ledger line");
            }
            let sent: &[Message] = view.as_deref().unwrap_or(&messages);
            let digest = RequestDigest::new(&ct, sent);
            if let Some(prev) = &last_request {
                let since = if since_previous_turn {
                    "since the previous turn: "
                } else {
                    ""
                };
                for line in prefix_change(prev, &digest) {
                    log!("[{i}] [prefix] {since}{line}");
                }
            }
            since_previous_turn = false;
            if let Some(s) = session {
                session::save_request(sessions_dir, s, &digest);
            }
            last_request = Some(digest);
            let r = match backend.chat(model, sent, &ct, &mut |p| match p {
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
                    // A model that sends nothing within the read timeout would do the same
                    // with the same request: the turn ends with a message, the work so far
                    // kept (bead boubou-yqu).
                    if let ChatError::Timeout { mid_reply } = e {
                        let secs = crate::ollama::api_timeout().as_secs();
                        log!(
                            "[{i}] ⏱ reply timeout {}: nothing for {secs}s, turn ended, not resent",
                            if mid_reply {
                                "mid-reply"
                            } else {
                                "before the first token"
                            }
                        );
                        sink(AgentEvent::Error(format!(
                            "No reply from {model} within {secs} s"
                        )));
                        sink(AgentEvent::Text(reply_timeout_note(
                            model, secs, mid_reply, est as u64,
                        )));
                        dedupe_trailing_assistant(&mut messages);
                        session_end = Some("timeout");
                        exhausted = false;
                        break;
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
                        // OpenAI-compatible servers word it differently: "image_url is
                        // only supported by certain models", "model does not support
                        // images", "vision is not supported" (bead boubou-2cr).
                        let image = (low.contains("image")
                            && (low.contains("decode")
                                || low.contains("invalid format")
                                || low.contains("invalid image")
                                || low.contains("unsupported")
                                || low.contains("not support")
                                || low.contains("only supported")))
                            || (low.contains("vision") && low.contains("not support"));
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
                    } else if matches!(e, ChatError::Unreachable(_)) {
                        ChatFailure::Unreachable
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
                                log!("[{i}] {fails} consecutive 500s (this engine cannot be restarted by rufous; compacting and retrying)");
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
                            if matches!(e, ChatError::Unreachable(_)) {
                                log!("[{i}] engine not reachable and not ours to start (--no-start-engine), stopped: {e}");
                                return Err(Error::Ollama(format!(
                                    "{}; rufous does not start the engine under --no-start-engine",
                                    engine_error(engine, &e)
                                )));
                            }
                            return Err(Error::Ollama(engine_error(engine, &e)));
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
                                return Err(Error::Ollama(engine_error(engine, &e)));
                            }
                            continue;
                        }
                    }
                }
            };

            // The main call rides on its context report; side calls made since the last
            // report go out first.
            let mut main_call = {
                let mut calls = backend.take();
                let main = if calls.last().is_some_and(|c| c.purpose == "turn") {
                    calls.pop()
                } else {
                    None
                };
                for call in calls {
                    sink(AgentEvent::ModelCall {
                        call,
                        used: last_used,
                        limit: params.num_ctx,
                    });
                }
                main
            };
            // Post-call context ladder from the real prompt token count.
            if let Some(pt) = r.prompt_eval_count.filter(|&p| p > 0) {
                last_used = pt;
                let pct = pt * 100 / u64::from(params.num_ctx.max(1));
                log!("[ctx: {pt}/{} = {pct}%]", params.num_ctx);
                if let Some(t) = r.timing {
                    log!(
                        "[timing: load {:.1}s · prompt {pt} tok {:.1}s · gen {} tok {:.1}s]",
                        t.load.as_secs_f64(),
                        t.prompt_eval.as_secs_f64(),
                        r.eval_count.unwrap_or(0),
                        t.eval.as_secs_f64()
                    );
                }
                sink(AgentEvent::ContextUsage {
                    used: pt,
                    limit: params.num_ctx,
                    call: main_call.take(),
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
            if let Some(call) = main_call.take() {
                sink(AgentEvent::ModelCall {
                    call,
                    used: last_used,
                    limit: params.num_ctx,
                });
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
                        } else if s.gemma {
                            " (gemma native)"
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
            // After a checkpoint no call runs: each is refused, and after two refusals the
            // harness reports what the turn recorded and ends it (bead boubou-m5d).
            if let Some(refused) = checkpoint_refusals.as_mut().filter(|_| !tcs.is_empty()) {
                for _ in &tcs {
                    messages.push(Message::tool_output(CirclingLadder::REFUSAL));
                }
                *refused += 1;
                log!("[{i}] [circling] tool call refused after the checkpoint ({refused})");
                if *refused >= 2 {
                    let report = circling_report(&circling, &cmd_records, t0.elapsed());
                    log!("[{i}] [circling] harness report after 2 refused calls");
                    sink(AgentEvent::Text(report.clone()));
                    messages.push(Message::assistant(report));
                    exhausted = false;
                    break;
                }
                continue;
            }

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
                if policy.text_ends_turn && checkpoint_refusals.is_some() {
                    // The report the checkpoint asked for: no hook or gate holds it.
                    log!("[{i}] [circling] the report ends the turn");
                    exhausted = false;
                    break;
                }
                if policy.text_ends_turn {
                    if let Some(reason) = stop_hook!() {
                        log!("[{i}] a Stop hook holds the turn open: {reason}");
                        messages.push(Message::user(format!("[stop hook] {reason}")));
                        continue;
                    }
                    // Gate 3c (bead boubou-78s) at the text ending: once, the reply is
                    // held when a file the instruction asks to write got no write and the
                    // reply does not say why.
                    if turn.gates.has(Gate::NamedWrites) && named_writes_warns < 1 {
                        let task_text = qa::task_original_text(&messages, 100_000);
                        let unwritten = integrity::task_named_unwritten(
                            &task_text,
                            &content,
                            &cmd_records,
                            workdir,
                        );
                        if !unwritten.is_empty() {
                            named_writes_warns += 1;
                            let (names, went) = named_writes_log(&unwritten);
                            log!("[{i}] ⚠️ named-write check: the instruction names {names:?} but nothing was written to them this turn{went}, reply held once");
                            messages.push(Message::user(named_writes_note(
                                "Your reply was held",
                                &unwritten,
                                workdir,
                            )));
                            continue;
                        }
                    }
                    // Gate 5 at the text ending (bead boubou-ear): a reply that ends the turn
                    // while this turn's own plan has open items is held, at most twice, the
                    // second time only after progress. A plan this turn did not change never
                    // holds it, so a question or a new instruction ends as before.
                    if turn.gates.has(Gate::PlanSync)
                        && !params.ablated(ablation::PLAN_HOLD)
                        && plan_holds < 2
                    {
                        let plan = load_todo(hdir);
                        if plan != plan_at_start {
                            let (t_done, t_all) = todo_progress(&plan);
                            let progressed = plan_hold_mark
                                .as_ref()
                                .is_none_or(|(p, n)| *p != plan || *n != producing_calls);
                            match plan_sync_check(&plan, &content, &cmd_records) {
                                PlanCheck::Pass => {}
                                PlanCheck::Waived(why) => {
                                    log!("[{i}] plan open ({t_done}/{t_all}) at the text ending; waived: {why}");
                                }
                                PlanCheck::Hold if !progressed => {
                                    log!("[{i}] plan hold released: no progress since the last hold ({t_done}/{t_all})");
                                }
                                PlanCheck::Hold => {
                                    plan_holds += 1;
                                    plan_hold_mark = Some((plan.clone(), producing_calls));
                                    log!("[{i}] ⚠️ plan hold ({t_done}/{t_all}), reply held");
                                    messages.push(Message::user(plan_hold_note(&plan)));
                                    continue;
                                }
                            }
                        }
                    }
                    // The claims hold (bead boubou-bcs): a reply that says this turn's code
                    // works, with nothing run after the last write to show it, is held once.
                    if claims_hold_on
                        && turn.gates.has(Gate::Claims)
                        && turn.route != Route::Question
                    {
                        let instruction = qa::task_routing_text(&messages);
                        if let Some(note) = claims_hold_step(
                            i,
                            &content,
                            &cmd_records,
                            &instruction,
                            &mut claims_holds,
                            &mut |l| log!("{l}"),
                        ) {
                            messages.push(Message::user(note));
                            continue;
                        }
                    }
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
                // What the call came to, as data: the executor's word, or the executor's
                // rule over the text for a runner that has no word (bead boubou-bj4).
                let status: crate::exec::ToolStatus;
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
                    status = crate::exec::ToolStatus::Error;
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
                    status = tools
                        .last_status()
                        .unwrap_or_else(|| crate::exec::ToolStatus::of_text(&res));
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
                // A host tool runs one of the host's automations: real work whatever its name.
                let mcp_producing =
                    is_mcp_tool(n) && (face::is_mcp_producing(n) || host.contains(n));
                if matches!(n, "create_file" | "edit_file" | "append_file" | "run_bash")
                    || mcp_producing
                {
                    productive_used = true;
                    producing_calls += 1;
                }
                if PRODUCTIVE.contains(&n) || mcp_producing {
                    todo_stale += 1;
                } else if n == "todo" {
                    let now = load_todo(hdir);
                    if now != plan_seen {
                        plan_seen = now;
                        todo_stale = 0;
                    }
                }
                for note in tools.take_scope_notes() {
                    log!("[{i}] ⚠️ scope gate {note}");
                }
                if n == "run_bash" {
                    let command = args.get("command").and_then(Value::as_str).unwrap_or("");
                    cmd_records.push(integrity::CommandRecord::from_result(command, &res));
                } else if matches!(
                    n,
                    "create_file" | "edit_file" | "append_file" | "delete_file"
                ) {
                    let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                    cmd_records.push(integrity::CommandRecord::file_write(
                        n,
                        path,
                        status == crate::exec::ToolStatus::Ok,
                    ));
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
                    "[{i}|+{}s] ⚙ {name} {} -> {}{}",
                    t0.elapsed().as_secs(),
                    text::head(&sig_json, 160),
                    text::one_line(text::head(&res, 90)),
                    status.tag().map(|t| format!(" ⟂{t}")).unwrap_or_default()
                );
                // A failed command's first stderr line as a protocol fact (bead boubou-bk9.14):
                // the bench reads it to count failures the shell or an interpreter reported,
                // the class a harness rewrite of the command produces.
                if n == "run_bash" {
                    if let Some((exit, _out, err)) = crate::agent::history::bash_sections(&res) {
                        if exit != "[exit 0]" {
                            let first = err
                                .lines()
                                .map(str::trim)
                                .find(|l| !l.is_empty())
                                .unwrap_or("");
                            log!("[{i}] 🐚 {exit}: {}", text::head(first, 120));
                        }
                    }
                }
                if let Some((from, to)) = crate::exec::rebased_path(&res) {
                    log!("[{i}] 📁 path rebased: {from} → {to}");
                }
                let egress = tools.take_egress();
                if !egress.is_empty() {
                    log!("[{i}] [egress] {}", crate::egress::describe(&egress));
                    sink(AgentEvent::Egress(egress));
                }
                sink(AgentEvent::ToolResult {
                    name: name.clone(),
                    args: args_v.clone(),
                    output: res.clone(),
                    blocked: matches!(status, crate::exec::ToolStatus::Refused(_)),
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
                    // The claims hold at a conversation turn's finish (bead boubou-bcs).
                    if claims_hold_on
                        && turn.gates.has(Gate::Claims)
                        && turn.route != Route::Question
                    {
                        let instruction = qa::task_routing_text(&messages);
                        if let Some(note) = claims_hold_step(
                            i,
                            &summary,
                            &cmd_records,
                            &instruction,
                            &mut claims_holds,
                            &mut |l| log!("{l}"),
                        ) {
                            messages.push(Message::user(note));
                            messages.push(Message::tool_output(
                                "[finish held: the summary claims an outcome nothing shown this turn backs. Run something that shows it, or say it is not yet run, then finish.]",
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
                    // Gate 3c (bead boubou-78s): a file the instruction asks to write got a
                    // landed write this turn, or the summary says why not. The same check
                    // holds a text reply that would end the turn (below).
                    if turn.gates.has(Gate::NamedWrites) && named_writes_warns < 1 {
                        let task_text = qa::task_original_text(&messages, 100_000);
                        let unwritten = integrity::task_named_unwritten(
                            &task_text,
                            &summary,
                            &cmd_records,
                            workdir,
                        );
                        if !unwritten.is_empty() {
                            named_writes_warns += 1;
                            let (names, went) = named_writes_log(&unwritten);
                            log!("[{i}] ⚠️ named-write check: the instruction names {names:?} but nothing was written to them this turn{went}, finish rejected");
                            messages.push(Message::user(named_writes_note(
                                "Your finish was rejected",
                                &unwritten,
                                workdir,
                            )));
                            messages.push(Message::tool_output(
                                "[finish rejected: a file the instruction names was not written. Write it or report why not, then finish.]",
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
                        let plan = load_todo(hdir);
                        let (t_done, t_all) = todo_progress(&plan);
                        match plan_sync_check(&plan, &summary, &cmd_records) {
                            PlanCheck::Pass => {}
                            PlanCheck::Waived(why) => {
                                // The plan is the model's record and stays as it wrote it
                                // (the harness used to tick everything, falsifying it).
                                log!("[{i}] plan out of sync ({t_done}/{t_all}) but {why}; finish allowed, plan left as is");
                                todo_gate_warns = 2;
                            }
                            PlanCheck::Hold => {
                                todo_gate_warns += 1;
                                log!(
                                    "[{i}] ⚠️ plan out of sync ({t_done}/{t_all}), finish rejected"
                                );
                                messages.push(Message::user(format!(
                                    "⚠️ Your finish was rejected: the plan (todo) still has {} item(s) not marked done ({t_done}/{t_all}).\nCurrent plan:\n{}\nTick off the steps that are done: todo(action=update, index=step number). If steps remain undone, carry on and finish afterwards; remove a step that goes beyond the task. When you finish again, summarise the outcome for the user in two or three sentences in summary (what was done, what the artefacts are).",
                                    t_all - t_done,
                                    load_todo_str(hdir)
                                )));
                                messages.push(Message::tool_output(
                                    "[finish deferred: sync the plan with actual progress (todo update) first, then finish.]",
                                ));
                                continue;
                            }
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
                    // A Stop hook may ask for more before the end, once per turn.
                    if let Some(reason) = stop_hook!() {
                        log!("[{i}] a Stop hook holds the turn open: {reason}");
                        messages.push(Message::user(format!("[stop hook] {reason}")));
                        messages.push(Message::tool_output(
                            "[finish deferred: a project hook asked for more before the end; see the message above, then finish again.]",
                        ));
                        continue;
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
                        backend.purpose = "wrap_up";
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
                        backend.purpose = "turn";
                    }
                    meter!();
                    log!("\n===== TASK COMPLETE =====");
                    log!("{res}");
                    sink(AgentEvent::Finished {
                        summary: if summary.trim().is_empty() {
                            content.clone()
                        } else {
                            summary.clone()
                        },
                    });
                    session_end = Some("done");
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
                        backend.purpose = "corrector";
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
                        backend.purpose = "turn";
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
                                let cstatus = tools
                                    .last_status()
                                    .unwrap_or_else(|| crate::exec::ToolStatus::of_text(&cres));
                                let cfailed = cstatus != crate::exec::ToolStatus::Ok
                                    || is_tool_error(&cres)
                                    || (cname == "run_bash"
                                        && integrity::exit_code_of(&cres).is_some_and(|c| c != 0));
                                let egress = tools.take_egress();
                                if !egress.is_empty() {
                                    log!("[{i}] [egress] {}", crate::egress::describe(&egress));
                                    sink(AgentEvent::Egress(egress));
                                }
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
                                        &cname,
                                        path,
                                        cstatus == crate::exec::ToolStatus::Ok,
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
                                    "[{i}|+{}s] ⚙ {cname} {} -> {}{}",
                                    t0.elapsed().as_secs(),
                                    text::head(&cargs.to_string(), 60),
                                    text::one_line(text::head(&cres, 90)),
                                    cstatus.tag().map(|t| format!(" ⟂{t}")).unwrap_or_default()
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
                            // The gate's word, not its prose (bead boubou-bj4).
                            let path_refused = matches!(
                                status,
                                crate::exec::ToolStatus::Refused(crate::safety::Refused {
                                    kind: crate::safety::RefusalKind::Boundary
                                        | crate::safety::RefusalKind::Substitute
                                        | crate::safety::RefusalKind::Child,
                                    ..
                                })
                            );
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
                        log!("[{i}] ⚠️ streak note: {name} {count} in a row, last call repeated");
                        messages.push(Message::user(format!(
                            "⚠️ You have called {name} {count} times in a row and the last call repeated the one before it. If nothing new is coming out, stop repeating: combine what you have and move to the next step (write a file / use another tool / call finish)."
                        )));
                    }
                    Streak::Disable { count } => {
                        // Only a true loop: same tool 8×, same arguments 4× in a row.
                        face.disable(n);
                        log!("[{i}] ⚠️ {name} {count} in a row is now disabled");
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
                let circling_step = if circling_on && turn.working {
                    let shell = cmd_records
                        .last()
                        .filter(|r| n == "run_bash" && r.kind == integrity::ActionKind::Shell)
                        .map(|r| (r.command.as_str(), r.exit, res.as_str()));
                    circling.on_call(shell, res.starts_with("[scope gate:"))
                } else {
                    Circling::Quiet
                };
                messages.push(Message::tool_output(res));
                if let Some(note) = after_output.take() {
                    messages.push(Message::user(note));
                }
                match circling_step {
                    Circling::Quiet => {}
                    Circling::Hypothesis { why } => {
                        log!("[{i}] [circling] stage 1: {why}");
                        messages.push(Message::user(CirclingLadder::hypothesis_note(&why)));
                    }
                    Circling::Checkpoint { why } => {
                        log!("[{i}] [circling] checkpoint: {why}");
                        messages.push(Message::user(CirclingLadder::checkpoint_note(&why)));
                        checkpoint_refusals = Some(0);
                    }
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
            session_end = Some("max-iter");
        }
        save!();
        Ok(messages)
    }
}

/// Gate 3c's note (bead boubou-78s): the plan tool is named as what it is, since both 4B
/// models put the file's content there and called the file done.
fn named_writes_note(opening: &str, files: &[integrity::Unwritten], workdir: &Path) -> String {
    let lines: Vec<String> = files.iter().map(integrity::Unwritten::line).collect();
    format!(
        "⚠️ {opening}: the instruction names these files, but nothing was written to them this turn:\n{}\nThe todo plan is the harness's own list, not a file. Write to the file itself with create_file, edit_file or append_file, then reply again; if it cannot or should not be written, say so and why.{}",
        bullet(&lines),
        ws_note(workdir)
    )
}

/// The log line's parts for gate 3c: the names, and ` (name is ./name; the write went to
/// …)` for the ones written at another path (bead boubou-di6), or nothing.
fn named_writes_log(files: &[integrity::Unwritten]) -> (Vec<&str>, String) {
    let names = files.iter().map(|u| u.name.as_str()).collect();
    let went: Vec<String> = files
        .iter()
        .filter(|u| !u.elsewhere.is_empty())
        .map(integrity::Unwritten::line)
        .collect();
    let went = if went.is_empty() {
        String::new()
    } else {
        format!(" ({})", went.join("; "))
    };
    (names, went)
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
fn compact<B: ChatBackend + ?Sized>(
    backend: &mut Metered<'_, B>,
    model: &str,
    messages: Vec<Message>,
    level: u8,
    todo: Option<String>,
    files: Option<String>,
    transcript: &mut Transcript,
    sink: &mut dyn FnMut(AgentEvent),
) -> Vec<Message> {
    let before = messages.len();
    backend.purpose = "compaction";
    let c = compact_history(
        backend,
        model,
        messages,
        level,
        todo.as_deref(),
        files.as_deref(),
        &mut |s| sink(AgentEvent::Log(s)),
    );
    backend.purpose = "turn";
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

/// What the user is told when the model sent nothing within the read timeout (bead
/// boubou-yqu): where it stalled, and what can be changed. `prompt_tokens` is the pre-call
/// estimate.
pub fn reply_timeout_note(model: &str, secs: u64, mid_reply: bool, prompt_tokens: u64) -> String {
    let stall = if mid_reply {
        "partway through its reply. A large tool call, such as a whole file, is not streamed \
         until it is complete, so a slow model writing one can take longer than the limit. \
         Asking for the work in smaller pieces (a file section by section) helps."
            .to_string()
    } else {
        format!(
            "before its first token. The prompt (~{prompt_tokens} tokens) is processed before \
             anything is sent, and at this model's speed that may take longer than the limit."
        )
    };
    format!(
        "\n\n⏱ Stopped: {model} sent nothing for {secs} s, {stall}\n\n\
         This is the reply time limit, so the turn ends here rather than sending the same \
         request again. To give the model longer, set AGENT_APITIMEOUT (seconds) in the \
         agent's environment (in Zed, an \"env\" entry under the agent server); or use a \
         faster model or a smaller context. The work so far is kept: a follow-up carries on \
         from here."
    )
}

/// Whether a rebuilt system prompt kept the old one's text, and if not where it first
/// differs: a change early in the prompt makes the server prefill the whole history again
/// (bead boubou-xwi).
pub fn prompt_change(old: &str, new: &str) -> String {
    let (o, n) = (text::clen(old), text::clen(new));
    let Some(at) = old
        .chars()
        .zip(new.chars())
        .position(|(a, b)| a != b)
        .or((o != n).then_some(o.min(n)))
    else {
        return format!("system prompt unchanged ({n} chars)");
    };
    let window = |s: &str| -> String {
        s.chars()
            .skip(at.saturating_sub(60))
            .take(120)
            .collect::<String>()
            .replace('\n', "⏎")
    };
    format!(
        "system prompt changed at char {at} of {o}→{n}: …{}… ⇢ …{}…",
        window(old),
        window(new)
    )
}

/// The harness's own reply when the model ignored the checkpoint twice (bead boubou-m5d):
/// what the turn recorded, so the user is not left with nothing.
fn circling_report(
    c: &CirclingLadder,
    cmds: &[integrity::CommandRecord],
    took: Duration,
) -> String {
    let mut files: Vec<(String, usize)> = Vec::new();
    for r in cmds
        .iter()
        .filter(|r| r.kind == integrity::ActionKind::FileWrite && r.exit.is_some())
    {
        let path = r.command.split_once(' ').map_or("", |(_, p)| p).to_string();
        match files.iter_mut().find(|(p, _)| *p == path) {
            Some((_, n)) => *n += 1,
            None => files.push((path, 1)),
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    let written = if files.is_empty() {
        "none".to_string()
    } else {
        files
            .iter()
            .take(5)
            .map(|(p, n)| format!("{p} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let shells = cmds
        .iter()
        .filter(|r| r.kind == integrity::ActionKind::Shell)
        .count();
    let failing = c.worst().map_or(String::new(), |(k, e, n, last)| {
        format!("; longest failing run: `{k}` exited {e} {n} times in a row (last output: {last})")
    });
    format!(
        "Stopped after {} tool calls ({} min) without a report from the model. This turn wrote: {written}. Shell commands: {shells}{failing}. Say how to go on: what to try next, or \"continue\".",
        c.calls(),
        took.as_secs() / 60
    )
}

/// What Gate 5 says about a plan at the end of a turn (bead boubou-ear).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanCheck {
    /// No plan, or every item done.
    Pass,
    /// Open items, but the end stands: the reason, for the log.
    Waived(&'static str),
    /// Open items and nothing waives them.
    Hold,
}

/// Gate 5 (plan sync), for `finish` and for the text reply that ends a turn. Two waivers
/// the probes showed were needed: a passing check last says the work is done even if not
/// every planned step was taken (the model gave up on finish after two refusals with a
/// verified result on disk), and an honest failure report must not be bounced into ticking
/// steps that did not happen (loop probe t02/t03).
pub fn plan_sync_check(
    plan: &[TodoItem],
    reply: &str,
    cmds: &[integrity::CommandRecord],
) -> PlanCheck {
    let (done, all) = todo_progress(plan);
    if all == 0 || done == all {
        PlanCheck::Pass
    } else if integrity::verified_at_end(cmds) {
        PlanCheck::Waived("the last command was a passing check")
    } else if integrity::admits_failure_about(reply, "") {
        PlanCheck::Waived("the reply reports a failure")
    } else {
        PlanCheck::Hold
    }
}

/// The note that holds a reply ending a turn with this turn's plan still open. It names
/// only this plan's open items and says to remove one that goes beyond the instruction, so
/// it never pushes the model into another task (bead boubou-ear).
pub fn plan_hold_note(plan: &[TodoItem]) -> String {
    let (done, all) = todo_progress(plan);
    let open: Vec<String> = plan
        .iter()
        .enumerate()
        .filter(|(_, x)| !x.done)
        .take(3)
        .map(|(k, x)| format!("{}. {}", k + 1, text::head(&x.item, 80)))
        .collect();
    format!(
        "⚠️ Your plan for this task still has {} open item(s) ({done}/{all}): {}. The task is not finished, so do not end the turn or start another task. For each open item: do it if the instruction asked for it, tick it if it is already done (todo(action=update, index=…)), or remove it with a one-line reason if it goes beyond the instruction (for example claiming or starting another task). Then reply.",
        all - done,
        open.join("; ")
    )
}

fn tool_names(defs: &[Value]) -> Vec<String> {
    defs.iter()
        .filter_map(|d| d["function"]["name"].as_str().map(str::to_string))
        .collect()
}

/// One sent message as the prefix diff keeps it: hashes, not the message (bead boubou-xwi).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MessageDigest {
    /// Over the serialised message, so thinking, calls and images count.
    pub hash: String,
    /// Over the content alone, for the "same text" remark.
    pub text_hash: String,
    /// Role, the head of the text and the tools it calls, for the log line.
    pub excerpt: String,
}

/// A request as sent, kept between calls and, through the session's `.request.json`
/// sidecar, between turns, so a turn's first call is compared with the previous turn's
/// last one (bead boubou-xwi).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestDigest {
    pub tools: Vec<String>,
    /// Over the serialised definitions: same names with another description or schema.
    pub tools_hash: String,
    pub msgs: Vec<MessageDigest>,
}

fn sha1_hex(s: &str) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(s.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl RequestDigest {
    pub fn new(tool_defs: &[Value], msgs: &[Message]) -> Self {
        let excerpt = |m: &Message| {
            let calls: Vec<&str> = m
                .tool_calls
                .iter()
                .map(|t| t.function.name.as_str())
                .collect();
            format!(
                "{} {}{}",
                m.role.as_str(),
                text::head(&m.content, 80).replace('\n', "⏎"),
                if calls.is_empty() {
                    String::new()
                } else {
                    format!(" [calls {}]", calls.join(","))
                }
            )
        };
        Self {
            tools: tool_names(tool_defs),
            tools_hash: sha1_hex(&Value::from(tool_defs.to_vec()).to_string()),
            msgs: msgs
                .iter()
                .map(|m| MessageDigest {
                    hash: sha1_hex(&serde_json::to_string(m).unwrap_or_default()),
                    text_hash: sha1_hex(&m.content),
                    excerpt: excerpt(m),
                })
                .collect(),
        }
    }
}

/// Where a request stops extending the previous one: a changed tool set, or the first
/// earlier message that differs. The server reuses its cache only up to there, so whatever
/// follows is processed again (bead boubou-xwi). Empty when the new request only appends.
pub fn prefix_change(old: &RequestDigest, new: &RequestDigest) -> Vec<String> {
    let mut out = Vec::new();
    let (old_tools, tools) = (&old.tools, &new.tools);
    if old_tools != tools {
        let added: Vec<&str> = tools
            .iter()
            .filter(|t| !old_tools.contains(t))
            .map(String::as_str)
            .collect();
        let gone: Vec<&str> = old_tools
            .iter()
            .filter(|t| !tools.contains(t))
            .map(String::as_str)
            .collect();
        out.push(if added.is_empty() && gone.is_empty() {
            "tools reordered".to_string()
        } else {
            format!("tools changed: +{} −{}", added.join(","), gone.join(","))
        });
    } else if old.tools_hash != new.tools_hash {
        out.push("tools changed: same names, descriptions or schemas differ".to_string());
    }
    let (old, new) = (&old.msgs, &new.msgs);
    if let Some(k) = (0..old.len()).find(|&k| new.get(k).map(|m| &m.hash) != Some(&old[k].hash)) {
        fn excerpt(m: Option<&MessageDigest>) -> &str {
            m.map_or("(gone)", |m| m.excerpt.as_str())
        }
        let same_text = new.get(k).is_some_and(|m| m.text_hash == old[k].text_hash);
        out.push(format!(
            "request differs from the previous at message {k} of {} ({} ⇢ {}){}",
            new.len(),
            excerpt(old.get(k)),
            excerpt(new.get(k)),
            if same_text {
                " (same text; calls, thinking or images differ)"
            } else {
                ""
            }
        ));
    }
    out
}

/// A model id written for the other kind of engine (bead boubou-56g): on Ollama an LM
/// Studio id (`gemma-4-12b`, `google/gemma-4-12b`) that [`crate::profile::ollama_tag`]
/// rewrites; on an OpenAI-compatible engine an Ollama tag (`gemma4:12b`). Warning only.
pub fn model_id_warning(model: &str, ollama: bool) -> Option<String> {
    let m = model.trim();
    if ollama {
        if m.starts_with("hf.co/") || m.starts_with("huggingface.co/") {
            return None;
        }
        let tag = crate::profile::ollama_tag(m);
        (tag != m.to_lowercase())
            .then(|| format!("{m} looks like an LM Studio id; Ollama names it {tag}"))
    } else {
        m.contains(':').then(|| {
            format!("{m} looks like an Ollama tag; this engine names its models differently (its /v1/models list)")
        })
    }
}

/// The text of a failed model request, naming the engine it went to.
fn engine_error(engine: &str, e: &impl std::fmt::Display) -> String {
    let engine = engine.split(" (from ").next().unwrap_or(engine);
    if engine.is_empty() {
        e.to_string()
    } else {
        format!("{engine}: {e}")
    }
}

/// One claims-hold decision (bead boubou-bcs): the note to send when `reply` makes an
/// outcome claim nothing after the turn's last write backs and the turn has not been held
/// for one yet; logs the claim either way.
fn claims_hold_step(
    i: usize,
    reply: &str,
    cmds: &[integrity::CommandRecord],
    instruction: &str,
    holds: &mut usize,
    log: &mut dyn FnMut(String),
) -> Option<String> {
    let claim = integrity::outcome_claim(reply)?;
    match integrity::claim_backing(cmds, instruction) {
        integrity::ClaimBacking::NoWrite => None,
        integrity::ClaimBacking::Backed(by) => {
            log(format!("[{i}] [claims] \"{claim}\" backed by {by}"));
            None
        }
        integrity::ClaimBacking::Unbacked(why) if *holds >= 1 => {
            log(format!(
                "[{i}] [claims] released: \"{claim}\" still unbacked ({why})"
            ));
            None
        }
        integrity::ClaimBacking::Unbacked(why) => {
            *holds += 1;
            log(format!(
                "[{i}] ⚠️ [claims] hold: \"{claim}\" ({why}), reply held"
            ));
            Some(integrity::claims_hold_note(&claim, &why))
        }
    }
}
