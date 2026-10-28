//! The tool face: which tool schemas the model sees on a given turn.
//!
//! Mingbird keeps this in module globals (`_active_tools`, `_disabled_tools`) that
//! `tools_for_categories` rebuilds every turn from category routing plus whatever the model
//! asked for through `enable_tools`. Disabled tools are filtered at assembly *and* refused at
//! call time, otherwise the per-turn rebuild would quietly resurrect them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use serde_json::Value;

use crate::mcp::{CAT_CODE, CAT_FILES, CAT_MCP, CAT_MEMORY, CAT_WEB};
use crate::tools::{advanced_tools, core_tools, CORE_TOOL_NAMES};

/// Tools every task gets regardless of category.
pub const BASE_TOOLS: [&str; 6] = [
    "read_file",
    "list_dir",
    "todo",
    "skills",
    "enable_tools",
    "finish",
];

/// Category → tools loaded only for tasks routed to it (`run_python` dropped in Boubou).
pub const CATEGORY_TOOLS: [(&str, &[&str]); 5] = [
    (
        CAT_FILES,
        &[
            "create_file",
            "edit_file",
            "append_file",
            "delete_file",
            "search_files",
        ],
    ),
    (CAT_CODE, &["run_bash"]),
    (
        CAT_WEB,
        &["web_search", "web_fetch", "web_search_multi", "batch_tools"],
    ),
    (CAT_MEMORY, &["memory_store", "memory_recall"]),
    (CAT_MCP, &["mcp_call"]),
];

/// Keyword routing. Matched case-insensitively as substrings of the task text, so a
/// keyword also matches its inflections (`test` hits `testing`, `tests`).
const CAT_KEYWORDS: [(&str, &[&str]); 5] = [
    (
        CAT_CODE,
        &[
            "code",
            "program",
            "script",
            "run ",
            "bash",
            "python",
            "compile",
            "function",
            "module",
            "fix",
            "refactor",
            "debug",
            "implement",
            "test",
            "execute",
            "algorithm",
            "compute",
            "calculate",
            "error",
            "traceback",
            "exception",
        ],
    ),
    (
        CAT_FILES,
        &[
            "create",
            "make ",
            "generate",
            "document",
            "file",
            "folder",
            "directory",
            "tidy",
            "organis",
            "organiz",
            "rename",
            "delete",
            "remove",
            "edit",
            "write",
            "save",
            "report",
            "markdown",
            "csv",
            "json",
        ],
    ),
    (
        CAT_WEB,
        &[
            "search", "look up", "research", "web", "internet", "online", "website", "page", "url",
            "http", "news", "weather", "download", "fetch", "latest",
        ],
    ),
    (
        CAT_MEMORY,
        &[
            "remember",
            "recall",
            "memory",
            "memorise",
            "memorize",
            "archive",
            "earlier",
            "last time",
            "previously",
        ],
    ),
    (
        CAT_MCP,
        &[
            "mcp",
            "external tool",
            "tool server",
            "plugin",
            "server",
            "api",
            "database",
        ],
    ),
];

/// Tools that reach the network; hidden in offline mode.
pub const ONLINE_TOOLS: [&str; 4] = ["web_search", "web_fetch", "web_search_multi", "batch_tools"];
/// Q&A mode sees only read-only tools (`_chat_tool_defs`).
pub const CHAT_TOOLS: [&str; 4] = ["read_file", "list_dir", "web_search", "web_fetch"];
/// `_RESEARCH_ALL`: research tools covered by the fingerprint-level streak guard.
pub const RESEARCH_ALL: [&str; 3] = ["web_search", "web_fetch", "web_search_multi"];
/// `_PRODUCTIVE`: tools that count as making progress (todo/research counters reset).
pub const PRODUCTIVE: [&str; 5] = [
    "create_file",
    "edit_file",
    "append_file",
    "finish",
    "run_bash",
];
/// `_MUTATE`: tools the Q&A guard intercepts.
pub const MUTATE: [&str; 5] = [
    "create_file",
    "edit_file",
    "append_file",
    "delete_file",
    "run_bash",
];
/// `_PRODUCING_PREFIX`: an MCP tool whose name starts with one of these did real work.
pub const PRODUCING_PREFIX: [&str; 12] = [
    "create_",
    "write_",
    "render_",
    "export_",
    "update_",
    "save_",
    "generate_",
    "build_",
    "insert_",
    "convert_",
    "import_",
    "add_",
];

/// `_is_mcp_producing`: `server.tool` whose tool part looks like it writes something.
pub fn is_mcp_producing(name: &str) -> bool {
    let tool = name.split_once('.').map(|(_, t)| t).unwrap_or("");
    PRODUCING_PREFIX.iter().any(|p| tool.starts_with(p))
}

/// `route_categories`: task text → categories. Code implies files; nothing matched means
/// the wide default (files + code + web) — better too many tools than too few.
pub fn route_categories(text: &str) -> BTreeSet<String> {
    let low = text.to_lowercase();
    let mut cats = BTreeSet::new();
    for (cat, kws) in CAT_KEYWORDS {
        if kws.iter().any(|k| low.contains(k)) {
            cats.insert(cat.to_string());
        }
    }
    if cats.contains(CAT_CODE) {
        cats.insert(CAT_FILES.to_string());
    }
    if cats.is_empty() {
        for c in [CAT_FILES, CAT_CODE, CAT_WEB] {
            cats.insert(c.to_string());
        }
    }
    cats
}

fn schema_map() -> &'static BTreeMap<String, Value> {
    static MAP: LazyLock<BTreeMap<String, Value>> = LazyLock::new(|| {
        core_tools()
            .into_iter()
            .chain(advanced_tools())
            .filter_map(|t| {
                let name = t["function"]["name"].as_str()?.to_string();
                Some((name, t))
            })
            .collect()
    });
    &MAP
}

/// Schema of one built-in tool by name.
pub fn schema_of(name: &str) -> Option<&'static Value> {
    schema_map().get(name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFace {
    /// `_active_tools`, in enable order (core tools first).
    pub active: Vec<String>,
    /// `_disabled_tools`: refused at call time and never re-assembled.
    pub disabled: BTreeSet<String>,
    /// Names the model asked for through `enable_tools` (`allowed_extra`).
    pub extra: BTreeSet<String>,
    /// Offline mode hides the online tools from every schema list.
    pub offline: bool,
}

impl Default for ToolFace {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolFace {
    pub fn new() -> Self {
        Self {
            active: CORE_TOOL_NAMES.iter().map(|s| s.to_string()).collect(),
            disabled: BTreeSet::new(),
            extra: BTreeSet::new(),
            offline: false,
        }
    }

    pub fn is_active(&self, name: &str) -> bool {
        self.active.iter().any(|n| n == name)
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.contains(name)
    }

    /// Child mode (`install_child_mode`): only `allowed` stays; every other built-in tool is
    /// disabled at assembly and refused at call time.
    pub fn narrow_to(&mut self, allowed: &[String]) {
        let keep = |n: &str| allowed.iter().any(|a| a == n);
        self.disabled = crate::tools::CORE_TOOL_NAMES
            .iter()
            .chain(crate::tools::ADVANCED_TOOL_NAMES.iter())
            .filter(|n| !keep(n))
            .map(|n| n.to_string())
            .collect();
        self.active.retain(|n| keep(n));
        self.extra.clear();
    }

    /// Persistently disable a tool (anti-loop escalation).
    pub fn disable(&mut self, name: &str) {
        self.disabled.insert(name.to_string());
        self.active.retain(|n| n != name);
    }

    /// `enable_tools` by the model: request it for the next assembly and, as Mingbird
    /// promises in its ban message, unlock a disabled tool.
    pub fn enable(&mut self, name: &str) {
        self.extra.insert(name.to_string());
        self.disabled.remove(name);
    }

    fn filter_offline(&self, defs: Vec<Value>) -> Vec<Value> {
        if !self.offline {
            return defs;
        }
        defs.into_iter()
            .filter(|t| {
                let n = t["function"]["name"].as_str().unwrap_or("");
                !ONLINE_TOOLS.contains(&n)
            })
            .collect()
    }

    /// `_chat_tool_defs`: read-only tools for Q&A turns. Whatever is offered here is also
    /// activated: the call-time gate refuses any tool missing from `active`, and a question
    /// turn that listed web_search but refused the call was how the model came to tell the
    /// user it "cannot browse" (web UI session of 2026-10-28).
    pub fn chat_tool_defs(&mut self) -> Vec<Value> {
        for n in CHAT_TOOLS {
            if !self.disabled.contains(n) && !self.is_active(n) {
                self.active.push(n.to_string());
            }
        }
        let defs = CHAT_TOOLS
            .iter()
            .filter(|n| !self.disabled.contains(**n))
            .filter_map(|n| schema_of(n).cloned())
            .collect();
        self.filter_offline(defs)
    }

    /// `tools_for_categories`: base tools + the routed categories + `extra`, minus
    /// disabled. Newly needed tools are appended to `active`; the result keeps active order.
    pub fn tools_for_categories(&mut self, cats: &BTreeSet<String>) -> Vec<Value> {
        let mut names: BTreeSet<String> = BASE_TOOLS.iter().map(|s| s.to_string()).collect();
        for (cat, tools) in CATEGORY_TOOLS {
            if cats.contains(cat) {
                names.extend(tools.iter().map(|s| s.to_string()));
            }
        }
        names.extend(self.extra.iter().cloned());
        for d in &self.disabled {
            names.remove(d);
        }
        for n in &names {
            if schema_of(n).is_some() && !self.is_active(n) {
                self.active.push(n.clone());
            }
        }
        let defs = self
            .active
            .iter()
            .filter(|n| names.contains(*n))
            .filter_map(|n| schema_of(n).cloned())
            .collect();
        self.filter_offline(defs)
    }

    /// `active_tool_defs`: everything currently active (the flat-prefill fallback).
    pub fn active_defs(&self) -> Vec<Value> {
        let defs = self
            .active
            .iter()
            .filter_map(|n| schema_of(n).cloned())
            .collect();
        self.filter_offline(defs)
    }
}
