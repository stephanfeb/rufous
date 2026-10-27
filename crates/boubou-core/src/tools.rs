//! Tool protocol: the JSON schemas sent to the model (byte-for-byte the shape Mingbird
//! sends, so the ~1250-token prefill budget is preserved) and the typed decoding of
//! tool calls coming back.

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::{Error, Result};

/// Names of the nine tools that are always loaded.
pub const CORE_TOOL_NAMES: [&str; 9] = [
    "create_file",
    "read_file",
    "edit_file",
    "list_dir",
    "run_bash",
    "todo",
    "skills",
    "enable_tools",
    "finish",
];

/// Names a model may request through `enable_tools`.
pub const ADVANCED_TOOL_NAMES: [&str; 11] = [
    "run_python",
    "append_file",
    "delete_file",
    "search_files",
    "web_search",
    "web_fetch",
    "web_search_multi",
    "batch_tools",
    "memory_store",
    "memory_recall",
    "mcp_call",
];

/// Equivalent of Python `_f(name, desc, props, req)`: a minimal function schema with no
/// per-parameter descriptions.
fn spec(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {
                "type": "object",
                "properties": props,
                "required": required,
            }
        }
    })
}

fn s() -> Value {
    json!({"type": "string"})
}
fn n() -> Value {
    json!({"type": "number"})
}
fn b() -> Value {
    json!({"type": "boolean"})
}
fn str_array() -> Value {
    json!({"type": "array", "items": {"type": "string"}})
}

pub fn core_tools() -> Vec<Value> {
    vec![
        spec("create_file", "Create or overwrite a file. Overwriting an existing larger file requires replace=true",
            json!({"path": s(), "content": s(), "replace": b()}), &["path", "content"]),
        spec("read_file", "Read a file's content (optional start_line/end_line to read a range)",
            json!({"path": s(), "start_line": n(), "end_line": n()}), &["path"]),
        spec("edit_file", "Find-and-replace text in a file",
            json!({"path": s(), "old": s(), "new": s()}), &["path", "old", "new"]),
        spec("list_dir", "List files in a directory", json!({"path": s()}), &["path"]),
        spec("run_bash", "Run a command (cwd=workspace, no cd /workspace)", json!({"command": s()}), &["command"]),
        spec("todo", "Manage the task plan. action: create(plan), update(item done; index is 1-based), list",
            json!({"action": {"type": "string", "enum": ["create", "update", "list"]}, "items": str_array(), "index": n(), "done": b()}),
            &["action"]),
        spec("skills", "List available skills, or load one by name to follow its instructions",
            json!({"action": {"type": "string", "enum": ["list", "load"]}, "name": s()}), &["action"]),
        spec("enable_tools", "Enable advanced tools on demand: append_file, delete_file, search_files, web_search, web_fetch, memory_store, memory_recall, mcp_call",
            json!({"tools": str_array()}), &["tools"]),
        spec("finish", "Declare the task complete with a short summary", json!({"summary": s()}), &["summary"]),
    ]
}

pub fn advanced_tools() -> Vec<Value> {
    vec![
        spec("run_python", "Run Python code with the bundled interpreter (cwd=workspace; pandas/numpy/PIL available). Use for docs/spreadsheet/image processing instead of bash one-liners",
            json!({"code": s()}), &["code"]),
        spec("append_file", "Append text to the end of a file", json!({"path": s(), "content": s()}), &["path", "content"]),
        spec("delete_file", "Delete a file (moved to .mingbird_trash/, recoverable)", json!({"path": s()}), &["path"]),
        spec("search_files", "Grep-like content search in a directory", json!({"path": s(), "pattern": s()}), &["path", "pattern"]),
        spec("web_search", "Search the web (Bing/Baidu), return titles+links+snippets", json!({"query": s()}), &["query"]),
        spec("web_fetch", "Fetch a URL: full text saved to sources/, return summary+path", json!({"url": s()}), &["url"]),
        spec("web_search_multi", "Parallel search multiple queries at once (fast)",
            json!({"queries": str_array(), "max_results": n()}), &["queries"]),
        spec("batch_tools", "Batch-run multiple tool calls in parallel (fast): [{tool,args},...], merge results. Reduces round-trips.",
            json!({"calls": {"type": "array", "items": {"type": "object"}}}), &["calls"]),
        spec("memory_store", "Save an important fact to long-term memory", json!({"text": s()}), &["text"]),
        spec("memory_recall", "Retrieve facts from memory relevant to a query", json!({"query": s(), "limit": n()}), &["query"]),
        spec("mcp_call", "Call a tool exposed by an MCP server",
            json!({"server": s(), "tool": s(), "args": {"type": "object"}}), &["server", "tool"]),
    ]
}

/// Core tools plus whichever advanced tools have been enabled, in canonical order.
pub fn tool_schemas(enabled: &[String]) -> Vec<Value> {
    let mut out = core_tools();
    for t in advanced_tools() {
        let name = t["function"]["name"].as_str().unwrap_or_default();
        if enabled.iter().any(|e| e == name) {
            out.push(t);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TodoAction {
    Create,
    Update,
    List,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillsAction {
    List,
    Load,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BatchCall {
    pub tool: String,
    #[serde(default)]
    pub args: Map<String, Value>,
}

/// A decoded, argument-checked tool call. Variants carry exactly the arguments the
/// schema declares; anything else the model sends is rejected with a hint it can act on.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolInvocation {
    CreateFile {
        path: String,
        content: String,
        replace: bool,
    },
    ReadFile {
        path: String,
        start_line: Option<u64>,
        end_line: Option<u64>,
    },
    EditFile {
        path: String,
        old: String,
        new: String,
    },
    ListDir {
        path: String,
    },
    RunBash {
        command: String,
    },
    Todo {
        action: TodoAction,
        items: Vec<String>,
        index: Option<u64>,
        done: Option<bool>,
    },
    Skills {
        action: SkillsAction,
        name: Option<String>,
    },
    EnableTools {
        tools: Vec<String>,
    },
    Finish {
        summary: String,
    },
    RunPython {
        code: String,
    },
    AppendFile {
        path: String,
        content: String,
    },
    DeleteFile {
        path: String,
    },
    SearchFiles {
        path: String,
        pattern: String,
    },
    WebSearch {
        query: String,
    },
    WebFetch {
        url: String,
    },
    WebSearchMulti {
        queries: Vec<String>,
        max_results: Option<u64>,
    },
    BatchTools {
        calls: Vec<BatchCall>,
    },
    MemoryStore {
        text: String,
    },
    MemoryRecall {
        query: String,
        limit: Option<u64>,
    },
    McpCall {
        server: String,
        tool: String,
        args: Map<String, Value>,
    },
}

impl ToolInvocation {
    /// Decode `(name, arguments)` as received from the model. Arguments may arrive as a
    /// JSON object or as a string containing JSON (small models do that).
    pub fn parse(name: &str, arguments: &Value) -> Result<Self> {
        let args = coerce_object(name, arguments)?;
        macro_rules! arg {
            ($ty:ty, $key:literal) => {
                decode_arg::<$ty>(&args, name, $key)?
            };
        }
        let inv = match name {
            "create_file" => Self::CreateFile {
                path: arg!(String, "path"),
                content: arg!(String, "content"),
                replace: arg!(Option<bool>, "replace").unwrap_or(false),
            },
            "read_file" => Self::ReadFile {
                path: arg!(String, "path"),
                start_line: arg!(Option<u64>, "start_line"),
                end_line: arg!(Option<u64>, "end_line"),
            },
            "edit_file" => Self::EditFile {
                path: arg!(String, "path"),
                old: arg!(String, "old"),
                new: arg!(String, "new"),
            },
            "list_dir" => Self::ListDir {
                path: arg!(String, "path"),
            },
            "run_bash" => Self::RunBash {
                command: arg!(String, "command"),
            },
            "todo" => Self::Todo {
                action: arg!(TodoAction, "action"),
                items: arg!(Option<Vec<String>>, "items").unwrap_or_default(),
                index: arg!(Option<u64>, "index"),
                done: arg!(Option<bool>, "done"),
            },
            "skills" => Self::Skills {
                action: arg!(SkillsAction, "action"),
                name: arg!(Option<String>, "name"),
            },
            "enable_tools" => Self::EnableTools {
                tools: arg!(Vec<String>, "tools"),
            },
            "finish" => Self::Finish {
                summary: arg!(String, "summary"),
            },
            "run_python" => Self::RunPython {
                code: arg!(String, "code"),
            },
            "append_file" => Self::AppendFile {
                path: arg!(String, "path"),
                content: arg!(String, "content"),
            },
            "delete_file" => Self::DeleteFile {
                path: arg!(String, "path"),
            },
            "search_files" => Self::SearchFiles {
                path: arg!(String, "path"),
                pattern: arg!(String, "pattern"),
            },
            "web_search" => Self::WebSearch {
                query: arg!(String, "query"),
            },
            "web_fetch" => Self::WebFetch {
                url: arg!(String, "url"),
            },
            "web_search_multi" => Self::WebSearchMulti {
                queries: arg!(Vec<String>, "queries"),
                max_results: arg!(Option<u64>, "max_results"),
            },
            "batch_tools" => Self::BatchTools {
                calls: arg!(Vec<BatchCall>, "calls"),
            },
            "memory_store" => Self::MemoryStore {
                text: arg!(String, "text"),
            },
            "memory_recall" => Self::MemoryRecall {
                query: arg!(String, "query"),
                limit: arg!(Option<u64>, "limit"),
            },
            "mcp_call" => Self::McpCall {
                server: arg!(String, "server"),
                tool: arg!(String, "tool"),
                args: arg!(Option<Map<String, Value>>, "args").unwrap_or_default(),
            },
            _ => return Err(Error::UnknownTool(name.to_string())),
        };
        Ok(inv)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::CreateFile { .. } => "create_file",
            Self::ReadFile { .. } => "read_file",
            Self::EditFile { .. } => "edit_file",
            Self::ListDir { .. } => "list_dir",
            Self::RunBash { .. } => "run_bash",
            Self::Todo { .. } => "todo",
            Self::Skills { .. } => "skills",
            Self::EnableTools { .. } => "enable_tools",
            Self::Finish { .. } => "finish",
            Self::RunPython { .. } => "run_python",
            Self::AppendFile { .. } => "append_file",
            Self::DeleteFile { .. } => "delete_file",
            Self::SearchFiles { .. } => "search_files",
            Self::WebSearch { .. } => "web_search",
            Self::WebFetch { .. } => "web_fetch",
            Self::WebSearchMulti { .. } => "web_search_multi",
            Self::BatchTools { .. } => "batch_tools",
            Self::MemoryStore { .. } => "memory_store",
            Self::MemoryRecall { .. } => "memory_recall",
            Self::McpCall { .. } => "mcp_call",
        }
    }

    /// Tools that reach the network; disabled wholesale in offline mode.
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            Self::WebSearch { .. } | Self::WebFetch { .. } | Self::WebSearchMulti { .. }
        )
    }
}

fn coerce_object(tool: &str, v: &Value) -> Result<Map<String, Value>> {
    match v {
        Value::Object(m) => Ok(m.clone()),
        Value::Null => Ok(Map::new()),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(Error::ToolArgs {
                tool: tool.to_string(),
                reason: "arguments must be a JSON object".into(),
            }),
        },
        _ => Err(Error::ToolArgs {
            tool: tool.to_string(),
            reason: "arguments must be a JSON object".into(),
        }),
    }
}

/// Decode one argument. `Option<T>` fields tolerate absence and `null`; required fields
/// produce the same actionable message Mingbird injects (`missing required argument`).
fn decode_arg<T: for<'de> Deserialize<'de> + IsOptional>(
    args: &Map<String, Value>,
    tool: &str,
    key: &str,
) -> Result<T> {
    match args.get(key) {
        None | Some(Value::Null) if T::OPTIONAL => {
            serde_json::from_value(Value::Null).map_err(|e| Error::ToolArgs {
                tool: tool.to_string(),
                reason: format!("{key}: {e}"),
            })
        }
        None | Some(Value::Null) => Err(Error::ToolArgs {
            tool: tool.to_string(),
            reason: format!("missing required argument `{key}`"),
        }),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| Error::ToolArgs {
            tool: tool.to_string(),
            reason: format!("argument `{key}`: {e}"),
        }),
    }
}

/// Marker so `decode_arg` knows whether absence is acceptable.
pub trait IsOptional {
    const OPTIONAL: bool = false;
}
impl<T> IsOptional for Option<T> {
    const OPTIONAL: bool = true;
}
impl IsOptional for String {}
impl IsOptional for bool {}
impl IsOptional for u64 {}
impl IsOptional for TodoAction {}
impl IsOptional for SkillsAction {}
impl<T> IsOptional for Vec<T> {}
impl IsOptional for Map<String, Value> {}
