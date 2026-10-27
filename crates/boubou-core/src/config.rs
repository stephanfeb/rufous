//! Machine configuration (`appconfig.py`).
//!
//! Data directory resolution: `BOUBOU_HOME` > `MINGBIRD_HOME` > `~/.ollama_agent`.
//! Boubou deliberately shares Mingbird's directory so existing config, skills and
//! sessions keep working after the switch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const DEFAULT_OLLAMA_HOST: &str = "http://127.0.0.1:11434";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Ollama API base URL. Overridden by `OLLAMA_HOST`.
    pub ollama_host: String,
    /// Full path to the ollama executable; empty = auto-detect. Overridden by `OLLAMA_BIN`.
    pub ollama_exe: String,
    /// Extra environment for a spawned `ollama serve` (e.g. `OLLAMA_IGPU_ENABLE=1`).
    pub ollama_env: BTreeMap<String, String>,
    /// Friendly display name -> model tag.
    pub models: BTreeMap<String, String>,
    /// One-switch offline mode: local models only, no network tools.
    pub offline_mode: bool,
    /// Cloud (OpenAI-compatible) provider: `{base_url, api_key, model, enabled}`.
    pub cloud: serde_json::Map<String, serde_json::Value>,
    /// Local inference backend: `{}` or `{"type":"ollama"}` or `{"type":"openai", ...}`.
    pub local_backend: serde_json::Map<String, serde_json::Value>,
    /// Command-output trimming: `auto` | `on` | `off`.
    pub slim_output: SlimOutput,
    /// Parallel sub-agent dispatch section (`parallel_config.py`); kept opaque until ported.
    pub parallel: serde_json::Map<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ollama_host: DEFAULT_OLLAMA_HOST.to_string(),
            ollama_exe: String::new(),
            ollama_env: BTreeMap::new(),
            models: BTreeMap::new(),
            offline_mode: false,
            cloud: Default::default(),
            local_backend: Default::default(),
            slim_output: SlimOutput::Auto,
            parallel: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SlimOutput {
    #[default]
    Auto,
    On,
    Off,
}

/// Where Boubou keeps config, global skills, sessions and memory.
pub fn agent_home() -> PathBuf {
    for var in ["BOUBOU_HOME", "MINGBIRD_HOME", "HUMMINGBIRD_HOME"] {
        if let Some(v) = std::env::var_os(var) {
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ollama_agent")
}

pub fn config_path() -> PathBuf {
    agent_home().join("config.json")
}

impl Config {
    /// Load from the default location; a missing file yields defaults, a malformed one is an error.
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| Error::Config(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text).map_err(|e| Error::io(path, e))
    }

    /// Effective Ollama host: `OLLAMA_HOST` env > config > default.
    pub fn ollama_host(&self) -> String {
        std::env::var("OLLAMA_HOST")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                if self.ollama_host.trim().is_empty() {
                    DEFAULT_OLLAMA_HOST.to_string()
                } else {
                    self.ollama_host.clone()
                }
            })
    }

    /// Effective offline switch: `AGENT_OFFLINE` env > config.
    pub fn offline(&self) -> bool {
        match std::env::var("AGENT_OFFLINE") {
            Ok(v) if !v.trim().is_empty() => {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            }
            _ => self.offline_mode,
        }
    }

    /// Display name for a model tag, falling back to the tag itself.
    pub fn display_name<'a>(&'a self, tag: &'a str) -> &'a str {
        self.models
            .iter()
            .find(|(_, t)| t.as_str() == tag)
            .map(|(name, _)| name.as_str())
            .unwrap_or(tag)
    }
}
