//! `_maybe_parallel_dispatch`: the harness-level hook at the start of a fresh task.
//!
//! When the plan is large, a batch of its items is mechanical, and the machine can hold a
//! small model next to the parent, those items are dispatched to headless children before
//! the parent model takes its first turn. The model does not need to know the mechanism
//! exists: the system prompt is unchanged and the trigger lives entirely in code. After a
//! dispatch the parent gets one user message prefixed with [`DISPATCH_NOTE_MARK`], which
//! routing and Q&A detection skip and which makes a resumed run idempotent.
//!
//! Any fault in the dispatch layer degrades to serial execution; it never fails the task.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::config::ParallelConfig;
use super::dispatch::summary_for_parent;
use super::dispatch::{ParallelDispatcher, ResultIntegrator, Spawner};
use super::probe::{CapacityPlanner, ResourceProbe};
use super::trigger::{should_dispatch, JsonTodoProvider};
use crate::guards::qa::DISPATCH_NOTE_MARK;
use crate::ollama::{Message, Role};

/// What the hook reports while it works.
#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    /// A `[parallel] …` log line.
    Log(String),
    /// A dispatcher progress event (`phase`: start / progress / done).
    Dispatch(Value),
}

/// The hook's dependencies. Production fills `probe` / `spawner` lazily with the real
/// machine probe and process spawner; tests inject stubs.
pub struct DispatchHook {
    pub cfg: ParallelConfig,
    pub env: HashMap<String, String>,
    pub ollama_host: String,
    /// Set in a child process: a child never dispatches (third depth lock).
    pub child_mode: bool,
    pub probe: Option<Arc<dyn CapacityPlanner>>,
    pub spawner: Option<Arc<dyn Spawner>>,
    pub agent_exe: Option<PathBuf>,
    /// The agent's stop flag: raised, the dispatcher kills its live children at the next
    /// poll and hands the batch back. Each child runs in its own process group (so a
    /// timeout can kill the whole tree), which is also why a terminal's Ctrl-C no longer
    /// reaches them by itself; the CLI's handler raises this flag instead.
    pub abort: Arc<AtomicBool>,
}

impl std::fmt::Debug for DispatchHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchHook")
            .field("enabled", &self.cfg.enabled)
            .field("child_model", &self.cfg.child_model)
            .field("child_mode", &self.child_mode)
            .finish_non_exhaustive()
    }
}

impl DispatchHook {
    pub fn new(cfg: ParallelConfig, ollama_host: impl Into<String>) -> Self {
        Self {
            cfg,
            env: super::config::env_snapshot(),
            ollama_host: ollama_host.into(),
            child_mode: false,
            probe: None,
            spawner: None,
            agent_exe: None,
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_abort(mut self, flag: Arc<AtomicBool>) -> Self {
        self.abort = flag;
        self
    }

    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    pub fn with_child_mode(mut self, on: bool) -> Self {
        self.child_mode = on;
        self
    }

    pub fn with_probe(mut self, p: Arc<dyn CapacityPlanner>) -> Self {
        self.probe = Some(p);
        self
    }

    pub fn with_spawner(mut self, s: Arc<dyn Spawner>) -> Self {
        self.spawner = Some(s);
        self
    }

    /// Does the fast, side-effect-free part say "do not even look"? The four gates that
    /// Mingbird checks before importing the dispatch modules.
    fn skip(&self, messages: &[Message]) -> bool {
        if self.child_mode {
            return true;
        }
        if super::config::hard_disabled(&self.env) {
            return true;
        }
        // Only a brand-new task: no tool results yet (a resumed run is left alone).
        if messages.iter().any(|m| m.role == Role::Tool) {
            return true;
        }
        // Idempotence: a receipt already in the history means the dispatch happened.
        if messages
            .iter()
            .any(|m| m.content.starts_with(DISPATCH_NOTE_MARK))
        {
            return true;
        }
        if messages.len() < 2 {
            return true;
        }
        !self.cfg.enabled
    }

    /// The hook. Returns the (possibly extended) message list.
    pub fn maybe_dispatch(
        &self,
        mut messages: Vec<Message>,
        workdir: &Path,
        emit: &mut dyn FnMut(HookEvent),
    ) -> Vec<Message> {
        if self.skip(&messages) {
            return messages;
        }
        match self.dispatch(workdir, emit) {
            Ok(Some(note)) => {
                messages.push(Message::user(format!("{DISPATCH_NOTE_MARK} {note}")));
            }
            Ok(None) => {}
            Err(e) => emit(HookEvent::Log(format!(
                "[parallel] dispatch layer failed, falling back to serial: {e}"
            ))),
        }
        messages
    }

    fn dispatch(
        &self,
        workdir: &Path,
        emit: &mut dyn FnMut(HookEvent),
    ) -> Result<Option<String>, String> {
        let cfg = &self.cfg;
        let mut provider = JsonTodoProvider::new(workdir);
        let probe: Arc<dyn CapacityPlanner> = match &self.probe {
            Some(p) => p.clone(),
            None => Arc::new(ResourceProbe::new(self.ollama_host.clone())),
        };
        let plan = should_dispatch(probe.as_ref(), &provider, cfg, &self.env, None);
        if !plan.should {
            emit(HookEvent::Log(format!(
                "[parallel] not dispatching: {}",
                plan.reasons
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ")
            )));
            return Ok(None);
        }
        let model = cfg.child_model.clone();
        let max_par = plan.capacity.as_ref().map(|c| c.max_parallel).unwrap_or(0);
        emit(HookEvent::Log(format!(
            "[parallel] triggered: {} simple item(s) -> dispatched to {model} in parallel (max_parallel={max_par})",
            plan.n_items()
        )));
        // Progress events cross worker threads; collect them and replay in order.
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let mut dispatcher = ParallelDispatcher::new(cfg.clone())
            .with_env(self.env.clone())
            .with_abort(self.abort.clone())
            .with_progress(Arc::new(move |ev| {
                if let Ok(mut v) = sink.lock() {
                    v.push(ev);
                }
            }));
        if let Some(s) = &self.spawner {
            dispatcher = dispatcher.with_spawner(s.clone());
        }
        if let Some(exe) = &self.agent_exe {
            dispatcher = dispatcher.with_agent_exe(exe.clone());
        }
        let depth = super::config::current_depth(&self.env);
        let mut result = dispatcher.run(
            plan.items,
            workdir,
            probe.as_ref(),
            Some(&model),
            None,
            depth,
        );
        for ev in events.lock().map(|v| v.clone()).unwrap_or_default() {
            emit(HookEvent::Dispatch(ev));
        }
        let (integrated, _conflicts, notes) = ResultIntegrator::new(cfg)
            .with_provider(&mut provider)
            .integrate(&mut result, workdir);
        if !notes.is_empty() {
            emit(HookEvent::Log(format!(
                "[parallel] {}",
                notes.replace('\n', " | ")
            )));
        }
        let note = summary_for_parent(&result, &integrated);
        Ok((!note.is_empty()).then_some(note))
    }
}
