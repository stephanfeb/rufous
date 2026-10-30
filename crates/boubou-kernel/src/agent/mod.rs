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
//! - [`policy`]: the turn policy (what the harness does around the model), with the
//!   `general`, `coding` and `unattended` presets.
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
pub mod policy;
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
pub use policy::{Gate, Gates, TurnPolicy};
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
    /// `AGENT_TEMP`: unset (or not a number) = omit from request.
    pub temperature: Option<f32>,
    /// `AGENT_THINK`: unset = omit; `1`/`true`/`yes`/`on` = on; anything else = off.
    pub think: Option<bool>,
    /// `AGENT_SYSTEM_FILE`: replacement system prompt.
    pub system_file: Option<PathBuf>,
    /// Trim long command output. Resolved by [`RunParams::apply_config`] as Mingbird's
    /// `slim_output_mode`: `AGENT_SLIM_OUTPUT` > `config.json` `slim_output` > automatic
    /// when `num_ctx <= 32K`. Until then, the environment or the automatic rule.
    pub slim_output: bool,
    /// `AGENT_SLIM_OUTPUT` as a switch (`1`/`true`/`yes`/`on`, `0`/`false`/`no`/`off`);
    /// unset or another word = no override.
    pub slim_output_env: Option<bool>,
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
    /// What the harness does around the model this turn ([`TurnPolicy`]): whether text
    /// ends the turn, whether `finish` meets the delivery gates, whether a question is
    /// routed away from the producing tools, whether a plan may be dispatched. The default
    /// is [`TurnPolicy::unattended`], Mingbird's batch harness; the ACP server sets
    /// [`TurnPolicy::coding`], the web UI's chat and `boubou chat` [`TurnPolicy::general`]. A child run
    /// is always unattended. The safety gate, the duplicate-call and failure ladders and
    /// compaction do not depend on the policy.
    pub policy: TurnPolicy,
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
        let slim_env = parse_switch("AGENT_SLIM_OUTPUT", env_raw("AGENT_SLIM_OUTPUT").as_deref());
        let budget: Option<f64> = env_parse("AGENT_TIME_BUDGET_SEC")?;
        let model = model.into();
        let profile = crate::profile::builtin_for(&model);
        Ok(Self {
            model,
            workdir: workdir.into(),
            num_ctx,
            num_predict: env_parse("AGENT_NUMPREDICT")?.unwrap_or(DEFAULT_NUM_PREDICT),
            temperature: parse_temp(env_raw("AGENT_TEMP").as_deref()),
            think: parse_think(env_raw("AGENT_THINK").as_deref()),
            system_file: std::env::var_os("AGENT_SYSTEM_FILE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            slim_output: slim_env.unwrap_or(num_ctx <= 32_768),
            slim_output_env: slim_env,
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
            policy: TurnPolicy::unattended(),
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
            slim_output_env: None,
            keep_alive: DEFAULT_KEEP_ALIVE.to_string(),
            budget_env_sec: None,
            unattended: false,
            ablation: Vec::new(),
            max_iterations: MAX_ITERATIONS,
            child_mode: false,
            sleep_scale: 1.0,
            prune_keep: 0,
            policy: TurnPolicy::unattended(),
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

    /// Everything `config.json` contributes to a run: the model profile
    /// ([`apply_profile`](Self::apply_profile)) and the `slim_output` mode, which the
    /// environment overrides and the context size decides when both are silent (Mingbird's
    /// `slim_output_mode`). The key used to be parsed and never read (review 2026-10-29).
    pub fn apply_config(&mut self, cfg: &crate::config::Config) {
        self.apply_profile(&cfg.profiles);
        self.slim_output = resolve_slim_output(self.slim_output_env, cfg.slim_output, self.num_ctx);
    }

    pub fn ablated(&self, flag: &str) -> bool {
        self.ablation.iter().any(|a| a == flag)
    }
}

/// `slim_output_mode`: environment switch > config `auto`/`on`/`off` > `num_ctx <= 32K`.
pub fn resolve_slim_output(
    env: Option<bool>,
    cfg: crate::config::SlimOutput,
    num_ctx: u32,
) -> bool {
    env.or(cfg.as_bool()).unwrap_or(num_ctx <= 32_768)
}

/// `AGENT_THINK` → tri-state as Mingbird's `_parse_think_env`: unset or empty = engine
/// default; a switch word sets it; any other word is off, with a warning (Mingbird reads
/// everything but `1` as off and says nothing).
pub fn parse_think(raw: Option<&str>) -> Option<bool> {
    let w = raw?.trim();
    if w.is_empty() {
        return None;
    }
    Some(crate::config::truthy(w).unwrap_or_else(|| {
        tracing::warn!(
            "AGENT_THINK={w:?} is not a switch word (1/0, on/off, yes/no, true/false); read as off"
        );
        false
    }))
}

/// `AGENT_TEMP` → a number, or unset when it is not one: a typo in a launch script must
/// not stop the agent (Mingbird's `_parse_temp_env`); it is logged instead.
pub fn parse_temp(raw: Option<&str>) -> Option<f32> {
    let w = raw?.trim();
    if w.is_empty() {
        return None;
    }
    match w.parse::<f32>() {
        Ok(t) if t.is_finite() => Some(t),
        _ => {
            tracing::warn!("AGENT_TEMP={w:?} is not a number; ignored");
            None
        }
    }
}

/// A switch variable: a switch word is the setting, an unknown word is a warning and no
/// setting (the next source decides), unset or empty is no setting.
pub fn parse_switch(name: &str, raw: Option<&str>) -> Option<bool> {
    let w = raw?.trim();
    if w.is_empty() {
        return None;
    }
    let v = crate::config::truthy(w);
    if v.is_none() {
        tracing::warn!(
            "{name}={w:?} is not a switch word (1/0, on/off, yes/no, true/false); ignored"
        );
    }
    v
}

fn env_raw(name: &str) -> Option<String> {
    std::env::var(name).ok()
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
