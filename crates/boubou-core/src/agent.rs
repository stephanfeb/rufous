//! The agent loop (`agent_loop` in `ollama_agent.py`). This file holds the typed
//! surface: run parameters from the environment, events the UI subscribes to, and the
//! executor boundary. The loop body itself is the main porting task.

use std::path::PathBuf;

use crate::ollama::ToolCall;
use crate::tools::ToolInvocation;
use crate::{Error, Result};

pub const DEFAULT_NUM_CTX: u32 = 131_072;
pub const DEFAULT_NUM_PREDICT: i32 = 2048;
/// Context occupancy at which the history is compacted.
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
}

impl RunParams {
    pub fn from_env(model: impl Into<String>, workdir: impl Into<PathBuf>) -> Result<Self> {
        let num_ctx = env_parse("AGENT_CTX")?.unwrap_or(DEFAULT_NUM_CTX);
        let slim_env: Option<u8> = env_parse("AGENT_SLIM_OUTPUT")?;
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
        })
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
    Thinking(String),
    Text(String),
    ToolCall(ToolCall),
    ToolResult {
        name: String,
        output: String,
        blocked: bool,
    },
    TodoUpdated(Vec<TodoItem>),
    ContextUsage {
        used: u64,
        limit: u32,
    },
    Compacted {
        before: usize,
        after: usize,
    },
    Finished {
        summary: String,
    },
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

/// Executes a decoded tool call inside the workspace. Separated from the loop so the
/// safety gates (anti-loop, finish gate, crawl guard, Q&A guard) wrap it uniformly.
pub trait ToolExecutor: Send + Sync {
    fn execute(&mut self, call: &ToolInvocation) -> Result<String>;
}

#[derive(Debug)]
pub struct Agent {
    pub params: RunParams,
}

impl Agent {
    pub fn new(params: RunParams) -> Self {
        Self { params }
    }

    /// Run a task to completion, emitting events. Not yet ported.
    pub async fn run(
        &mut self,
        _task: &str,
        _on_event: &mut dyn FnMut(AgentEvent),
    ) -> Result<String> {
        Err(Error::Unimplemented("agent_loop (ollama_agent.py)"))
    }
}
