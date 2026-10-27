//! Parallel-dispatch configuration (`parallel_config.py`): every threshold, model tag,
//! concurrency cap and memory coefficient is read from here; nothing is hard-coded in the
//! dispatch logic.
//!
//! Override order: defaults ← the `"parallel"` section of `config.json` ← environment
//! (`AGENT_PARALLEL_*`). `AGENT_PARALLEL=0` is the unconditional kill switch and
//! `MINGBIRD_DEPTH` the dispatch depth (the dispatcher gives children depth 1).

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value};

use crate::config::Config;

/// Default child tool face: strictly smaller than the parent's (no commands, network, MCP,
/// deletion, editing or `enable_tools`).
pub const DEFAULT_CHILD_ALLOWED_TOOLS: &[&str] =
    &["read_file", "list_dir", "create_file", "todo", "finish"];

/// Classifier veto list (hard rejection no annotation can lift).
pub const DEFAULT_VETO_PATTERNS: &[&str] = &[
    // destructive
    "删除",
    "删掉",
    "清空",
    "覆盖",
    "重置",
    "format ",
    "delete ",
    "remove ",
    "truncate",
    "drop ",
    // writes to the outside world
    "git push",
    "push to",
    "发布",
    "上传",
    "提交到",
    "发送给",
    "upload",
    "publish",
    // sensitive surface
    ".env",
    "config.json",
    "mcp.json",
    "credential",
    "token",
    "secret",
    "密码",
    "凭据",
    "api key",
    "apikey",
    // cross-file / ambiguous
    "重构",
    "refactor",
    "合并",
    "merge ",
    "迁移",
    "migration",
    "架构",
    "数据库",
    "pip install",
    "npm install",
    "安装依赖",
    "全局替换",
    "所有文件",
    "整个项目",
    // command-only work (failure modes cannot be enumerated)
    "运行测试",
    "跑测试",
    "部署",
    "deploy",
];

/// Positive features: mechanical transformations a small model does well.
pub const DEFAULT_SIMPLE_PATTERNS: &[&str] = &[
    "改名",
    "重命名",
    "rename",
    "转换",
    "convert",
    "格式化",
    "format as",
    "统计",
    "计数",
    "count ",
    "抽取",
    "提取",
    "extract",
    "翻译",
    "translate",
    "总结",
    "汇总",
    "summarize",
    "罗列",
    "列出",
    "list all",
    "生成模板",
    "补全注释",
    "排序",
    "sort ",
    "去重",
    "dedupe",
    "按字母",
    "转成",
    "导出为",
    "export to",
    "csv",
    "markdown 表格",
    "统计行数",
    "逐行",
    "行数",
    "字数",
];

/// One dangerous-command rule: `severity` is `severe` (terminate the child) or `deny`
/// (count towards `max_denies_per_child`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRule {
    pub pattern: String,
    pub severity: String,
    pub reason: String,
}

impl CommandRule {
    pub fn severe(pattern: &str, reason: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
            severity: "severe".to_string(),
            reason: reason.to_string(),
        }
    }

    pub fn deny(pattern: &str, reason: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
            severity: "deny".to_string(),
            reason: reason.to_string(),
        }
    }

    pub fn is_severe(&self) -> bool {
        self.severity == "severe"
    }

    /// `{"pattern","severity","reason"}` objects, or a bare pattern string (`deny`,
    /// reason `user_rule`). Anything else is dropped.
    fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::Object(o) => {
                let pattern = o.get("pattern")?;
                let pattern = stringify(pattern)?;
                if pattern.is_empty() {
                    return None;
                }
                Some(Self {
                    pattern,
                    severity: o
                        .get("severity")
                        .and_then(stringify)
                        .unwrap_or_else(|| "deny".to_string()),
                    reason: o.get("reason").and_then(stringify).unwrap_or_default(),
                })
            }
            Value::String(s) if !s.trim().is_empty() => Some(Self::deny(s.trim(), "user_rule")),
            _ => None,
        }
    }
}

fn stringify(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

/// `DEFAULT_PARALLEL` with the user's overrides applied. Field names follow the JSON keys.
#[derive(Debug, Clone, PartialEq)]
pub struct ParallelConfig {
    /// Master switch; off by default (benchmark freeze safety).
    pub enabled: bool,
    /// Child model tag; empty = feature off. Never pre-filled with a model name.
    pub child_model: String,
    /// Child context window: the main KV-cost lever.
    pub child_ctx: u64,
    /// Trigger: pending todo items below this and a child is not worth loading.
    pub min_pending_items: usize,
    /// Trigger: dispatchable items below this and a child process is not worth it.
    pub min_dispatchable: usize,
    /// Concurrency hard cap (min with the capacity formula).
    pub hard_cap: usize,
    /// Fraction of available RAM that may be used (UMA: RAM backs the shared VRAM).
    pub ram_utilization_cap: f64,
    /// Absolute reserve for the OS / GUI, bytes.
    pub ram_reserve_bytes: f64,
    /// Bytes per billion parameters (Q4–Q8 upper bound = conservative).
    pub bytes_per_param: f64,
    /// Compute/activation buffer multiplier on the weights.
    pub weight_overhead: f64,
    /// llama runner / GPU host-side fixed overhead, bytes.
    pub runtime_overhead_bytes: f64,
    /// The child agent process itself, bytes.
    pub process_overhead_bytes: f64,
    /// KV estimate: parameter-count tier (key) → bytes per 1K tokens (value).
    pub kv_bytes_per_1k_token: BTreeMap<String, f64>,
    /// Children must be small models; larger tags are rejected.
    pub max_child_params_b: f64,
    /// Evicting the parent model from VRAM is thrash; forbidden by default.
    pub allow_parent_eviction: bool,
    /// Overwriting files already in the parent workdir is forbidden by default.
    pub allow_overwrite_existing: bool,
    /// Hard timeout per child task, seconds.
    pub per_task_timeout_s: f64,
    /// Optional verify command timeout, seconds.
    pub verify_timeout_s: f64,
    /// Retries before the item falls back to the parent.
    pub max_retries: usize,
    /// Dispatch depth limit (1 = children never dispatch).
    pub max_depth: usize,
    /// Sub-directory of the workdir that holds the child partitions.
    pub dispatch_dirname: String,
    /// Child liveness poll interval, seconds.
    pub poll_interval_s: f64,
    /// Workdir-misconfiguration guard subset; empty = factory default.
    pub core_system_dir_patterns: Vec<String>,
    /// Child tool face.
    pub child_allowed_tools: Vec<String>,
    /// Sensitive path patterns refused even inside the workdir; empty = factory default.
    pub sensitive_dir_patterns: Vec<String>,
    /// Absolute system locations (audit reason + workdir guard); empty = factory default.
    pub system_dir_patterns: Vec<String>,
    /// Sensitive paths the child may read.
    pub read_allow_patterns: Vec<String>,
    /// Dangerous command rules; empty = factory default. `extra_*` appends.
    pub dangerous_command_patterns: Vec<CommandRule>,
    pub extra_dangerous_command_patterns: Vec<CommandRule>,
    /// Give the child command tools at all (they still pass the blacklist).
    pub child_allow_commands: bool,
    /// Non-severe refusals before the child is terminated.
    pub max_denies_per_child: usize,
    /// Retry after a severe violation (default: fall back to the parent at once).
    pub retry_on_severe: bool,
    /// Audit sampling interval while a child runs, seconds.
    pub audit_poll_interval_s: f64,
    pub veto_patterns: Vec<String>,
    pub simple_patterns: Vec<String>,
}

impl Default for ParallelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            child_model: String::new(),
            child_ctx: 8192,
            min_pending_items: 6,
            min_dispatchable: 2,
            hard_cap: 2,
            ram_utilization_cap: 0.70,
            ram_reserve_bytes: 1_610_612_736.0,
            bytes_per_param: 1_000_000_000.0,
            weight_overhead: 1.25,
            runtime_overhead_bytes: 858_993_459.0,
            process_overhead_bytes: 214_748_365.0,
            kv_bytes_per_1k_token: [
                ("2.5", 67_108_864.0),
                ("4.5", 157_286_400.0),
                ("8.0", 262_144_000.0),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
            max_child_params_b: 8.0,
            allow_parent_eviction: false,
            allow_overwrite_existing: false,
            per_task_timeout_s: 900.0,
            verify_timeout_s: 300.0,
            max_retries: 1,
            max_depth: 1,
            dispatch_dirname: "_dispatch".to_string(),
            poll_interval_s: 0.5,
            core_system_dir_patterns: Vec::new(),
            child_allowed_tools: DEFAULT_CHILD_ALLOWED_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            sensitive_dir_patterns: Vec::new(),
            system_dir_patterns: Vec::new(),
            read_allow_patterns: Vec::new(),
            dangerous_command_patterns: Vec::new(),
            extra_dangerous_command_patterns: Vec::new(),
            child_allow_commands: false,
            max_denies_per_child: 3,
            retry_on_severe: false,
            audit_poll_interval_s: 5.0,
            veto_patterns: DEFAULT_VETO_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            simple_patterns: DEFAULT_SIMPLE_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// Environment overrides (`_ENV_KEYS`).
const ENV_KEYS: [(&str, &str); 7] = [
    ("child_model", "AGENT_PARALLEL_MODEL"),
    ("child_ctx", "AGENT_PARALLEL_CTX"),
    ("min_pending_items", "AGENT_PARALLEL_MIN_ITEMS"),
    ("min_dispatchable", "AGENT_PARALLEL_MIN_DISPATCH"),
    ("hard_cap", "AGENT_PARALLEL_MAX"),
    ("per_task_timeout_s", "AGENT_PARALLEL_TIMEOUT"),
    ("max_depth", "AGENT_PARALLEL_DEPTH"),
];

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// `int(float(val))`, rejecting negatives for the unsigned fields.
fn as_usize(v: &Value) -> Option<usize> {
    let f = as_f64(v)?;
    if f.is_finite() && f >= 0.0 {
        Some(f.trunc() as usize)
    } else {
        None
    }
}

fn as_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(
            s.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::Null => false,
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Comma-separated string or a JSON array of anything stringifiable.
fn as_str_list(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::String(s) => Some(
            s.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect(),
        ),
        Value::Array(a) => Some(a.iter().filter_map(stringify).collect()),
        Value::Null => Some(Vec::new()),
        _ => None,
    }
}

fn as_rules(v: &Value) -> Option<Vec<CommandRule>> {
    match v {
        Value::Array(a) => Some(a.iter().filter_map(CommandRule::from_value).collect()),
        Value::String(s) => Some(
            s.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(|p| CommandRule::deny(p, "user_rule"))
                .collect(),
        ),
        Value::Null => Some(Vec::new()),
        _ => None,
    }
}

impl ParallelConfig {
    /// Defaults with the `"parallel"` section of a loaded config merged in.
    pub fn from_config(cfg: &Config) -> Self {
        Self::from_map(&cfg.parallel)
    }

    /// Defaults with a raw `"parallel"` object merged in.
    pub fn from_map(user: &Map<String, Value>) -> Self {
        let mut c = Self::default();
        c.merge(user);
        c
    }

    /// Apply user values key by key. Unknown keys are ignored; a value of the wrong type
    /// keeps the current setting (never panics, never fails).
    pub fn merge(&mut self, user: &Map<String, Value>) {
        for (k, v) in user {
            self.set(k, v);
        }
    }

    /// Environment overrides on top (`AGENT_PARALLEL_MODEL`, `AGENT_PARALLEL_MAX`, …).
    pub fn with_env(mut self, env: &HashMap<String, String>) -> Self {
        self.apply_env(env);
        self
    }

    pub fn apply_env(&mut self, env: &HashMap<String, String>) {
        for (key, var) in ENV_KEYS {
            if let Some(v) = env.get(var).filter(|s| !s.is_empty()) {
                self.set(key, &Value::String(v.clone()));
            }
        }
    }

    fn set(&mut self, key: &str, v: &Value) {
        macro_rules! num {
            ($field:ident, $conv:ident) => {
                if let Some(x) = $conv(v) {
                    self.$field = x;
                }
            };
        }
        match key {
            "enabled" => self.enabled = as_bool(v),
            "allow_parent_eviction" => self.allow_parent_eviction = as_bool(v),
            "allow_overwrite_existing" => self.allow_overwrite_existing = as_bool(v),
            "child_allow_commands" => self.child_allow_commands = as_bool(v),
            "retry_on_severe" => self.retry_on_severe = as_bool(v),
            "child_model" => {
                if let Some(s) = stringify(v) {
                    self.child_model = s.trim().to_string();
                }
            }
            "child_ctx" => {
                if let Some(x) = as_usize(v) {
                    self.child_ctx = x as u64;
                }
            }
            "min_pending_items" => num!(min_pending_items, as_usize),
            "min_dispatchable" => num!(min_dispatchable, as_usize),
            "hard_cap" => num!(hard_cap, as_usize),
            "max_retries" => num!(max_retries, as_usize),
            "max_depth" => num!(max_depth, as_usize),
            "max_denies_per_child" => num!(max_denies_per_child, as_usize),
            "ram_reserve_bytes" => num!(ram_reserve_bytes, as_f64),
            "runtime_overhead_bytes" => num!(runtime_overhead_bytes, as_f64),
            "process_overhead_bytes" => num!(process_overhead_bytes, as_f64),
            "per_task_timeout_s" => num!(per_task_timeout_s, as_f64),
            "verify_timeout_s" => num!(verify_timeout_s, as_f64),
            "poll_interval_s" => num!(poll_interval_s, as_f64),
            "audit_poll_interval_s" => num!(audit_poll_interval_s, as_f64),
            "ram_utilization_cap" => num!(ram_utilization_cap, as_f64),
            "bytes_per_param" => num!(bytes_per_param, as_f64),
            "weight_overhead" => num!(weight_overhead, as_f64),
            "max_child_params_b" => num!(max_child_params_b, as_f64),
            "kv_bytes_per_1k_token" => {
                if let Value::Object(o) = v {
                    let table: BTreeMap<String, f64> = o
                        .iter()
                        .filter_map(|(k, x)| as_f64(x).map(|f| (k.clone(), f)))
                        .collect();
                    if !table.is_empty() {
                        self.kv_bytes_per_1k_token = table;
                    }
                }
            }
            "dispatch_dirname" => {
                if let Some(s) = stringify(v) {
                    let s = s.trim();
                    let p = std::path::Path::new(s);
                    if !s.is_empty() && !p.is_absolute() && !s.contains("..") {
                        self.dispatch_dirname = s.to_string();
                    }
                }
            }
            "core_system_dir_patterns" => num!(core_system_dir_patterns, as_str_list),
            "child_allowed_tools" => num!(child_allowed_tools, as_str_list),
            "sensitive_dir_patterns" => num!(sensitive_dir_patterns, as_str_list),
            "system_dir_patterns" => num!(system_dir_patterns, as_str_list),
            "read_allow_patterns" => num!(read_allow_patterns, as_str_list),
            "veto_patterns" => num!(veto_patterns, as_str_list),
            "simple_patterns" => num!(simple_patterns, as_str_list),
            "dangerous_command_patterns" => num!(dangerous_command_patterns, as_rules),
            "extra_dangerous_command_patterns" => {
                num!(extra_dangerous_command_patterns, as_rules)
            }
            _ => {}
        }
    }

    /// `kv_bytes_per_1k_token` as sorted `(params_b_limit, bytes_per_1k)` pairs.
    pub fn kv_tiers(&self) -> Vec<(f64, f64)> {
        let mut tiers: Vec<(f64, f64)> = self
            .kv_bytes_per_1k_token
            .iter()
            .filter_map(|(k, v)| k.trim().parse::<f64>().ok().map(|lim| (lim, *v)))
            .collect();
        tiers.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        tiers
    }
}

/// `AGENT_PARALLEL=0|false|off|no`: nothing is dispatched whatever the config says.
pub fn hard_disabled(env: &HashMap<String, String>) -> bool {
    env.get("AGENT_PARALLEL").is_some_and(|v| {
        matches!(
            v.trim().to_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

/// Dispatch depth: parent 0, children 1 (`MINGBIRD_DEPTH`; the old `HUMMINGBIRD_DEPTH`
/// is still read). Garbage reads as 0.
pub fn current_depth(env: &HashMap<String, String>) -> usize {
    let raw = env
        .get("MINGBIRD_DEPTH")
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env.get("HUMMINGBIRD_DEPTH"))
        .map(|s| s.trim())
        .unwrap_or("0");
    if raw.is_empty() {
        return 0;
    }
    raw.parse().unwrap_or(0)
}

/// The process environment as a map (the dispatch layer takes maps so tests inject them).
pub fn env_snapshot() -> HashMap<String, String> {
    std::env::vars().collect()
}
