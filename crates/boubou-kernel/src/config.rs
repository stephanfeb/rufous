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
    /// A number or boolean value is written as its text (`{"OLLAMA_NUM_GPU": 1}`).
    #[serde(deserialize_with = "env_map")]
    pub ollama_env: BTreeMap<String, String>,
    /// Friendly display name -> model tag.
    pub models: BTreeMap<String, String>,
    /// One-switch offline mode: local models only, no network tools. `"1"`, `"on"`,
    /// `"yes"`, `"true"` and a non-zero number count as on.
    #[serde(deserialize_with = "loose_bool")]
    pub offline_mode: bool,
    /// Cloud (OpenAI-compatible) provider: `{base_url, api_key, model, enabled}`.
    pub cloud: serde_json::Map<String, serde_json::Value>,
    /// Local inference backend: `{}` or `{"type":"ollama"}` or `{"type":"openai", ...}`.
    pub local_backend: serde_json::Map<String, serde_json::Value>,
    /// Command-output trimming: `auto` | `on` | `off`.
    pub slim_output: SlimOutput,
    /// Parallel sub-agent dispatch section (`parallel_config.py`); kept opaque until ported.
    pub parallel: serde_json::Map<String, serde_json::Value>,
    /// Per-model profile overrides keyed by family (`gemma4`) or exact tag (`gemma4:12b`);
    /// each sets only the fields it names on top of the built-in table (`profile.rs`).
    pub profiles: BTreeMap<String, crate::profile::ProfileOverride>,
    /// System One decider (`decider.rs`): `{"mode": "off" | "shadow" | "on", "model": "d1:latest",
    /// "timeout_ms": 1500, "threshold": 0.9}`. Off by default.
    pub decider: crate::decider::DeciderConfig,
    /// The scope gate (`scope.rs`, bead boubou-6x3): `"on"` asks before the first call that
    /// writes code under a tracker or docs instruction, `"shadow"` logs what it would have
    /// asked, `"off"` does nothing. On by default.
    pub scope_gate: crate::scope::ScopeGateMode,
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
            profiles: BTreeMap::new(),
            decider: Default::default(),
            scope_gate: Default::default(),
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

/// `slim_output`: `"auto"` | `"on"` | `"off"`. Mingbird 3.0.0 wrote a boolean (`true` =
/// forced on; `false` is read as auto, as `appconfig.slim_output_mode` does), and the
/// switch words `1/true/yes` and `0/false/no` are taken in either case.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SlimOutput {
    #[default]
    Auto,
    On,
    Off,
}

impl SlimOutput {
    /// `on`/`off` as a setting; `auto` is none (the context size decides).
    pub fn as_bool(self) -> Option<bool> {
        match self {
            SlimOutput::Auto => None,
            SlimOutput::On => Some(true),
            SlimOutput::Off => Some(false),
        }
    }
}

impl<'de> Deserialize<'de> for SlimOutput {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Bool(true) => Ok(SlimOutput::On),
            Value::Bool(false) | Value::Null => Ok(SlimOutput::Auto),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "auto" | "" => Ok(SlimOutput::Auto),
                w => match truthy(w) {
                    Some(true) => Ok(SlimOutput::On),
                    Some(false) => Ok(SlimOutput::Off),
                    None => Err(serde::de::Error::custom(format!(
                        "expected auto, on or off, got {s:?}"
                    ))),
                },
            },
            other => Err(serde::de::Error::custom(format!(
                "expected auto, on or off, got {other}"
            ))),
        }
    }
}

/// The switch words both the config and the environment accept.
pub fn truthy(word: &str) -> Option<bool> {
    match word.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn loose_bool<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    match Value::deserialize(d)? {
        Value::Bool(b) => Ok(b),
        Value::Null => Ok(false),
        Value::Number(n) => Ok(n.as_f64().is_some_and(|f| f != 0.0)),
        Value::String(s) => truthy(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("expected true or false, got {s:?}"))),
        other => Err(serde::de::Error::custom(format!(
            "expected true or false, got {other}"
        ))),
    }
}

/// A `{name: value}` map whose scalar values are written as text; `null` entries are
/// dropped, anything structured is an error for that key.
fn env_map<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error> {
    let raw: BTreeMap<String, Value> = Deserialize::deserialize(d)?;
    let mut out = BTreeMap::new();
    for (k, v) in raw {
        let text = match v {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => continue,
            other => {
                return Err(serde::de::Error::custom(format!(
                    "{k}: expected a string, got {other}"
                )))
            }
        };
        out.insert(k, text);
    }
    Ok(out)
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
    /// Load from the default location; a missing file yields defaults, a file that is not
    /// JSON is an error, and a key whose value has the wrong shape is dropped with a
    /// warning (see [`Config::from_json`]).
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let (cfg, dropped) = Self::from_json(&text)
                    .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
                for d in dropped {
                    tracing::warn!("{}: {d}", path.display());
                }
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// Parse the config text. The file is shared with Mingbird, whose loader copies the
    /// keys it knows and falls back to defaults on any error, so a key Boubou cannot read
    /// (`"decider": {"threshold": "high"}`) must not stop the binary from starting: the
    /// whole document is tried first, and when that fails each top-level key is tried on
    /// its own, the ones that parse are kept and the rest are returned as messages for the
    /// caller to log. Text that is not a JSON object is an error: that file needs the
    /// user's attention, not a silent default.
    pub fn from_json(text: &str) -> Result<(Self, Vec<String>)> {
        let value: Value = serde_json::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
        let Value::Object(map) = value else {
            return Err(Error::Config(
                "the config must be a JSON object".to_string(),
            ));
        };
        if let Ok(cfg) = serde_json::from_value::<Self>(Value::Object(map.clone())) {
            return Ok((cfg, Vec::new()));
        }
        let mut kept = serde_json::Map::new();
        let mut dropped = Vec::new();
        for (k, v) in map {
            let mut trial = kept.clone();
            trial.insert(k.clone(), v.clone());
            match serde_json::from_value::<Self>(Value::Object(trial)) {
                Ok(_) => {
                    kept.insert(k, v);
                }
                Err(e) => dropped.push(format!("ignoring `{k}` ({e}); using the default")),
            }
        }
        let cfg = serde_json::from_value::<Self>(Value::Object(kept))
            .map_err(|e| Error::Config(e.to_string()))?;
        Ok((cfg, dropped))
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
            Ok(v) if !v.trim().is_empty() => truthy(&v) == Some(true),
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
