//! Running the agent for the web UI: prefs → `RunParams`, Mingbird's resume semantics,
//! and the mapping of [`AgentEvent`] to SSE events.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use boubou_core::agent::session::{load_session, sessions_dir};
use boubou_core::agent::{Agent, AgentEvent, RunParams};
use boubou_core::config::Config;
use boubou_core::guards::budget::resolve_time_budget;
use boubou_core::ollama::Message;
use boubou_core::safety::{Confirmer, Gate, SafetyPolicy};
use boubou_core::text::head;
use boubou_core::todo::todo_file;
use serde_json::{json, Map, Value};

use crate::events::Bus;

/// One `POST /api/chat`.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub prompt: String,
    /// Model tag (display names already mapped through the config).
    pub model: String,
    /// Continue the current session (`--append`) instead of starting clean.
    pub resume: bool,
    pub session: String,
    pub workdir: PathBuf,
    pub home: PathBuf,
    pub prefs: Map<String, Value>,
    pub config: Config,
}

/// Runs one request to completion on the calling thread. The production implementation
/// drives the real agent; tests script one.
pub trait Spawner: Send + Sync {
    fn run(
        &self,
        req: RunRequest,
        stop: &Arc<AtomicBool>,
        bus: Arc<Bus>,
        confirmer: Box<dyn Confirmer>,
    );
}

/// Map one loop event to the SSE vocabulary the page understands.
pub fn publish(bus: &Bus, ev: AgentEvent) {
    match ev {
        AgentEvent::Text(t) => {
            bus.push("tok", Value::String(t), Map::new());
        }
        AgentEvent::Thinking(t) => {
            bus.push("think", Value::String(t), Map::new());
        }
        AgentEvent::ContextUsage { used, limit } => {
            let pct = if limit == 0 {
                0
            } else {
                (used * 100 / u64::from(limit)).min(100)
            };
            bus.push(
                "ctx",
                json!({"used": used, "total": limit, "pct": pct}),
                Map::new(),
            );
        }
        AgentEvent::ToolCall(tc) => {
            // Rendered as a chip "⚙ <kind> <name>"; Mingbird used this for sub-agent dispatch,
            // Boubou shows every tool call that way.
            // `name` is the 80-char head Mingbird's page showed; `args` is the full value so
            // the run block can summarise the call (path, command, query) properly.
            let args = tc.function.arguments.to_string();
            bus.push(
                "dispatch",
                json!({"kind": tc.function.name, "name": head(&args, 80), "args": tc.function.arguments}),
                Map::new(),
            );
        }
        AgentEvent::Log(l) => {
            // The `[ctx: …]` line is already a `ctx` event; the page ignores `log` anyway.
            if !l.starts_with("[ctx:") {
                bus.push("log", Value::String(l), Map::new());
            }
        }
        AgentEvent::Error(e) => bus.note(format!("[error] {e}")),
        AgentEvent::Dispatch(v) => {
            // Parallel-dispatch progress: a "⚙ parallel <label>" chip, full event attached.
            let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let label = match s("phase").as_str() {
                "start" => format!(
                    "start total={} max_parallel={}",
                    v["total"], v["max_parallel"]
                ),
                "done" => format!("done ok={} failed={}", v["ok"], v["failed"]),
                _ => format!("{} #{} {}", s("status"), v["index"], s("task")),
            };
            bus.push(
                "dispatch",
                json!({"kind": "parallel", "name": head(&label, 80), "event": v}),
                Map::new(),
            );
        }
        AgentEvent::Finished { summary } => {
            // Mingbird printed the summary as a stdout line the page ignores; a tool-only run
            // then showed no answer at all. Surface it as answer text instead.
            let t = summary.trim();
            if !t.is_empty() {
                bus.push("tok", Value::String(format!("\n\n{t}")), Map::new());
            }
            bus.note("===== TASK COMPLETE =====");
        }
        AgentEvent::MaxIterations => bus.note("===== MAX ITERATIONS ====="),
        AgentEvent::ToolResult {
            name,
            args,
            output,
            blocked,
        } => {
            // The page times each row itself (dispatch → tool_done) and shows the first line
            // of the output in the run block; the full output lives in the session file.
            bus.push(
                "tool_done",
                json!({"name": name, "args": args, "blocked": blocked, "output": head(&output, 160)}),
                Map::new(),
            );
        }
        AgentEvent::Compacted {
            level,
            before,
            after,
        } => bus.note(format!(
            "[context compacted: level {level}, {before} → {after}]"
        )),
    }
}

/// `RunParams` from the environment with the UI preferences applied on top (Mingbird set
/// `AGENT_CTX` / `AGENT_TEMP` / `AGENT_NUMPREDICT` / `AGENT_THINK` / `AGENT_SYSTEM_FILE`).
pub fn params_from_prefs(
    model: &str,
    workdir: &Path,
    home: &Path,
    prefs: &Map<String, Value>,
) -> boubou_core::Result<RunParams> {
    let mut p = RunParams::from_env(model, workdir.to_path_buf())?;
    if let Some(ctx) = prefs.get("ctx").and_then(Value::as_u64) {
        // `Agent::standard` resolves slim output from this context size, the config and
        // the environment.
        p.num_ctx = ctx as u32;
    }
    p.temperature = prefs.get("temp").and_then(Value::as_f64).map(|t| t as f32);
    if let Some(np) = prefs.get("num_predict").and_then(Value::as_i64) {
        if np > 0 {
            p.num_predict = np as i32;
        }
    }
    p.think = prefs.get("think").and_then(Value::as_bool);
    // A chat turn is a standard agent turn, as in the editor: text or `finish` ends it and
    // the batch gates stay out of the way. `batch: true` opts a session into Mingbird's
    // unattended task run (finish gates, test guard, continue notes, child dispatch) for
    // watching the harness work a task in the workdir.
    p.interactive = prefs.get("batch").and_then(Value::as_bool) != Some(true);
    if prefs.get("sys_enable").and_then(Value::as_bool) == Some(true) {
        let f = home.join("system_override.txt");
        if f.is_file() {
            p.system_file = Some(f);
        }
    }
    Ok(p)
}

/// The production spawner: the real agent on the real Ollama.
#[derive(Debug, Default)]
pub struct AgentSpawner;

impl Spawner for AgentSpawner {
    fn run(
        &self,
        req: RunRequest,
        stop: &Arc<AtomicBool>,
        bus: Arc<Bus>,
        confirmer: Box<dyn Confirmer>,
    ) {
        if let Err(e) = std::fs::create_dir_all(&req.workdir) {
            bus.note(format!("[error] workdir {}: {e}", req.workdir.display()));
            return;
        }
        // Mingbird wrote the prompt to task_input.txt for the CLI; keep the file for parity.
        let _ = std::fs::write(req.workdir.join("task_input.txt"), &req.prompt);
        if !req.resume {
            for stale in [".agent_state.json", "todo.json"] {
                let _ = std::fs::remove_file(req.workdir.join(stale));
            }
        }
        let params = match params_from_prefs(&req.model, &req.workdir, &req.home, &req.prefs) {
            Ok(p) => p,
            Err(e) => {
                bus.note(format!("[error] {e}"));
                return;
            }
        };
        let budget_env = params.budget_env_sec.unwrap_or(0.0);
        let (tb_min, _) = resolve_time_budget(
            None,
            Some(&req.prompt),
            budget_env,
            std::env::var("AGENT_TIME_BUDGET_PARSE")
                .ok()
                .is_none_or(|v| v != "0"),
        );
        let sink_bus = bus.clone();
        let mut agent = match Agent::standard(params, &req.config) {
            Ok(a) => a,
            Err(e) => {
                bus.note(format!("[error] {e}"));
                return;
            }
        };
        if !agent.engine.starts_with("Ollama") {
            bus.note(format!("[engine: {}]", agent.engine));
        }
        agent.tools.gate = Gate::new(SafetyPolicy::from_env(), confirmer);
        let mut agent = agent
            .with_sink(move |ev| publish(&sink_bus, ev))
            .with_stop(stop.clone());
        let sdir = sessions_dir(&req.home);
        // Only the session's own file is continued. The workdir checkpoint is flushed from
        // the same messages every iteration, and falling back to it is the one path that can
        // splice another session's conversation into this one.
        let msgs: Vec<Message> = if req.resume {
            let prior = load_session(&sdir, &req.session).unwrap_or_default();
            if prior.is_empty() {
                agent.task_messages(&req.prompt)
            } else {
                let mut m = prior;
                m.push(Message::user(req.prompt.clone()));
                m
            }
        } else {
            agent.task_messages(&req.prompt)
        };
        if msgs.len() == 2 {
            let _ = std::fs::remove_file(todo_file(&req.workdir));
        }
        if let Err(e) = agent.run(msgs, Some(&req.session), tb_min.map(|m| m * 60.0)) {
            bus.note(format!("[error] {e}"));
        }
    }
}
