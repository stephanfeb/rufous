//! MCP server configuration (`mcp.json`) and the category tagging that lets MCP tools be
//! flattened into the built-in tool categories. The client itself is pending the `rmcp`
//! port; see `docs/PORTING.md`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Built-in categories the router understands. MCP tools tagged outside these are never routed.
pub const CATEGORIES: [&str; 5] = ["文件", "代码", "网络", "记忆", "MCP"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
    /// Default categories for every tool on this server (`["MCP"]` when absent).
    #[serde(default)]
    pub categories: Vec<String>,
    /// Per-tool category overrides for "large" servers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Vec<String>>,
}

impl ServerConfig {
    pub fn is_http(&self) -> bool {
        self.url.is_some()
    }

    /// Categories for a tool: per-tool override, else server default, else `MCP`.
    pub fn categories_for(&self, tool: &str) -> Vec<String> {
        if let Some(c) = self.tools.get(tool) {
            return c.clone();
        }
        if self.categories.is_empty() {
            vec!["MCP".to_string()]
        } else {
            self.categories.clone()
        }
    }

    /// Tags not in `CATEGORIES`, which the router would silently ignore.
    pub fn unknown_categories(&self) -> Vec<String> {
        let mut bad: Vec<String> = self
            .categories
            .iter()
            .chain(self.tools.values().flatten())
            .filter(|c| !CATEGORIES.contains(&c.as_str()))
            .cloned()
            .collect();
        bad.sort();
        bad.dedup();
        bad
    }
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
