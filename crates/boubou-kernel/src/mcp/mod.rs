//! MCP: `mcp.json` configuration, category tagging, the probed tool manifest with its
//! memory + disk cache, flattening of MCP tools into the built-in categories as real tool
//! definitions named `server.tool`, and the `rmcp` client (stdio child process and
//! streamable HTTP) behind `mcp_call` and the flattened names.
//!
//! The data formats are Mingbird's: the same `mcp.json` keys (`command`/`args`/`env`,
//! `url`/`headers`, `categories`, `tools`, `expose`) and the same
//! `<home>/mcp_manifest_cache.json` shape, so a probe by either stack warms the other.

mod client;
mod manifest;

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub use client::{call_tool, probe_server, ProbedTool, CALL_TIMEOUT, PROBE_TIMEOUT};
pub use manifest::{
    compact_schema, is_path_key, resolve_mcp_paths, Manifest, ManifestTool, McpRegistry, Prober,
    ServerManifest, CACHE_FILE, MANIFEST_TTL,
};

use crate::{Error, Result};

/// Built-in categories the router understands. MCP tools tagged outside these are never routed.
pub const CATEGORIES: [&str; 5] = [CAT_FILES, CAT_CODE, CAT_WEB, CAT_MEMORY, CAT_MCP];
pub const CAT_FILES: &str = "files";
pub const CAT_CODE: &str = "code";
pub const CAT_WEB: &str = "web";
pub const CAT_MEMORY: &str = "memory";
pub const CAT_MCP: &str = "mcp";

/// Canonical category for a tag written in `mcp.json`: case-insensitive English (`Files`,
/// `network`). Unknown tags return `None`. (Mingbird's Chinese tag names are not accepted:
/// Boubou is English-only.)
pub fn normalise_category(tag: &str) -> Option<&'static str> {
    let t = tag.trim();
    Some(match t.to_ascii_lowercase().as_str() {
        "files" | "file" => CAT_FILES,
        "code" => CAT_CODE,
        "web" | "network" => CAT_WEB,
        "memory" => CAT_MEMORY,
        "mcp" => CAT_MCP,
        _ => return None,
    })
}

/// Per-tool entry under `tools`: either a bare category list or `{categories, expose}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ToolTag {
    Categories(Vec<String>),
    Full {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        categories: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expose: Option<String>,
    },
}

impl ToolTag {
    fn categories(&self) -> Option<&[String]> {
        match self {
            Self::Categories(c) => Some(c),
            Self::Full { categories, .. } if !categories.is_empty() => Some(categories),
            Self::Full { .. } => None,
        }
    }

    fn expose(&self) -> Option<&str> {
        match self {
            Self::Full { expose, .. } => expose.as_deref(),
            Self::Categories(_) => None,
        }
    }
}

/// One `mcp.json` entry. Unknown keys are ignored, as Mingbird ignores them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ServerConfig {
    /// stdio transport: executable to spawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// streamable-HTTP transport: skipped entirely in offline mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Extra HTTP headers for a `url` server (API keys and the like).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Default categories for every tool on this server (`["MCP"]` when absent).
    #[serde(default)]
    pub categories: Vec<String>,
    /// Per-tool category / expose overrides for "large" servers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolTag>,
    /// Server-wide exposure: `auto` (default), `on-demand` or `disabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose: Option<String>,
}

impl ServerConfig {
    pub fn is_http(&self) -> bool {
        self.url.is_some()
    }

    /// Spawnable or reachable: has a `command` or a `url`.
    pub fn is_runnable(&self) -> bool {
        self.command.as_deref().is_some_and(|c| !c.is_empty()) || self.url.is_some()
    }

    /// `mcp_server_categories`: the declared list normalised and filtered to known
    /// categories, `["mcp"]` when nothing is declared.
    pub fn server_categories(&self) -> Vec<String> {
        if self.categories.is_empty() {
            return vec![CAT_MCP.to_string()];
        }
        normalise_categories(&self.categories)
    }

    /// Categories for a tool: per-tool override, else the server default.
    pub fn categories_for(&self, tool: &str) -> Vec<String> {
        if let Some(c) = self.tools.get(tool).and_then(ToolTag::categories) {
            return normalise_categories(c);
        }
        self.server_categories()
    }

    /// `_mcp_expose`: tool-level `expose` > server-level > `auto`.
    pub fn expose_for(&self, tool: &str) -> String {
        self.tools
            .get(tool)
            .and_then(ToolTag::expose)
            .or(self.expose.as_deref())
            .unwrap_or("auto")
            .to_string()
    }

    /// Tags not in `CATEGORIES`, which the router would silently ignore.
    pub fn unknown_categories(&self) -> Vec<String> {
        let mut bad: Vec<String> = self
            .categories
            .iter()
            .chain(
                self.tools
                    .values()
                    .filter_map(ToolTag::categories)
                    .flatten(),
            )
            .filter(|c| normalise_category(c).is_none())
            .cloned()
            .collect();
        bad.sort();
        bad.dedup();
        bad
    }
}

/// Known tags mapped to their canonical names, order kept, duplicates dropped.
pub fn normalise_categories(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags.iter().filter_map(|t| normalise_category(t)) {
        if !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    out
}

pub type McpConfig = BTreeMap<String, ServerConfig>;

pub fn load_from(path: &Path) -> Result<McpConfig> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(McpConfig::new()),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// `~/.ollama_agent/mcp.json`, falling back to `<install_dir>/mcp.json`.
pub fn load(agent_home: &Path, install_dir: &Path) -> Result<McpConfig> {
    let user = agent_home.join("mcp.json");
    if user.exists() {
        return load_from(&user);
    }
    load_from(&install_dir.join("mcp.json"))
}

/// Model-facing tool identity: `server.tool`.
pub fn qualified_name(server: &str, tool: &str) -> String {
    format!("{server}.{tool}")
}

/// `server.tool` → `(server, tool)`; `None` when either side is empty.
pub fn split_qualified(name: &str) -> Option<(&str, &str)> {
    let (server, tool) = name.split_once('.')?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}
