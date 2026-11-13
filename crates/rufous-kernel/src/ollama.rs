//! Ollama `/api/chat` client with streaming NDJSON.
//!
//! Tri-state options mirror Mingbird: `temperature` and `think` are `Option`s and are
//! omitted from the request when `None`, so the server/model default applies.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<String>,
    pub options: Options,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Options {
    pub num_ctx: u32,
    pub num_predict: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// One conversation message. Deserialization is lenient about `null` in `content`,
/// `tool_calls` and `images` so Mingbird checkpoints and raw Ollama chunks both load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    #[serde(default, deserialize_with = "null_default")]
    pub content: String,
    #[serde(
        default,
        alias = "reasoning_content",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking: Option<String>,
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tool_calls: Vec<ToolCall>,
    /// Ollama ≥0.4 echoes the tool name on `role: tool` messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Base64 images attached for multimodal models (`_attach_pending_images`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub images: Vec<String>,
}

fn null_default<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Option::unwrap_or_default)
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }
    /// An assistant turn that only calls `name` (arguments optional).
    pub fn assistant_call(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            tool_calls: vec![ToolCall {
                function: FunctionCall {
                    name: name.into(),
                    arguments,
                },
                extra_content: None,
            }],
            ..Self::new(Role::Assistant, "")
        }
    }
    /// A tool result without `tool_name`: Mingbird's shape, so checkpoints match.
    pub fn tool_output(content: impl Into<String>) -> Self {
        Self::new(Role::Tool, content)
    }
    pub fn tool(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_name: Some(name.into()),
            ..Self::new(Role::Tool, content)
        }
    }
    fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_name: None,
            images: Vec::new(),
        }
    }

    /// `content` is empty or only whitespace and there is no tool call.
    pub fn is_blank(&self) -> bool {
        self.content.trim().is_empty() && self.tool_calls.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub function: FunctionCall,
    /// Provider data an OpenAI-compatible server attached to this call and wants back
    /// verbatim with it (bead boubou-fdd: Gemini 3's `{"google": {"thought_signature": …}}`,
    /// without which the next request is a 400). Opaque; kept in session files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// One line of the streamed response.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatChunk {
    pub message: Message,
    pub done: bool,
    #[serde(default)]
    pub done_reason: Option<String>,
    #[serde(default)]
    pub prompt_eval_count: Option<u64>,
    #[serde(default)]
    pub eval_count: Option<u64>,
    /// Ollama's done-chunk durations, in nanoseconds.
    #[serde(default)]
    pub load_duration: Option<u64>,
    #[serde(default)]
    pub prompt_eval_duration: Option<u64>,
    #[serde(default)]
    pub eval_duration: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    /// Same digest = same weights under another name (`ollama cp`); the UI dedupes on it.
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<ModelInfo>,
}

#[derive(Debug, Deserialize)]
struct VersionResponse {
    version: String,
}

/// Incremental NDJSON decoder: push bytes, pop complete JSON values.
#[derive(Debug, Default)]
pub struct NdjsonDecoder {
    buf: Vec<u8>,
}

impl NdjsonDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Next complete, non-blank line parsed as `T`; `None` when more bytes are needed.
    pub fn next_value<T: for<'de> Deserialize<'de>>(&mut self) -> Option<Result<T>> {
        loop {
            let nl = self.buf.iter().position(|&b| b == b'\n')?;
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            return Some(serde_json::from_slice(&line).map_err(Error::from));
        }
    }

    /// Flush a trailing line that had no newline (end of stream).
    pub fn finish<T: for<'de> Deserialize<'de>>(&mut self) -> Option<Result<T>> {
        if self.buf.iter().all(u8::is_ascii_whitespace) {
            self.buf.clear();
            return None;
        }
        let line = std::mem::take(&mut self.buf);
        Some(serde_json::from_slice(&line).map_err(Error::from))
    }
}

/// One entry of `/api/ps`: a model resident in memory.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LoadedModel {
    pub model: String,
    pub size: f64,
    pub size_vram: f64,
}

/// Lenient `/api/ps` decoding: `model` or `name`, numbers possibly as strings.
pub fn parse_ps(v: &Value) -> Vec<LoadedModel> {
    fn num(v: Option<&Value>) -> f64 {
        match v {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
            _ => 0.0,
        }
    }
    v.get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .map(|m| LoadedModel {
                    model: m
                        .get("model")
                        .or_else(|| m.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    size: num(m.get("size")),
                    size_vram: num(m.get("size_vram")),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One request to `/v1/systemone`: a state block and named typed questions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemOneRequest {
    pub model: String,
    pub state: String,
    pub questions: std::collections::BTreeMap<String, SystemOneQuestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<String>,
}

/// The three question types llama.cpp-backed decision models answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SystemOneQuestion {
    /// Yes/no: the answer is the probability of "yes".
    Noul { instructions: String },
    /// One of the named options; `criteria` maps option name to its description.
    Choice {
        instructions: String,
        criteria: std::collections::BTreeMap<String, String>,
    },
    /// A level on an ordered scale; `criteria` lists the levels, lowest first.
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SystemOneAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: std::collections::BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        #[serde(default)]
        legend: std::collections::BTreeMap<String, String>,
        probabilities: std::collections::BTreeMap<String, f64>,
        confidence: f64,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemOneResponse {
    #[serde(default)]
    pub model: String,
    pub answers: std::collections::BTreeMap<String, SystemOneAnswer>,
    #[serde(default)]
    pub usage: Option<Value>,
}

/// Default socket timeout, seconds: Mingbird's `API_TIMEOUT_OPEN` (`AGENT_APITIMEOUT`).
pub const DEFAULT_API_TIMEOUT_SECS: u64 = 240;
/// Time allowed to open the TCP connection.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// `AGENT_APITIMEOUT` in seconds, default 240. Zero or unparsable falls back to the default.
pub fn api_timeout() -> std::time::Duration {
    let secs = std::env::var("AGENT_APITIMEOUT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_API_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

/// A read timeout is the model taking longer than `AGENT_APITIMEOUT` to send anything, which
/// the loop ends the turn on; any other transport failure stays an HTTP error, which it
/// retries (bead boubou-yqu).
fn timeout_or_http(e: reqwest::Error, mid_reply: bool) -> Error {
    if e.is_timeout() {
        Error::Timeout { mid_reply }
    } else if e.is_connect() {
        Error::Unreachable(connect_reason(&e))
    } else {
        Error::Http(e)
    }
}

/// Why a connection failed, in the OS's words ("Connection refused (os error 61)"): the
/// innermost cause, since reqwest's own text names only the URL.
pub fn connect_reason(e: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = e;
    while let Some(next) = cause.source() {
        cause = next;
    }
    cause.to_string()
}

impl Client {
    /// A client with the socket semantics of Mingbird's `urlopen(timeout=API_TIMEOUT_OPEN)`:
    /// the connect and every single read are bounded by [`api_timeout`], the whole request is
    /// not, so a slow model that keeps streaming tokens is never cut off, while a server that
    /// goes silent (review 2026-10-29: the loop hung for ever on a stalled `/api/chat`) fails
    /// after `AGENT_APITIMEOUT` seconds and the loop's retry ladder takes over.
    pub fn new(host: impl Into<String>) -> Self {
        Self::with_timeouts(host, CONNECT_TIMEOUT, api_timeout())
    }

    /// `new` with explicit connect and per-read timeouts.
    pub fn with_timeouts(
        host: impl Into<String>,
        connect: std::time::Duration,
        read: std::time::Duration,
    ) -> Self {
        let mut base: String = host.into();
        while base.ends_with('/') {
            base.pop();
        }
        let http = reqwest::Client::builder()
            .connect_timeout(connect)
            .read_timeout(read)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { base, http }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// `POST /v1/systemone` (Ollama ≥ 0.40, models with the `decision` capability): typed
    /// questions over a block of state, answered in one forward pass with probabilities.
    pub async fn systemone(
        &self,
        req: &SystemOneRequest,
        timeout: std::time::Duration,
    ) -> Result<SystemOneResponse> {
        let resp = self
            .http
            .post(format!("{}/v1/systemone", self.base))
            .timeout(timeout)
            .json(req)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Status {
                status: status.as_u16(),
                body: crate::text::head(&body, 300).to_string(),
            });
        }
        Ok(resp.json().await?)
    }

    pub async fn version(&self) -> Result<String> {
        let v: VersionResponse = self
            .http
            .get(format!("{}/api/version", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v.version)
    }

    /// `GET /api/ps`: the models currently resident in memory.
    pub async fn loaded_models(&self) -> Result<Vec<LoadedModel>> {
        let v: Value = self
            .http
            .get(format!("{}/api/ps", self.base))
            .header("User-Agent", "rufous-parallel")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_ps(&v))
    }

    /// `POST /api/show`: the model card (capabilities, details, template…).
    pub async fn show(&self, model: &str) -> Result<Value> {
        let v: Value = self
            .http
            .post(format!("{}/api/show", self.base))
            .json(&serde_json::json!({"model": model}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v)
    }

    /// The `capabilities` list of a model (`completion`, `tools`, `thinking`, `vision`…).
    pub async fn capabilities(&self, model: &str) -> Result<Vec<String>> {
        let v = self.show(model).await?;
        Ok(v.get("capabilities")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let t: TagsResponse = self
            .http
            .get(format!("{}/api/tags", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(t.models)
    }

    /// Stream a chat completion, invoking `on_chunk` per NDJSON line until `done` or
    /// the callback returns `false`. Returns the final chunk when the stream completed.
    pub async fn chat_stream<F>(
        &self,
        req: &ChatRequest,
        mut on_chunk: F,
    ) -> Result<Option<ChatChunk>>
    where
        F: FnMut(&ChatChunk) -> bool,
    {
        use futures_util::StreamExt;

        let resp = self
            .http
            .post(format!("{}/api/chat", self.base))
            .json(req)
            .send()
            .await
            .map_err(|e| timeout_or_http(e, false))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Status { status, body });
        }

        let mut stream = resp.bytes_stream();
        let mut dec = NdjsonDecoder::default();
        let mut chunks = false;
        while let Some(bytes) = stream.next().await {
            dec.push(&bytes.map_err(|e| timeout_or_http(e, chunks))?);
            while let Some(chunk) = dec.next_value::<ChatChunk>() {
                let chunk = chunk?;
                chunks = true;
                let go_on = on_chunk(&chunk);
                if chunk.done || !go_on {
                    return Ok(Some(chunk));
                }
            }
        }
        match dec.finish::<ChatChunk>() {
            Some(chunk) => {
                let chunk = chunk?;
                on_chunk(&chunk);
                Ok(Some(chunk))
            }
            None => Ok(None),
        }
    }
}
