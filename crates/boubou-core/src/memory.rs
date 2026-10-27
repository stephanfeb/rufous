//! `memory_store` / `memory_recall`: the long-term fact list in `<agent home>/memory.json`.
//! Recall is word-overlap scoring, Mingbird's exact ordering, so both stacks rank alike.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::todo::atomic_write_json;
use crate::Result;

pub fn memory_file(agent_home: &Path) -> PathBuf {
    agent_home.join("memory.json")
}

/// Entries are `{text, ts}` objects; a missing or corrupt file is an empty memory.
pub fn load_memory(agent_home: &Path) -> Vec<Value> {
    std::fs::read_to_string(memory_file(agent_home))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

pub fn save_memory(agent_home: &Path, mem: &[Value]) -> Result<()> {
    std::fs::create_dir_all(agent_home).ok();
    atomic_write_json(&memory_file(agent_home), mem)
}

/// `memory_store(text)` → `[saved N facts]`.
pub fn memory_store(agent_home: &Path, text: &str) -> String {
    let mut mem = load_memory(agent_home);
    mem.push(json!({
        "text": text,
        "ts": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
    }));
    match save_memory(agent_home, &mem) {
        Ok(()) => format!("[saved {} facts]", mem.len()),
        Err(e) => format!("[memory_store error: {e}]"),
    }
}

fn words(s: &str) -> BTreeSet<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w一-鿿]+").expect("regex"));
    RE.find_iter(&s.to_lowercase())
        .map(|m| m.as_str().to_string())
        .collect()
}

/// `memory_recall(query, limit=3)`: facts sharing the most words with the query, newest
/// three when nothing overlaps, `(no memory)` when empty.
pub fn memory_recall(agent_home: &Path, query: &str, limit: usize) -> String {
    let mem = load_memory(agent_home);
    let qw = words(query);
    let mut scored: Vec<(usize, String, String)> = mem
        .iter()
        .map(|m| {
            let text = m
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let ts = m
                .get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let score = words(&text).intersection(&qw).count();
            (score, text, ts)
        })
        .collect();
    // Python sorts the (score, text, ts) tuples in reverse.
    scored.sort_by(|a, b| b.cmp(a));
    let mut hits: Vec<String> = scored
        .iter()
        .filter(|(s, _, _)| *s > 0)
        .take(limit)
        .map(|(_, t, ts)| format!("[{ts}] {t}"))
        .collect();
    if hits.is_empty() {
        hits = mem
            .iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .filter_map(|m| m.get("text").and_then(Value::as_str).map(str::to_string))
            .collect();
    }
    if hits.is_empty() {
        "(no memory)".to_string()
    } else {
        hits.join("\n")
    }
}
