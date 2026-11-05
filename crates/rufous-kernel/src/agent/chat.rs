//! `call_chat`: the one place the loop talks to a model. Behind a trait so tests script
//! replies without a network, and so cloud / OpenAI-compatible backends can slot in later.

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::RunParams;
use crate::ollama::{ChatRequest, Client, Message, Options, Role, ToolCall};
use crate::ollama_proc::OllamaProc;
use crate::Error;

/// One streamed fragment, answer text or thinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPiece<'a> {
    Token(&'a str),
    Think(&'a str),
}

/// What `call_chat` returns, normalised across streaming and non-streaming.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub message: Message,
    pub prompt_eval_count: Option<u64>,
    pub eval_count: Option<u64>,
    pub done_reason: Option<String>,
    /// Where the call's time went, when the server says (Ollama does; OpenAI-style servers
    /// do not). Logged per call to tell model loads and full prefills apart (bead boubou-xwi).
    pub timing: Option<Timing>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub load: Duration,
    pub prompt_eval: Duration,
    pub eval: Duration,
}

impl ChatResponse {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            message: Message::assistant(content),
            prompt_eval_count: None,
            eval_count: None,
            done_reason: None,
            timing: None,
        }
    }

    pub fn call(content: impl Into<String>, name: &str, args: Value) -> Self {
        let mut m = Message::assistant_call(name, args);
        m.content = content.into();
        Self {
            message: m,
            prompt_eval_count: None,
            eval_count: None,
            done_reason: None,
            timing: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatError {
    #[error("HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("{0}")]
    Transport(String),
    /// The engine could not be connected to (refused, no route): nothing is listening, as
    /// opposed to a server that failed mid-request (boubou-f8d).
    #[error("not reachable ({0})")]
    Unreachable(String),
    /// No reply within the read timeout; the loop ends the turn instead of resending
    /// (bead boubou-yqu). `mid_reply`: some of the reply had arrived.
    #[error("no reply from the model within the read timeout ({})", if *mid_reply { "mid-reply" } else { "before the first token" })]
    Timeout { mid_reply: bool },
}

impl ChatError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ChatError::Status { status, .. } => Some(*status),
            ChatError::Transport(_) | ChatError::Unreachable(_) | ChatError::Timeout { .. } => None,
        }
    }

    pub fn is_4xx(&self) -> bool {
        self.status().is_some_and(|s| (400..500).contains(&s))
    }

    pub fn is_500(&self) -> bool {
        self.status() == Some(500)
    }

    /// Lower-cased error text (status line plus body) for keyword triage.
    pub fn lower_text(&self) -> String {
        self.to_string().to_lowercase()
    }
}

impl From<Error> for ChatError {
    fn from(e: Error) -> Self {
        match e {
            Error::Status { status, body } => ChatError::Status { status, body },
            Error::Timeout { mid_reply } => ChatError::Timeout { mid_reply },
            Error::Unreachable(why) => ChatError::Unreachable(why),
            other => ChatError::Transport(other.to_string()),
        }
    }
}

pub trait ChatBackend {
    /// One model turn. `tools` is the schema list for this turn (may be empty).
    /// Streaming fragments go to `on_piece`; the full reply comes back.
    fn chat(
        &mut self,
        model: &str,
        messages: &[Message],
        tools: &[Value],
        on_piece: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError>;

    /// A user-stop flag: when set mid-stream the backend abandons the request and returns
    /// a transport error (the loop checks the same flag and ends the run). Default: ignored.
    fn set_cancel(&mut self, _flag: Arc<AtomicBool>) {}

    /// `restart_ollama`: after four consecutive 500s the loop asks the engine to restart.
    /// `None` = this engine cannot be restarted from here (OpenAI-compatible servers, test
    /// backends); `Some(ok)` = a restart was attempted and whether the API came back.
    fn restart_engine(&mut self) -> Option<bool> {
        None
    }
}

/// A backend chosen at run time ([`super::Agent::standard`] picks Ollama, the cloud provider
/// or a local OpenAI-compatible engine from the config).
pub type DynBackend = Box<dyn ChatBackend + Send>;

impl ChatBackend for DynBackend {
    fn chat(
        &mut self,
        model: &str,
        messages: &[Message],
        tools: &[Value],
        on_piece: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        (**self).chat(model, messages, tools, on_piece)
    }
    fn set_cancel(&mut self, flag: Arc<AtomicBool>) {
        (**self).set_cancel(flag)
    }
    fn restart_engine(&mut self) -> Option<bool> {
        (**self).restart_engine()
    }
}

impl<B: ChatBackend + ?Sized> ChatBackend for &mut B {
    fn chat(
        &mut self,
        model: &str,
        messages: &[Message],
        tools: &[Value],
        on_piece: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        (**self).chat(model, messages, tools, on_piece)
    }
    fn set_cancel(&mut self, flag: Arc<AtomicBool>) {
        (**self).set_cancel(flag)
    }
    fn restart_engine(&mut self) -> Option<bool> {
        (**self).restart_engine()
    }
}

/// Text of the error a cancelled stream returns.
pub const CANCELLED: &str = "cancelled by user";

/// Streaming Ollama `/api/chat` backend. Owns a small runtime so the synchronous loop
/// can run on any plain thread.
pub struct OllamaBackend {
    client: Client,
    rt: tokio::runtime::Runtime,
    cancel: Option<Arc<AtomicBool>>,
    /// How to restart this machine's Ollama; `None` = a remote host we do not manage.
    pub proc: Option<OllamaProc>,
    pub num_ctx: u32,
    pub num_predict: i32,
    pub temperature: Option<f32>,
    pub think: Option<bool>,
    pub keep_alive: Option<String>,
}

impl std::fmt::Debug for OllamaBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OllamaBackend")
            .field("base_url", &self.client.base_url())
            .field("num_ctx", &self.num_ctx)
            .field("num_predict", &self.num_predict)
            .field("think", &self.think)
            .finish()
    }
}

impl OllamaBackend {
    pub fn new(host: impl Into<String>, params: &RunParams) -> crate::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Config(format!("tokio runtime: {e}")))?;
        Ok(Self {
            client: Client::new(host),
            rt,
            cancel: None,
            proc: None,
            num_ctx: params.num_ctx,
            num_predict: params.num_predict,
            temperature: params.temperature,
            think: params.think,
            keep_alive: Some(params.keep_alive.clone()),
        })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Make the backend restartable (the loop's 500-degradation self-heal).
    pub fn with_proc(mut self, proc: OllamaProc) -> Self {
        self.proc = Some(proc);
        self
    }

    fn request(&self, model: &str, messages: &[Message], tools: &[Value]) -> ChatRequest {
        ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
            stream: true,
            think: self.think,
            keep_alive: self.keep_alive.clone(),
            options: Options {
                num_ctx: self.num_ctx,
                num_predict: self.num_predict,
                temperature: self.temperature,
            },
        }
    }

    fn stream_once(
        &self,
        req: &ChatRequest,
        on_piece: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        let mut content = String::new();
        let mut thinking = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut prompt_eval = None;
        let mut eval = None;
        let mut done_reason = None;
        let mut timing = None;
        let mut aborted = false;
        self.rt.block_on(self.client.chat_stream(req, |chunk| {
            if self
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                aborted = true;
                return false;
            }
            let m = &chunk.message;
            if !m.content.is_empty() {
                on_piece(StreamPiece::Token(&m.content));
                content.push_str(&m.content);
            }
            if let Some(t) = m.thinking.as_deref().filter(|t| !t.is_empty()) {
                on_piece(StreamPiece::Think(t));
                thinking.push_str(t);
            }
            // Ollama streams each tool call in its own chunk (0.8+, seen on 0.40: two
            // `list_dir` calls arrive as two chunks, `function.index` 0 and 1), so the
            // calls accumulate. Replacing the list with each chunk's, as Mingbird does,
            // kept only the last call of a multi-call turn (review 2026-10-29). A server
            // that resends the cumulative list is tolerated: an identical call is not
            // appended twice.
            for tc in &m.tool_calls {
                if !tool_calls.contains(tc) {
                    tool_calls.push(tc.clone());
                }
            }
            if chunk.prompt_eval_count.is_some() {
                prompt_eval = chunk.prompt_eval_count;
            }
            if chunk.eval_count.is_some() {
                eval = chunk.eval_count;
            }
            if chunk.done_reason.is_some() {
                done_reason = chunk.done_reason.clone();
            }
            if chunk.load_duration.is_some()
                || chunk.prompt_eval_duration.is_some()
                || chunk.eval_duration.is_some()
            {
                let ns = |v: Option<u64>| Duration::from_nanos(v.unwrap_or(0));
                timing = Some(Timing {
                    load: ns(chunk.load_duration),
                    prompt_eval: ns(chunk.prompt_eval_duration),
                    eval: ns(chunk.eval_duration),
                });
            }
            true
        }))?;
        if aborted {
            return Err(ChatError::Transport(CANCELLED.to_string()));
        }
        let mut message = Message::assistant(content);
        message.role = Role::Assistant;
        if !thinking.is_empty() {
            message.thinking = Some(thinking);
        }
        message.tool_calls = tool_calls;
        Ok(ChatResponse {
            message,
            prompt_eval_count: prompt_eval,
            eval_count: eval,
            done_reason,
            timing,
        })
    }
}

impl ChatBackend for OllamaBackend {
    fn chat(
        &mut self,
        model: &str,
        messages: &[Message],
        tools: &[Value],
        on_piece: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        let mut req = self.request(model, messages, tools);
        match self.stream_once(&req, on_piece) {
            // A non-thinking model may reject the `think` field with 400: retry without it
            // (Mingbird does the same, once).
            Err(ChatError::Status { status: 400, .. }) if req.think.is_some() => {
                req.think = None;
                self.stream_once(&req, on_piece)
            }
            other => other,
        }
    }
    fn set_cancel(&mut self, flag: Arc<AtomicBool>) {
        self.cancel = Some(flag);
    }
    fn restart_engine(&mut self) -> Option<bool> {
        self.proc
            .as_ref()
            .map(|p| p.restart(std::time::Duration::from_secs(40)))
    }
}

// ---------------- `_model_can_think` ----------------

static THINK_CAP_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Option<bool>>>,
> = std::sync::LazyLock::new(Default::default);

/// `_model_can_think`: does the model advertise `thinking` in `/api/show` capabilities?
/// `None` = unknown (probe failed, or the engine is OpenAI-compatible and has no
/// `/api/show`). Cached per model for the process. Diagnostics only: the request path
/// sends the tri-state `think` and relies on the 400 retry instead.
pub fn model_can_think(cfg: &crate::config::Config, model: &str) -> Option<bool> {
    if cfg.backend_openai().is_some() {
        return None;
    }
    if let Some(v) = THINK_CAP_CACHE
        .lock()
        .ok()
        .and_then(|c| c.get(model).copied())
    {
        return v;
    }
    let host = cfg.ollama_host();
    let m = model.to_string();
    let v = crate::ollama_proc::block_on_fresh(async move {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .ok()?;
        let r: Value = http
            .post(format!("{}/api/show", host.trim_end_matches('/')))
            .json(&serde_json::json!({"model": m}))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        Some(
            r.get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|c| c.as_str() == Some("thinking"))),
        )
    });
    if let Ok(mut c) = THINK_CAP_CACHE.lock() {
        c.insert(model.to_string(), v);
    }
    v
}
