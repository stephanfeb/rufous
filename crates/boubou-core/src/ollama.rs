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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Ollama ≥0.4 echoes the tool name on `role: tool` messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub function: FunctionCall,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    #[serde(default)]
    pub size: u64,
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

#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(host: impl Into<String>) -> Self {
        let mut base: String = host.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self {
            base,
            http: reqwest::Client::new(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
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
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Ollama(format!("{status}: {body}")));
        }

        let mut stream = resp.bytes_stream();
        let mut dec = NdjsonDecoder::default();
        while let Some(bytes) = stream.next().await {
            dec.push(&bytes?);
            while let Some(chunk) = dec.next_value::<ChatChunk>() {
                let chunk = chunk?;
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
