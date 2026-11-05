//! OpenAI-compatible chat engines (`_call_cloud` / `_call_openai_local` in Mingbird).
//!
//! Two deployments share one wire format but differ in measured ways Mingbird insists on
//! keeping apart (Mingbird's note: "all measured differences, do not unify"):
//!
//! * **cloud** (`config.cloud`): non-streaming, `Authorization: Bearer` always, and
//!   `enable_thinking: false` (thinking off by default, like the local protocol);
//! * **local engine** (`config.local_backend = {"type":"openai"}`: LM Studio, llama.app,
//!   llama-server, Jan, vLLM): true SSE streaming, Bearer only when a key is set (an empty
//!   Bearer gets a 401 from strict servers), no `enable_thinking` (the server's reasoning
//!   parser decides; inline `<think>` is split out here as a fallback).
//!
//! Ollama-only fields (`keep_alive`, `num_ctx`, tri-state `think`) are never sent; the context
//! budget is still enforced on the harness side by `AGENT_CTX`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};

use super::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece, CANCELLED};
use super::RunParams;
use crate::ollama::{FunctionCall, Message, Role, ToolCall};
use crate::Error;

/// Request timeout for one completion (Mingbird: 900 s).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(900);

// ---------------- message conversion ----------------

/// `_cloud_to_openai_messages`: Ollama-shaped history → OpenAI shape. Tool results are
/// paired with the preceding assistant's `tool_calls` in order (FIFO), since Ollama tool
/// messages carry no `tool_call_id`.
pub fn to_openai_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    let mut pending: std::collections::VecDeque<String> = Default::default();
    for m in messages {
        match m.role {
            Role::System => out.push(json!({"role": "system", "content": m.content})),
            Role::User => out.push(json!({"role": "user", "content": m.content})),
            Role::Assistant => {
                let mut om = Map::new();
                om.insert("role".into(), json!("assistant"));
                om.insert(
                    "content".into(),
                    if m.content.is_empty() {
                        Value::Null
                    } else {
                        Value::String(m.content.clone())
                    },
                );
                if !m.tool_calls.is_empty() {
                    let mut otcs = Vec::new();
                    for (i, tc) in m.tool_calls.iter().enumerate() {
                        let args = match &tc.function.arguments {
                            Value::String(s) => s.clone(),
                            Value::Null => "{}".to_string(),
                            other => other.to_string(),
                        };
                        let cid = format!("call_{}_{i}", out.len());
                        pending.push_back(cid.clone());
                        otcs.push(json!({
                            "id": cid,
                            "type": "function",
                            "function": {"name": tc.function.name, "arguments": args},
                        }));
                    }
                    om.insert("tool_calls".into(), Value::Array(otcs));
                }
                out.push(Value::Object(om));
            }
            Role::Tool => {
                let cid = pending.pop_front().unwrap_or_else(|| "call_0".to_string());
                out.push(json!({"role": "tool", "tool_call_id": cid, "content": m.content}));
            }
        }
    }
    out
}

// ---------------- inline <think> handling ----------------

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// `_split_inline_thinking` (non-streaming): peel `<think>…</think>` out of `content`.
/// An unclosed trailing `<think>` counts as thinking (the shape when `max_tokens` cut the
/// output). Returns `(body, thinking)`.
pub fn split_inline_thinking(content: &str) -> (String, String) {
    if content.is_empty() || !content.contains(OPEN) {
        return (content.to_string(), String::new());
    }
    let mut body = String::new();
    let mut think = String::new();
    let mut rest = content;
    loop {
        match rest.find(OPEN) {
            None => {
                body.push_str(rest);
                break;
            }
            Some(i) => {
                body.push_str(&rest[..i]);
                let after = &rest[i + OPEN.len()..];
                match after.find(CLOSE) {
                    Some(j) => {
                        think.push_str(&after[..j]);
                        rest = &after[j + CLOSE.len()..];
                    }
                    None => {
                        think.push_str(after);
                        break;
                    }
                }
            }
        }
    }
    (body.trim().to_string(), think.trim().to_string())
}

/// `_InlineThinkSplitter` (streaming): routes deltas to body or thinking, holding back a
/// tail that could be the start of a torn tag (`"<th" + "ink>"`) until the next chunk.
#[derive(Debug, Default)]
pub struct InlineThinkSplitter {
    buf: String,
    in_think: bool,
}

impl InlineThinkSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many trailing chars of `buf` could be the prefix of `tag` (must be held back).
    fn hold(buf: &str, tag: &str) -> usize {
        let max = (tag.len() - 1).min(buf.len());
        for k in (1..=max).rev() {
            if !buf.is_char_boundary(buf.len() - k) {
                continue;
            }
            if tag.starts_with(&buf[buf.len() - k..]) {
                return k;
            }
        }
        0
    }

    /// Feed one delta; returns `(body delta, thinking delta)`.
    pub fn feed(&mut self, s: &str) -> (String, String) {
        self.buf.push_str(s);
        let mut body = String::new();
        let mut think = String::new();
        loop {
            let tag = if self.in_think { CLOSE } else { OPEN };
            if let Some(i) = self.buf.find(tag) {
                let seg = self.buf[..i].to_string();
                if self.in_think {
                    think.push_str(&seg);
                } else {
                    body.push_str(&seg);
                }
                self.buf = self.buf[i + tag.len()..].to_string();
                self.in_think = !self.in_think;
                continue;
            }
            let h = Self::hold(&self.buf, tag);
            let cut = self.buf.len() - h;
            let seg = self.buf[..cut].to_string();
            if self.in_think {
                think.push_str(&seg);
            } else {
                body.push_str(&seg);
            }
            self.buf = if h > 0 {
                self.buf[cut..].to_string()
            } else {
                String::new()
            };
            return (body, think);
        }
    }

    /// End of stream: an unclosed thought is thinking; a held tail that never became a tag
    /// goes back to the body (it was text after all).
    pub fn finish(&mut self) -> (String, String) {
        let rest = std::mem::take(&mut self.buf);
        if self.in_think {
            self.in_think = false;
            (String::new(), rest)
        } else {
            (rest, String::new())
        }
    }
}

// ---------------- response normalisation ----------------

/// `_openai_tc_args_from_str`: a JSON-string argument blob → object; unparsable text is
/// kept as `{"_raw": …}` so the tool side can give a correctable error.
pub fn tc_args_from_str(s: &str) -> Value {
    if s.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({"_raw": s}))
}

fn tool_calls_from(v: Option<&Value>) -> Vec<ToolCall> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|tc| {
                    let f = tc.get("function").cloned().unwrap_or(Value::Null);
                    let name = f
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let arguments = match f.get("arguments") {
                        Some(Value::String(s)) => tc_args_from_str(s),
                        Some(Value::Object(o)) => Value::Object(o.clone()),
                        _ => json!({}),
                    };
                    ToolCall {
                        function: FunctionCall { name, arguments },
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `_openai_normalize_message`: an OpenAI `message` → Ollama-shaped assistant message.
/// Field-form reasoning (`reasoning_content` / `reasoning`) comes first, inline `<think>`
/// text after it; tool arguments accept string, object, or broken JSON.
pub fn normalize_message(msg: &Value) -> Message {
    let content = msg.get("content").and_then(Value::as_str).unwrap_or("");
    let rc = msg
        .get("reasoning_content")
        .or_else(|| msg.get("reasoning"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let (body, inline) = split_inline_thinking(content);
    let mut m = Message::assistant(body);
    let th = format!("{rc}{inline}").trim().to_string();
    if !th.is_empty() {
        m.thinking = Some(th);
    }
    m.tool_calls = tool_calls_from(msg.get("tool_calls"));
    m
}

/// A whole (non-streaming) `chat.completions` body → [`ChatResponse`].
pub fn parse_completion(d: &Value) -> ChatResponse {
    let ch0 = d
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Null);
    let message = normalize_message(ch0.get("message").unwrap_or(&Value::Null));
    let usage = d.get("usage").cloned().unwrap_or(Value::Null);
    let done_reason = ch0
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            Some(if message.tool_calls.is_empty() {
                "stop".to_string()
            } else {
                "tool_calls".to_string()
            })
        });
    ChatResponse {
        message,
        prompt_eval_count: usage.get("prompt_tokens").and_then(Value::as_u64),
        eval_count: usage.get("completion_tokens").and_then(Value::as_u64),
        done_reason,
        timing: None,
    }
}

// ---------------- SSE assembly ----------------

#[derive(Debug, Default, Clone)]
struct TcAcc {
    id: Option<String>,
    name: String,
    args: String,
}

/// Assembles an OpenAI SSE stream: body/thinking deltas are routed through the inline
/// splitter, tool-call fragments are joined by `index` (name and argument string may both
/// be torn across deltas), `usage` is merged, `[DONE]` ends the stream.
#[derive(Debug, Default)]
pub struct SseAssembler {
    splitter: InlineThinkSplitter,
    body: String,
    think: String,
    tcs: std::collections::BTreeMap<u64, TcAcc>,
    usage: Map<String, Value>,
    finish_reason: Option<String>,
    pending: Vec<u8>,
    pub done: bool,
}

impl SseAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push raw bytes; complete `data:` lines are consumed, deltas go to `on_piece`.
    pub fn push(&mut self, bytes: &[u8], on_piece: &mut dyn FnMut(StreamPiece<'_>)) {
        self.pending.extend_from_slice(bytes);
        while let Some(i) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line).trim().to_string();
            self.line(&line, on_piece);
        }
    }

    /// One SSE line (`data: {...}` / `data: [DONE]`; anything else ignored).
    pub fn line(&mut self, line: &str, on_piece: &mut dyn FnMut(StreamPiece<'_>)) {
        let Some(payload) = line.strip_prefix("data:") else {
            return;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            self.done = true;
            return;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(payload) else {
            return;
        };
        if let Some(u) = chunk.get("usage").and_then(Value::as_object) {
            for (k, v) in u {
                self.usage.insert(k.clone(), v.clone());
            }
        }
        for ch in chunk
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(fr) = ch.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(fr.to_string());
            }
            let delta = ch.get("delta").cloned().unwrap_or(Value::Null);
            if let Some(c) = delta.get("content").and_then(Value::as_str) {
                if !c.is_empty() {
                    let (b, t) = self.splitter.feed(c);
                    if !b.is_empty() {
                        on_piece(StreamPiece::Token(&b));
                        self.body.push_str(&b);
                    }
                    if !t.is_empty() {
                        on_piece(StreamPiece::Think(&t));
                        self.think.push_str(&t);
                    }
                }
            }
            if let Some(rc) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
            {
                if !rc.is_empty() {
                    on_piece(StreamPiece::Think(rc));
                    self.think.push_str(rc);
                }
            }
            for dtc in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let idx = dtc.get("index").and_then(Value::as_u64).unwrap_or(0);
                let acc = self.tcs.entry(idx).or_default();
                if let Some(id) = dtc.get("id").and_then(Value::as_str) {
                    acc.id = Some(id.to_string());
                }
                if let Some(f) = dtc.get("function") {
                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                        acc.name.push_str(n);
                    }
                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                        acc.args.push_str(a);
                    }
                }
            }
        }
    }

    /// End of stream → the assembled response.
    pub fn finish(mut self, on_piece: &mut dyn FnMut(StreamPiece<'_>)) -> ChatResponse {
        let (b, t) = self.splitter.finish();
        if !b.is_empty() {
            on_piece(StreamPiece::Token(&b));
            self.body.push_str(&b);
        }
        if !t.is_empty() {
            on_piece(StreamPiece::Think(&t));
            self.think.push_str(&t);
        }
        let mut message = Message::assistant(self.body);
        if !self.think.is_empty() {
            message.thinking = Some(self.think);
        }
        message.tool_calls = self
            .tcs
            .values()
            .map(|a| ToolCall {
                function: FunctionCall {
                    name: a.name.clone(),
                    arguments: tc_args_from_str(&a.args),
                },
            })
            .collect();
        let done_reason = self.finish_reason.or_else(|| {
            Some(if message.tool_calls.is_empty() {
                "stop".to_string()
            } else {
                "tool_calls".to_string()
            })
        });
        ChatResponse {
            message,
            prompt_eval_count: self.usage.get("prompt_tokens").and_then(Value::as_u64),
            eval_count: self.usage.get("completion_tokens").and_then(Value::as_u64),
            done_reason,
            timing: None,
        }
    }
}

// ---------------- the backend ----------------

/// Which measured protocol variant to speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// `config.cloud`: non-streaming, Bearer always, `enable_thinking: false`.
    Cloud,
    /// `config.local_backend`: SSE streaming, Bearer only with a key.
    LocalEngine,
}

/// The request body for one turn (pure; tested without a server).
#[derive(Debug, Clone, PartialEq)]
pub struct RequestShape {
    pub model: String,
    pub stream: bool,
    /// `num_predict > 0` → `max_tokens`; `-1`/`0` → omitted (no negative semantics here).
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub enable_thinking: Option<bool>,
}

impl RequestShape {
    pub fn body(&self, messages: &[Message], tools: &[Value]) -> Value {
        let mut req = Map::new();
        req.insert("model".into(), json!(self.model));
        req.insert(
            "messages".into(),
            Value::Array(to_openai_messages(messages)),
        );
        req.insert("stream".into(), json!(self.stream));
        if let Some(et) = self.enable_thinking {
            req.insert("enable_thinking".into(), json!(et));
        }
        if let Some(mt) = self.max_tokens {
            req.insert("max_tokens".into(), json!(mt));
        }
        if let Some(t) = self.temperature {
            req.insert("temperature".into(), json!(t));
        }
        if !tools.is_empty() {
            req.insert("tools".into(), Value::Array(tools.to_vec()));
            req.insert("tool_choice".into(), json!("auto"));
        }
        Value::Object(req)
    }
}

/// OpenAI-compatible `chat.completions` backend. Owns a small runtime like
/// [`super::OllamaBackend`] so the synchronous loop can run on a plain thread.
pub struct OpenAiBackend {
    pub base_url: String,
    pub api_key: String,
    pub flavor: Flavor,
    /// Cloud: the configured model replaces whatever the loop asks for.
    pub model_override: Option<String>,
    pub num_predict: i32,
    pub temperature: Option<f32>,
    http: reqwest::Client,
    rt: tokio::runtime::Runtime,
    cancel: Option<Arc<AtomicBool>>,
}

impl std::fmt::Debug for OpenAiBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiBackend")
            .field("base_url", &self.base_url)
            .field("flavor", &self.flavor)
            .field("model_override", &self.model_override)
            .field("has_key", &!self.api_key.is_empty())
            .finish()
    }
}

impl OpenAiBackend {
    fn build(
        base_url: &str,
        api_key: &str,
        flavor: Flavor,
        model_override: Option<String>,
        params: &RunParams,
    ) -> crate::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Config(format!("tokio runtime: {e}")))?;
        // Built inside the runtime: the client's timer needs a reactor at construction.
        let http = rt
            .block_on(async { reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() })
            .map_err(Error::Http)?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            flavor,
            model_override,
            num_predict: params.num_predict,
            temperature: params.temperature,
            http,
            rt,
            cancel: None,
        })
    }

    /// `_call_cloud`: the configured cloud provider.
    pub fn cloud(cp: &crate::config::CloudProvider, params: &RunParams) -> crate::Result<Self> {
        Self::build(
            &cp.base_url,
            &cp.api_key,
            Flavor::Cloud,
            Some(cp.model.clone()),
            params,
        )
    }

    /// `_call_openai_local`: LM Studio / llama.app / llama-server / Jan / vLLM.
    pub fn local(eng: &crate::config::OpenAiEngine, params: &RunParams) -> crate::Result<Self> {
        Self::build(
            &eng.base_url,
            &eng.api_key,
            Flavor::LocalEngine,
            None,
            params,
        )
    }

    /// The request shape for `model` under this backend's flavor.
    pub fn shape(&self, model: &str) -> RequestShape {
        RequestShape {
            model: self
                .model_override
                .clone()
                .unwrap_or_else(|| model.to_string()),
            stream: self.flavor == Flavor::LocalEngine,
            max_tokens: (self.num_predict > 0).then_some(self.num_predict as u32),
            temperature: self.temperature,
            enable_thinking: (self.flavor == Flavor::Cloud).then_some(false),
        }
    }

    /// Bearer header policy: cloud always (even empty, as Mingbird does), local only with a key.
    pub fn auth_header(&self) -> Option<String> {
        match self.flavor {
            Flavor::Cloud => Some(format!("Bearer {}", self.api_key)),
            Flavor::LocalEngine if !self.api_key.is_empty() => {
                Some(format!("Bearer {}", self.api_key))
            }
            Flavor::LocalEngine => None,
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    fn post(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .header("Content-Type", "application/json")
            .json(body);
        if let Some(h) = self.auth_header() {
            rb = rb.header("Authorization", h);
        }
        rb
    }

    fn once(
        &self,
        body: &Value,
        on_piece: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        // The request future (and reqwest's timeout timer) must be created inside the runtime.
        let resp = self
            .rt
            .block_on(async { self.post(body).send().await })
            .map_err(send_error)?;
        let status = resp.status();
        if !status.is_success() {
            let text = self.rt.block_on(resp.text()).unwrap_or_default();
            return Err(ChatError::Status {
                status: status.as_u16(),
                body: text,
            });
        }
        let d: Value = self
            .rt
            .block_on(resp.json())
            .map_err(|e| ChatError::Transport(e.to_string()))?;
        let r = parse_completion(&d);
        // The callbacks fire once, after the fact (same wiring as the local stream path).
        if !r.message.content.is_empty() {
            on_piece(StreamPiece::Token(&r.message.content));
        }
        if let Some(t) = r.message.thinking.as_deref().filter(|t| !t.is_empty()) {
            on_piece(StreamPiece::Think(t));
        }
        Ok(r)
    }

    fn stream(
        &self,
        body: &Value,
        on_piece: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        use futures_util::StreamExt;
        // The request future (and reqwest's timeout timer) must be created inside the runtime.
        let resp = self
            .rt
            .block_on(async { self.post(body).send().await })
            .map_err(send_error)?;
        let status = resp.status();
        if !status.is_success() {
            let text = self.rt.block_on(resp.text()).unwrap_or_default();
            return Err(ChatError::Status {
                status: status.as_u16(),
                body: text,
            });
        }
        let mut asm = SseAssembler::new();
        let mut aborted = false;
        self.rt.block_on(async {
            let mut s = resp.bytes_stream();
            while let Some(b) = s.next().await {
                if self.cancelled() {
                    aborted = true;
                    break;
                }
                match b {
                    Ok(bytes) => asm.push(&bytes, on_piece),
                    Err(e) => return Err(ChatError::Transport(e.to_string())),
                }
                if asm.done {
                    break;
                }
            }
            Ok(())
        })?;
        if aborted {
            return Err(ChatError::Transport(CANCELLED.to_string()));
        }
        Ok(asm.finish(on_piece))
    }
}

impl ChatBackend for OpenAiBackend {
    fn chat(
        &mut self,
        model: &str,
        messages: &[Message],
        tools: &[Value],
        on_piece: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        let shape = self.shape(model);
        let body = shape.body(messages, tools);
        if shape.stream {
            self.stream(&body, on_piece)
        } else {
            self.once(&body, on_piece)
        }
    }
    fn set_cancel(&mut self, flag: Arc<AtomicBool>) {
        self.cancel = Some(flag);
    }
    // `restart_engine`: default `None` — we have no right to restart someone else's server.
}

// ---------------- probes (async; the web server and the CLI call them) ----------------

/// `probe_openai_backend`: `GET {base}/models` answers → the engine is up.
pub async fn probe(base_url: &str, api_key: &str, timeout: Duration) -> bool {
    list_models(base_url, api_key, timeout).await.is_ok()
}

/// `GET {base}/models` → model ids (OpenAI `{"data":[{"id":…}]}` shape).
pub async fn list_models(
    base_url: &str,
    api_key: &str,
    timeout: Duration,
) -> crate::Result<Vec<String>> {
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(Error::Http)?;
    let mut rb = http.get(format!("{}/models", base_url.trim_end_matches('/')));
    if !api_key.is_empty() {
        rb = rb.header("Authorization", format!("Bearer {api_key}"));
    }
    let v: Value = rb.send().await?.error_for_status()?.json().await?;
    Ok(v.get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// Blocking wrapper for [`probe`] (CLI start-up check).
/// A request that never got a reply: `Unreachable` when nothing could be connected to,
/// else a transport error the loop retries.
fn send_error(e: reqwest::Error) -> ChatError {
    if e.is_connect() {
        ChatError::Unreachable(crate::ollama::connect_reason(&e))
    } else {
        ChatError::Transport(e.to_string())
    }
}

pub fn probe_blocking(base_url: &str, api_key: &str, timeout: Duration) -> bool {
    crate::ollama_proc::block_on_fresh(probe(base_url, api_key, timeout))
}
