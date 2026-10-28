//! Machine configuration (`appconfig.py`).
//!
//! Data directory resolution: `BOUBOU_HOME` > `MINGBIRD_HOME` > `~/.ollama_agent`.
//! Boubou deliberately shares Mingbird's directory so existing config, skills and
//! sessions keep working after the switch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// An enabled, complete cloud provider (`config.cloud`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

/// An OpenAI-compatible local engine (`config.local_backend`): LM Studio `:1234/v1`,
/// llama.app `:9931/v1`, llama-server `:8080/v1`, Jan, vLLM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiEngine {
    pub base_url: String,
    pub api_key: String,
}

/// The local inference engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalBackend {
    Ollama,
    OpenAi(OpenAiEngine),
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
    /// `dedupe_model_entries`: one entry per digest (an `ollama cp` alias shares the weights).
    /// Preference: a name the config maps > a non-`:latest` name > the shorter name.
    /// Returns `(display name, tag)` pairs in the order the server listed them.
    pub fn dedupe_model_entries(
        &self,
        models: &[crate::ollama::ModelInfo],
    ) -> Vec<(String, String)> {
        let mapped = |n: &str| self.models.values().any(|t| t == n);
        let rank = |n: &str| (mapped(n), !n.ends_with(":latest"), -(n.len() as i64));
        let mut order: Vec<String> = Vec::new();
        let mut best: BTreeMap<String, String> = BTreeMap::new();
        for m in models {
            if m.name.is_empty() {
                continue;
            }
            let key = if m.digest.is_empty() {
                format!("name:{}", m.name)
            } else {
                m.digest.clone()
            };
            match best.get(&key) {
                Some(cur) if rank(&m.name) <= rank(cur) => {}
                Some(_) => {
                    best.insert(key, m.name.clone());
                }
                None => {
                    order.push(key.clone());
                    best.insert(key, m.name.clone());
                }
            }
        }
        order
            .iter()
            .filter_map(|k| best.get(k))
            .map(|tag| (self.display_name(tag).to_string(), tag.clone()))
            .collect()
    }

    /// `cloud_provider`: the cloud (OpenAI-compatible) provider when it is enabled and
    /// complete (`base_url` + `model`). Offline mode disables it (local-first red line).
    pub fn cloud_provider(&self) -> Option<CloudProvider> {
        if self.offline() {
            return None;
        }
        let s = |k: &str| {
            self.cloud
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let enabled = match self.cloud.get("enabled") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(x)) => !x.is_empty(),
            _ => false,
        };
        let (base_url, model) = (s("base_url").trim_end_matches('/').to_string(), s("model"));
        if !enabled || base_url.is_empty() || model.is_empty() {
            return None;
        }
        Some(CloudProvider {
            base_url,
            api_key: s("api_key"),
            model,
        })
    }

    /// `local_backend`, normalised: an `openai` entry without a `base_url` falls back to
    /// Ollama (a half-configured engine is not an engine).
    pub fn local_backend(&self) -> LocalBackend {
        let s = |k: &str| {
            self.local_backend
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        if s("type") == "openai" && !s("base_url").is_empty() {
            return LocalBackend::OpenAi(OpenAiEngine {
                base_url: s("base_url").trim_end_matches('/').to_string(),
                api_key: s("api_key"),
            });
        }
        LocalBackend::Ollama
    }

    /// `backend_openai`: the local engine when it is an OpenAI-compatible server.
    pub fn backend_openai(&self) -> Option<OpenAiEngine> {
        match self.local_backend() {
            LocalBackend::OpenAi(e) => Some(e),
            LocalBackend::Ollama => None,
        }
    }

    /// `engine_display_name`: product names, not translated.
    pub fn engine_display_name(&self) -> &'static str {
        if self.backend_openai().is_some() {
            "OpenAI-compatible"
        } else {
            "Ollama"
        }
    }

    pub fn display_name<'a>(&'a self, tag: &'a str) -> &'a str {
        self.models
            .iter()
            .find(|(_, t)| t.as_str() == tag)
            .map(|(name, _)| name.as_str())
            .unwrap_or(tag)
    }
}
