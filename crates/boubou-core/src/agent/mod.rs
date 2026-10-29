//! The agent loop (`agent_loop` and friends in `ollama_agent.py`).
//!
//! Layout:
//! - [`chat`]: the model-call boundary (`call_chat`) as a trait, plus the Ollama backend.
//! - [`face`]: which tools the model sees this turn (`_active_tools`, `_disabled_tools`,
//!   category routing, `enable_tools` unlocking).
//! - [`history`]: context hygiene (`compact_history`, `_dedupe_trailing_assistant`,
//!   `sanitize_ckpt`, `_similar`, pending-image attachment).
//! - [`session`]: checkpoint, transcript and session files, byte-compatible with Mingbird.
//! - [`prompt`]: the system prompts and the platform block.
//! - [`testguard`]: the finish-gate test runner.
//! - [`run`]: the loop body itself.
//!
//! The loop is synchronous: it is a state machine that blocks on one model call at a
//! time, exactly as Mingbird runs it, so a GUI drives it from a worker thread and tests
//! drive it with a scripted backend.

pub mod chat;
pub mod corrector;
pub mod face;
pub mod history;
pub mod openai;
pub mod prompt;
pub mod run;
pub mod session;
pub mod testguard;

use std::path::PathBuf;

use serde_json::Value;

use crate::ollama::ToolCall;
use crate::{Error, Result};

pub use chat::{ChatBackend, ChatError, ChatResponse, DynBackend, OllamaBackend, StreamPiece};
pub use face::ToolFace;
pub use openai::OpenAiBackend;
pub use run::{Agent, ToolRunner};

pub const DEFAULT_NUM_CTX: u32 = 131_072;
pub const DEFAULT_NUM_PREDICT: i32 = 8192;
/// `AGENT_BASH_TIMEOUT`, seconds.
pub const DEFAULT_BASH_TIMEOUT_SECS: u64 = 120;
/// `AGENT_KEEPALIVE`: keep the model resident across long tool chains.
pub const DEFAULT_KEEP_ALIVE: &str = "30m";
/// Hard cap on model turns per `run` (Mingbird `for i in range(200)`).
pub const MAX_ITERATIONS: usize = 200;
/// Context occupancy at which the history is compacted (L3).
pub const COMPACT_AT: f32 = 0.90;

/// Per-run knobs. Env names are inherited from Mingbird so existing launch scripts work.
#[derive(Debug, Clone, PartialEq)]
pub struct RunParams {
    pub model: String,
    pub workdir: PathBuf,
    pub num_ctx: u32,
    pub num_predict: i32,
    /// `AGENT_TEMP`: unset = omit from request.
    pub temperature: Option<f32>,
    /// `AGENT_THINK`: unset = omit; `1` = on; `0` = off.
    pub think: Option<bool>,
    /// `AGENT_SYSTEM_FILE`: replacement system prompt.
    pub system_file: Option<PathBuf>,
    /// `AGENT_SLIM_OUTPUT`, or automatic when `num_ctx <= 32K`.
    pub slim_output: bool,
    /// `AGENT_KEEPALIVE`.
    pub keep_alive: String,
    /// `AGENT_TIME_BUDGET_SEC`: module-level fallback when `run` gets no explicit budget.
    pub budget_env_sec: Option<f64>,
    /// `AGENT_UNATTENDED=1`: wording of the platform block (hard refusals, no confirms).
    pub unattended: bool,
    /// `AGENT_ABLATION`: comma-separated switches used by the benchmark harness.
    pub ablation: Vec<String>,
    pub max_iterations: usize,
    /// `AGENT_CHILD_SANDBOX=1`: this process is a dispatched child (ring-0 sandbox,
    /// minimal prompt, narrowed tool face, never Q&A, never dispatches itself).
    pub child_mode: bool,
    /// Multiplier on the loop's retry/back-off sleeps (`AGENT_SLEEP_SCALE`; tests use 0).
    pub sleep_scale: f32,
    /// `AGENT_PRUNE_KEEP`: tool results kept verbatim by recency pruning (5 is the measured
    /// setting, [`history::PRUNE_KEEP`]); 0, the default, leaves the ladder alone as Mingbird.
    pub prune_keep: usize,
    /// Interactive chat (`boubou chat`): a plain text reply ends the turn instead of
    /// drawing a "continue with tools" note. The finish gates and the Q&A route still apply.
    pub chat_mode: bool,
    /// An editor session (the ACP server): standard agent-turn semantics instead of the
    /// unattended batch harness. The turn ends when the model replies in text without a
    /// tool call or calls `finish`; `finish` is accepted as is (no delivery, test, artefact,
    /// plan or self-check gate: those exist for batch tasks and would run the user's own
    /// test suite and bounce the turn); no "continue" notes; every turn takes the task
    /// prompt and the full tool set (no Q&A route, so an instruction phrased as a question
    /// can still write files); no parallel dispatch into the user's project. The safety
    /// gate, the duplicate-call and failure ladders and compaction are unchanged.
    pub interactive: bool,
    /// Where the loop's own files go: `todo.json`, the backups edits leave behind, the
    /// trash of deleted files and the image queue. `None` is the workdir itself (the CLI,
    /// Mingbird's layout). An embedding whose workdir is the user's project sets
    /// [`session::harness_dir`] so the project never shows the safety net: the model was
    /// seen deleting one `.bak` nine times in a 47-call editor turn (boubou-7s3).
    pub harness_dir: Option<PathBuf>,
    /// The model's profile (`profile.rs`): built-ins at construction, config overrides and
    /// the think/temperature defaults applied by [`Agent::standard`].
    pub profile: crate::profile::ModelProfile,
}

impl RunParams {
    pub fn from_env(model: impl Into<String>, workdir: impl Into<PathBuf>) -> Result<Self> {
        let num_ctx = env_parse("AGENT_CTX")?.unwrap_or(DEFAULT_NUM_CTX);
        let slim_env: Option<u8> = env_parse("AGENT_SLIM_OUTPUT")?;
        let budget: Option<f64> = env_parse("AGENT_TIME_BUDGET_SEC")?;
        let model = model.into();
        let profile = crate::profile::builtin_for(&model);
        Ok(Self {
            model,
            workdir: workdir.into(),
            num_ctx,
            num_predict: env_parse("AGENT_NUMPREDICT")?.unwrap_or(DEFAULT_NUM_PREDICT),
            temperature: env_parse("AGENT_TEMP")?,
            think: env_parse::<u8>("AGENT_THINK")?.map(|v| v != 0),
            system_file: std::env::var_os("AGENT_SYSTEM_FILE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            slim_output: match slim_env {
                Some(v) => v != 0,
                None => num_ctx <= 32_768,
            },
            keep_alive: std::env::var("AGENT_KEEPALIVE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_KEEP_ALIVE.to_string()),
            budget_env_sec: budget.filter(|b| *b > 0.0),
            unattended: std::env::var("AGENT_UNATTENDED").is_ok_and(|v| v == "1"),
            ablation: std::env::var("AGENT_ABLATION")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            max_iterations: MAX_ITERATIONS,
            child_mode: std::env::var("AGENT_CHILD_SANDBOX").is_ok_and(|v| v == "1"),
            sleep_scale: env_parse::<f32>("AGENT_SLEEP_SCALE")?.unwrap_or(1.0),
            prune_keep: env_parse("AGENT_PRUNE_KEEP")?.unwrap_or(0),
            chat_mode: false,
            interactive: false,
            harness_dir: None,
            profile,
        })
    }

    /// Defaults with no environment lookups (tests, embedding).
    pub fn plain(model: impl Into<String>, workdir: impl Into<PathBuf>) -> Self {
        let model = model.into();
        let profile = crate::profile::builtin_for(&model);
        Self {
            model,
            workdir: workdir.into(),
            num_ctx: DEFAULT_NUM_CTX,
            num_predict: DEFAULT_NUM_PREDICT,
            temperature: None,
            think: None,
            system_file: None,
            slim_output: false,
            keep_alive: DEFAULT_KEEP_ALIVE.to_string(),
            budget_env_sec: None,
            unattended: false,
            ablation: Vec::new(),
            max_iterations: MAX_ITERATIONS,
            child_mode: false,
            sleep_scale: 1.0,
            prune_keep: 0,
            chat_mode: false,
            interactive: false,
            harness_dir: None,
            profile,
        }
    }

    /// Re-resolve the profile with the user's config overrides and let it supply the
    /// `think` / `temperature` defaults the environment (or UI) left unset. Explicit
    /// settings always win over the profile.
    pub fn apply_profile(
        &mut self,
        overrides: &std::collections::BTreeMap<String, crate::profile::ProfileOverride>,
    ) {
        self.profile = crate::profile::resolve(&self.model, overrides);
        if self.think.is_none() {
            self.think = self.profile.think;
        }
        if self.temperature.is_none() {
            self.temperature = self.profile.temperature;
        }
    }

    pub fn ablated(&self, flag: &str) -> bool {
        self.ablation.iter().any(|a| a == flag)
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Result<Option<T>>
where
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse()
            .map(Some)
            .map_err(|e| Error::Config(format!("{name}={v:?}: {e}"))),
        _ => Ok(None),
    }
}

/// Everything the GUI/CLI renders, streamed from the loop.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// A streamed thinking fragment.
    Thinking(String),
    /// A streamed answer fragment.
    Text(String),
    /// A harness protocol line (Mingbird printed these to stdout; `[ctx: …]`, `⚙`, …).
    Log(String),
    ToolCall(ToolCall),
    ToolResult {
        name: String,
        args: Value,
        output: String,
        blocked: bool,
    },
    ContextUsage {
        used: u64,
        limit: u32,
    },
    Compacted {
        level: u8,
        before: usize,
        after: usize,
    },
    /// `===== TASK COMPLETE =====`
    Finished {
        summary: String,
    },
    /// `===== MAX ITERATIONS =====`
    MaxIterations,
    /// Parallel-dispatch progress (`phase`: start / progress / done); Mingbird's
    /// `@@DISPATCH@@` stdout line.
    Dispatch(Value),
    Error(String),
}
