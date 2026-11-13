//! Machine configuration (`appconfig.py`).
//!
//! Data directory resolution: `RUFOUS_HOME` > `BOUBOU_HOME` (the name before rufous) >
//! `MINGBIRD_HOME` > `~/.ollama_agent`.
//! rufous deliberately shares Mingbird's directory so existing config, skills and
//! sessions keep working after the switch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

pub const DEFAULT_OLLAMA_HOST: &str = "http://127.0.0.1:11434";
/// LM Studio's OpenAI-compatible endpoint when `local_backend.base_url` names none.
pub const LMSTUDIO_BASE_URL: &str = "http://127.0.0.1:1234/v1";

/// A named model server (`--backend`, config `backend`; bead boubou-56g).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Ollama,
    Lmstudio,
    /// Any other OpenAI-compatible server; its URL is `local_backend.base_url`.
    Openai,
}

impl BackendKind {
    pub const NAMES: [&'static str; 3] = ["ollama", "lmstudio", "openai"];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ollama" => Some(Self::Ollama),
            "lmstudio" => Some(Self::Lmstudio),
            "openai" => Some(Self::Openai),
            _ => None,
        }
    }
}

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
    /// With `backend` named, only its `base_url` and `api_key` are read.
    pub local_backend: serde_json::Map<String, serde_json::Value>,
    /// The model server: `"ollama"`, `"lmstudio"` (`local_backend.base_url`, else
    /// [`LMSTUDIO_BASE_URL`]) or `"openai"` (`local_backend.base_url` required). Named, it
    /// wins over `local_backend.type` and over an enabled `cloud`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendKind>,
    /// `--backend` from the command line: over `backend`, and never written back.
    #[serde(skip)]
    pub backend_flag: Option<BackendKind>,
    /// `--base-url` from the command line: over `local_backend.base_url` for a named
    /// `lmstudio` or `openai` backend, and never written back (boubou-f8d).
    #[serde(skip)]
    pub base_url_flag: Option<String>,
    /// `--no-start-engine`: the engine is the host's. rufous neither starts nor restarts
    /// Ollama, and a model call that cannot connect ends the turn at once (boubou-f8d).
    #[serde(skip)]
    pub no_start_engine: bool,
    /// Offline for this session whatever the config and `AGENT_OFFLINE` say: a host's
    /// per-session switch (`_meta.rufous.offline`, docs/egress.md). Never written.
    #[serde(skip)]
    pub offline_forced: bool,
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
            backend: None,
            backend_flag: None,
            base_url_flag: None,
            no_start_engine: false,
            offline_forced: false,
            slim_output: SlimOutput::Auto,
            parallel: Default::default(),
            profiles: BTreeMap::new(),
            decider: Default::default(),
            scope_gate: Default::default(),
        }
    }
}

/// The environment variable that carries the local engine's API key: over
/// `local_backend.api_key`, for a host that keeps the key in a keychain and must not write
/// it to `config.json`. Read when the engine is built and never stored in [`Config`], so
/// saving the config does not write it back.
pub const API_KEY_ENV: &str = "RUFOUS_API_KEY";

/// The same for the cloud provider: over `cloud.api_key`.
pub const CLOUD_API_KEY_ENV: &str = "RUFOUS_CLOUD_API_KEY";

/// Both, for the processes rufous starts (the model's shell commands, hooks, MCP servers,
/// `ollama serve`): they are taken out of each one's environment, so a command the model
/// runs cannot read the key. Dispatched children are rufous itself and keep them.
pub const API_KEY_ENVS: [&str; 2] = [API_KEY_ENV, CLOUD_API_KEY_ENV];

/// An API key from the environment; unset, empty and blank all mean there is none.
fn env_api_key(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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
    /// `LM Studio` or `OpenAI-compatible`, for logs and errors.
    pub label: &'static str,
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

/// Where rufous keeps config, global skills, sessions and memory.
pub fn agent_home() -> PathBuf {
    for var in [
        "RUFOUS_HOME",
        "BOUBOU_HOME",
        "MINGBIRD_HOME",
        "HUMMINGBIRD_HOME",
    ] {
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

/// The user's home as the prompt names it (boubou-f8d): `RUFOUS_USER_HOME` (`--user-home`)
/// when set; none inside a macOS App Sandbox, where the process's home is the host app's
/// container and naming it would send the model to the wrong Desktop; else the home
/// directory.
pub fn user_home() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("RUFOUS_USER_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v));
    }
    if std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    dirs::home_dir()
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
    /// keys it knows and falls back to defaults on any error, so a key rufous cannot read
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
        if self.offline_forced {
            return true;
        }
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
        if self.offline() || self.named_backend().is_some() {
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
            api_key: env_api_key(CLOUD_API_KEY_ENV).unwrap_or_else(|| s("api_key")),
            model,
        })
    }

    /// The backend named by `--backend` or the config's `backend`, with where it came from.
    pub fn named_backend(&self) -> Option<(BackendKind, &'static str)> {
        self.backend_flag
            .map(|b| (b, "--backend"))
            .or_else(|| self.backend.map(|b| (b, "config.json backend")))
    }

    /// What chose the model server, for the `[engine]` log line.
    pub fn backend_source(&self) -> &'static str {
        match self.named_backend() {
            Some((_, from)) => from,
            None if self.cloud_provider().is_some() => "config.json cloud",
            None if self.backend_openai().is_some() => "config.json local_backend",
            None => "default",
        }
    }

    /// The model engine as a host reports it: its product name (`Ollama`, `LM Studio`,
    /// `OpenAI-compatible`, `cloud`) and its URL (docs/egress.md).
    pub fn engine_endpoint(&self) -> (&'static str, String) {
        if let Some(cp) = self.cloud_provider() {
            return ("cloud", cp.base_url);
        }
        match self.backend_openai() {
            Some(e) => (e.label, e.base_url),
            None => ("Ollama", self.ollama_host()),
        }
    }

    /// A named backend that cannot be used as configured: `openai` without a URL, a
    /// `--base-url` with no backend that takes one, or, offline, an engine on another
    /// machine (offline means this machine, docs/egress.md).
    pub fn backend_problem(&self) -> Option<String> {
        if self.offline() {
            let (_, url) = self.engine_endpoint();
            if !crate::egress::is_local_url(&url) {
                return Some(format!(
                    "offline: the model engine at {url} is not on this machine"
                ));
            }
        }
        let url = self.local_backend_str("base_url");
        if self.base_url_flag.is_some()
            && !matches!(
                self.named_backend(),
                Some((BackendKind::Lmstudio | BackendKind::Openai, _))
            )
        {
            return Some(
                "--base-url needs --backend lmstudio or openai (Ollama's address is OLLAMA_HOST)"
                    .to_string(),
            );
        }
        match self.named_backend() {
            Some((BackendKind::Openai, from)) if url.is_empty() => Some(format!(
                "backend \"openai\" (from {from}) needs local_backend.base_url, the server's /v1 URL"
            )),
            _ => None,
        }
    }

    fn local_backend_str(&self, k: &str) -> String {
        if k == "base_url" {
            if let Some(url) = self.base_url_flag.as_deref() {
                return url.trim().to_string();
            }
        }
        if k == "api_key" {
            if let Some(key) = env_api_key(API_KEY_ENV) {
                return key;
            }
        }
        self.local_backend
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    }

    /// The local engine: a named backend first ([`Config::named_backend`]); else the legacy
    /// `local_backend`, where an `openai` entry without a `base_url` falls back to Ollama
    /// (a half-configured engine is not an engine; a named `openai` reports it instead,
    /// see [`Config::backend_problem`]).
    pub fn local_backend(&self) -> LocalBackend {
        let url = self
            .local_backend_str("base_url")
            .trim_end_matches('/')
            .to_string();
        let engine = |label: &'static str, base_url: String| {
            LocalBackend::OpenAi(OpenAiEngine {
                base_url,
                api_key: self.local_backend_str("api_key"),
                label,
            })
        };
        match self.named_backend().map(|(b, _)| b) {
            Some(BackendKind::Ollama) => LocalBackend::Ollama,
            Some(BackendKind::Lmstudio) if url.is_empty() => {
                engine("LM Studio", LMSTUDIO_BASE_URL.to_string())
            }
            Some(BackendKind::Lmstudio) => engine("LM Studio", url),
            Some(BackendKind::Openai) | None
                if !url.is_empty()
                    && (self.named_backend().is_some()
                        || self.local_backend_str("type") == "openai") =>
            {
                engine("OpenAI-compatible", url)
            }
            _ => LocalBackend::Ollama,
        }
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
        match self.backend_openai() {
            Some(e) => e.label,
            None => "Ollama",
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
