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
pub mod face;
pub mod history;
pub mod prompt;
pub mod run;
pub mod session;
pub mod testguard;

use std::path::PathBuf;

use serde_json::Value;

use crate::ollama::ToolCall;
use crate::{Error, Result};

pub use chat::{ChatBackend, ChatError, ChatResponse, OllamaBackend, StreamPiece};
pub use face::ToolFace;
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
    /// `AGENT_LANG=en`: force English in the prompts.
    pub lang_en: bool,
    /// `AGENT_UNATTENDED=1`: wording of the platform block (hard refusals, no confirms).
    pub unattended: bool,
    /// `AGENT_ABLATION`: comma-separated switches used by the benchmark harness.
    pub ablation: Vec<String>,
    pub max_iterations: usize,
    /// `AGENT_CHILD_SANDBOX=1`: this process is a dispatched child (ring-0 sandbox,
    /// minimal prompt, narrowed tool face, never Q&A, never dispatches itself).
    pub child_mode: bool,
}

impl RunParams {
    pub fn from_env(model: impl Into<String>, workdir: impl Into<PathBuf>) -> Result<Self> {
        let num_ctx = env_parse("AGENT_CTX")?.unwrap_or(DEFAULT_NUM_CTX);
        let slim_env: Option<u8> = env_parse("AGENT_SLIM_OUTPUT")?;
        let budget: Option<f64> = env_parse("AGENT_TIME_BUDGET_SEC")?;
        Ok(Self {
            model: model.into(),
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
            lang_en: std::env::var("AGENT_LANG").is_ok_and(|v| v == "en"),
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
        })
    }

    /// Defaults with no environment lookups (tests, embedding).
    pub fn plain(model: impl Into<String>, workdir: impl Into<PathBuf>) -> Self {
        Self {
            model: model.into(),
            workdir: workdir.into(),
            num_ctx: DEFAULT_NUM_CTX,
            num_predict: DEFAULT_NUM_PREDICT,
            temperature: None,
            think: None,
            system_file: None,
            slim_output: false,
            keep_alive: DEFAULT_KEEP_ALIVE.to_string(),
            budget_env_sec: None,
            lang_en: false,
            unattended: false,
            ablation: Vec::new(),
            max_iterations: MAX_ITERATIONS,
            child_mode: false,
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
