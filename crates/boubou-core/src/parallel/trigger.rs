//! The trigger layer (`parallel_todo.py`): todo provider, task classifier, dispatch decision.
//!
//! Everything is decided in harness code (memory probe, todo counts, a deterministic
//! simplicity heuristic); the system prompt is untouched and the model never needs to know
//! that sub-agents exist (zero prefill cost). Every rejected item carries a plain-language
//! reason. The parent model's own annotations (`[simple]` / `[complex]`) can only demote an
//! item, never lift it past the veto list or the safety precheck.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::config::{current_depth, hard_disabled, ParallelConfig};
use super::probe::{CapacityPlan, CapacityPlanner};
use super::sandbox::{norm_command, norm_path, RuleSet, DEFAULT_SENSITIVE_DIR_PATTERNS};
use crate::todo::atomic_write_json;

static FILE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9_\-\x{4e00}-\x{9fff}]+\.[A-Za-z0-9]{2,5}").expect("regex")
});
const ARTIFACT_EXTS: [&str; 15] = [
    ".md", ".txt", ".csv", ".json", ".py", ".yaml", ".yml", ".html", ".tsv", ".log", ".tex",
    ".svg", ".rst", ".ini", ".toml",
];
const SIMPLE_PREFIX: &str = "[simple]";
const COMPLEX_PREFIX: &str = "[complex]";
const COMPLEXITY_WORDS: [&str; 14] = [
    "complex",
    "complicated",
    "hard",
    "difficult",
    "critical",
    "dangerous",
    "risky",
    "cross-file",
    "core",
    "critical",
    "complex",
    "hard",
    "risky",
    "careful",
];

/// A todo item in the neutral shape the trigger works on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DispatchItem {
    pub id: String,
    pub text: String,
    pub done: bool,
    /// `simple` / `complex` / empty.
    pub complexity: String,
}

impl DispatchItem {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            done: false,
            complexity: String::new(),
        }
    }
}

/// The todo pipeline's minimal interface. Storage adapters implement this.
pub trait TodoProvider {
    /// All items in order.
    fn all_items(&self) -> Result<Vec<DispatchItem>, String>;
    /// Unfinished items in order.
    fn pending_items(&self) -> Result<Vec<DispatchItem>, String> {
        Ok(self.all_items()?.into_iter().filter(|i| !i.done).collect())
    }
    /// Mark done. Idempotent; must not reorder the other items.
    fn mark_done(&mut self, item_id: &str, note: &str) -> bool;
    /// Persist the parent model's complexity annotation (optional).
    fn set_complexity(&mut self, _item_id: &str, _level: &str) -> bool {
        false
    }
}

/// `<workdir>/todo.json` as the loop writes it (`[{"item","done"}, …]`), read leniently:
/// `item` / `text` / `title` keys, bare strings, and an `{"items": […]}` wrapper all work.
#[derive(Debug, Clone)]
pub struct JsonTodoProvider {
    pub path: PathBuf,
}

impl JsonTodoProvider {
    pub fn new(workdir: &Path) -> Self {
        Self {
            path: crate::todo::todo_file(workdir),
        }
    }

    fn read_raw(&self) -> Option<Value> {
        let s = std::fs::read_to_string(&self.path).ok()?;
        serde_json::from_str(&s).ok()
    }

    fn items_of(raw: &Value) -> Vec<DispatchItem> {
        let list = match raw {
            Value::Array(a) => a.as_slice(),
            Value::Object(o) => match o.get("items").or_else(|| o.get("todo")) {
                Some(Value::Array(a)) => a.as_slice(),
                _ => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        for (i, it) in list.iter().enumerate() {
            let (text, done, id, complexity) = match it {
                Value::String(s) => (s.trim().to_string(), false, None, String::new()),
                Value::Object(o) => {
                    let text = ["item", "text", "title"]
                        .iter()
                        .filter_map(|k| o.get(*k))
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .find(|s| !s.is_empty())
                        .unwrap_or("")
                        .to_string();
                    let done = o.get("done").is_some_and(truthy);
                    let id = o.get("id").and_then(scalar_str).filter(|s| !s.is_empty());
                    let complexity = o.get("complexity").and_then(scalar_str).unwrap_or_default();
                    (text, done, id, complexity)
                }
                _ => continue,
            };
            if text.is_empty() {
                continue;
            }
            out.push(DispatchItem {
                id: id.unwrap_or_else(|| (i + 1).to_string()),
                text,
                done,
                complexity,
            });
        }
        out
    }

    /// Edit matching entries in place (`id` field or 1-based position) and write back.
    fn mutate(&self, item_id: &str, f: impl Fn(&mut serde_json::Map<String, Value>)) -> bool {
        let Some(Value::Array(mut data)) = self.read_raw() else {
            return false;
        };
        let mut hits = 0;
        for (i, it) in data.iter_mut().enumerate() {
            let Value::Object(o) = it else { continue };
            let by_id = o.get("id").and_then(scalar_str).unwrap_or_default();
            if (i + 1).to_string() == item_id || by_id == item_id {
                f(o);
                hits += 1;
            }
        }
        if hits == 0 {
            return false;
        }
        atomic_write_json(&self.path, &Value::Array(data)).is_ok()
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Null => false,
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn scalar_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

impl TodoProvider for JsonTodoProvider {
    fn all_items(&self) -> Result<Vec<DispatchItem>, String> {
        Ok(self
            .read_raw()
            .map(|v| Self::items_of(&v))
            .unwrap_or_default())
    }

    fn mark_done(&mut self, item_id: &str, note: &str) -> bool {
        self.mutate(item_id, |o| {
            o.insert("done".to_string(), Value::Bool(true));
            if !note.is_empty() {
                o.insert("note".to_string(), Value::String(note.to_string()));
            }
        })
    }

    fn set_complexity(&mut self, item_id: &str, level: &str) -> bool {
        self.mutate(item_id, |o| {
            o.insert("complexity".to_string(), Value::String(level.to_string()));
        })
    }
}

// ---------------- classifier (deterministic, no model call) ----------------

/// Non-ASCII hints match as substrings; ASCII hints on word boundaries (so `csv` does not
/// hit `scsv`).
fn hint_hit(hint: &str, low: &str) -> bool {
    if !hint.is_ascii() {
        return low.contains(hint);
    }
    let h = hint.trim();
    if h.is_empty() {
        return false;
    }
    Regex::new(&format!(r"\b{}\b", regex::escape(h))).is_ok_and(|re| re.is_match(low))
}

/// File-name tokens with an artefact extension.
pub fn file_tokens(text: &str) -> Vec<String> {
    FILE_TOKEN
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .filter(|t| {
            let l = t.to_lowercase();
            ARTIFACT_EXTS.iter().any(|e| l.ends_with(e))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifyResult {
    pub dispatchable: bool,
    pub reason: String,
    pub file_tokens: Vec<String>,
}

impl ClassifyResult {
    fn no(reason: impl Into<String>) -> Self {
        Self {
            dispatchable: false,
            reason: reason.into(),
            file_tokens: Vec::new(),
        }
    }
}

/// `classify_task`: one todo item → dispatchable to a child? Three layers: hard veto,
/// positive mechanical features with a single-file scope, and the parent's annotation.
pub fn classify_task(text: &str, cfg: &ParallelConfig) -> ClassifyResult {
    let low = text.to_lowercase();
    let low = low.trim();
    if low.is_empty() {
        return ClassifyResult::no("empty_item");
    }
    let annotated_simple = low.starts_with(SIMPLE_PREFIX);
    if low.starts_with(COMPLEX_PREFIX) {
        return ClassifyResult::no("annotated_complex(the main model marked it complex)");
    }
    for pat in &cfg.veto_patterns {
        if !pat.is_empty() && hint_hit(&pat.to_lowercase(), low) {
            return ClassifyResult::no(format!("veto_pattern({pat})"));
        }
    }
    for w in COMPLEXITY_WORDS {
        if hint_hit(w, low) {
            return ClassifyResult::no(format!("complexity_word({w})"));
        }
    }
    let simple_hit = cfg
        .simple_patterns
        .iter()
        .any(|p| hint_hit(&p.to_lowercase(), low));
    let toks = file_tokens(text);
    if annotated_simple {
        if !(1..=2).contains(&toks.len()) {
            return ClassifyResult::no("annotated_simple_but_not_single_file_scope");
        }
        return ClassifyResult {
            dispatchable: true,
            reason: "annotated_simple".to_string(),
            file_tokens: toks,
        };
    }
    if simple_hit && toks.len() == 1 {
        return ClassifyResult {
            dispatchable: true,
            reason: format!("mechanical_single_file({toks:?})"),
            file_tokens: toks,
        };
    }
    if simple_hit {
        return ClassifyResult::no(format!(
            "mechanical_but_files={}(not a single-file scope)",
            toks.len()
        ));
    }
    if toks.len() == 1 {
        return ClassifyResult::no(
            "single_file_but_not_mechanical(no mechanical-transformation feature)",
        );
    }
    ClassifyResult::no("not_mechanical_and_not_single_file")
}

/// `safety_precheck`: the second gate before spawning — item text naming a sensitive path
/// pattern or a severe command is not dispatched (better than killing the child later).
pub fn safety_precheck(text: &str, cfg: &ParallelConfig) -> Result<(), String> {
    let norm = norm_path(text);
    let patterns: Vec<String> = if cfg.sensitive_dir_patterns.is_empty() {
        DEFAULT_SENSITIVE_DIR_PATTERNS
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        cfg.sensitive_dir_patterns.clone()
    };
    for pat in &patterns {
        let p = norm_path(pat);
        if !p.is_empty() && norm.contains(&p) {
            return Err(format!("sensitive_in_task_text({pat})"));
        }
    }
    let rules = RuleSet::from_config(cfg);
    if let Some(rule) = rules.find(&norm_command(text)) {
        if rule.is_severe() {
            return Err(format!("severe_command_in_task_text({})", rule.reason));
        }
    }
    Ok(())
}

// ---------------- the decision (all conditions AND) ----------------

#[derive(Debug, Clone, Default)]
pub struct DispatchPlan {
    pub should: bool,
    /// Plain-language reasons, including why nothing is dispatched.
    pub reasons: Vec<String>,
    pub items: Vec<DispatchItem>,
    pub skipped: Vec<(DispatchItem, String)>,
    pub capacity: Option<CapacityPlan>,
}

impl DispatchPlan {
    pub fn n_items(&self) -> usize {
        self.items.len()
    }

    fn stop(reason: String) -> Self {
        Self {
            reasons: vec![reason],
            ..Default::default()
        }
    }
}

/// `should_dispatch`: every condition must hold; the first failing one is the reason.
pub fn should_dispatch(
    probe: &dyn CapacityPlanner,
    provider: &dyn TodoProvider,
    cfg: &ParallelConfig,
    env: &HashMap<String, String>,
    child_model: Option<&str>,
) -> DispatchPlan {
    if !cfg.enabled {
        return DispatchPlan::stop(
            "disabled(parallel.enabled=false; off by default, safe during the freeze)".to_string(),
        );
    }
    if hard_disabled(env) {
        return DispatchPlan::stop("hard_kill_switch(AGENT_PARALLEL=0)".to_string());
    }
    let depth = current_depth(env);
    if depth >= cfg.max_depth {
        return DispatchPlan::stop(format!(
            "depth_limit(current depth {depth} >= max_depth {}; a sub-agent may not dispatch again)",
            cfg.max_depth
        ));
    }
    let model = child_model
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(String::from)
        .unwrap_or_else(|| cfg.child_model.trim().to_string());
    if model.is_empty() {
        return DispatchPlan::stop(
            "no_child_model(parallel.child_model is not configured; nothing is hard-coded, it must be set explicitly)".to_string(),
        );
    }
    let pending = match provider.pending_items() {
        Ok(p) => p,
        Err(e) => return DispatchPlan::stop(format!("todo_provider_error({e})")),
    };
    let n_pending = pending.len();
    if n_pending < cfg.min_pending_items {
        return DispatchPlan::stop(format!(
            "too_few_pending(pending {n_pending} < threshold {})",
            cfg.min_pending_items
        ));
    }
    let mut plan = DispatchPlan::default();
    for it in pending {
        let cls = classify_task(&it.text, cfg);
        if !cls.dispatchable {
            plan.skipped.push((it, cls.reason));
            continue;
        }
        if let Err(why) = safety_precheck(&it.text, cfg) {
            plan.skipped.push((it, why));
            continue;
        }
        plan.items.push(it);
    }
    if plan.items.len() < cfg.min_dispatchable {
        plan.reasons.push(format!(
            "too_few_dispatchable(dispatchable {} < threshold {}; the rest were rejected by the classifier / safety pre-check)",
            plan.items.len(),
            cfg.min_dispatchable
        ));
        return plan;
    }
    let cap = probe.plan_capacity(&model, cfg);
    if cap.max_parallel < 1 {
        plan.reasons.push(format!(
            "no_capacity(no room for even one small model) | {}",
            cap.reasons.join("; ")
        ));
        plan.capacity = Some(cap);
        return plan;
    }
    plan.reasons.insert(
        0,
        format!(
            "trigger_ok(pending={n_pending}, dispatchable={}, max_parallel={}, model={model})",
            plan.items.len(),
            cap.max_parallel
        ),
    );
    plan.capacity = Some(cap);
    plan.should = true;
    plan
}
