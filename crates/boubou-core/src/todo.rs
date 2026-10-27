//! The task plan (`todo.json` in the workspace), shared by the harness receipts and the
//! GUI plan panel so both sides render identical text.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tools::TodoAction;
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct TodoItem {
    pub item: String,
    pub done: bool,
}

impl TodoItem {
    pub fn new(item: impl Into<String>) -> Self {
        Self {
            item: item.into(),
            done: false,
        }
    }
}

pub fn todo_file(workdir: &Path) -> PathBuf {
    workdir.join("todo.json")
}

/// Missing or malformed file yields an empty plan, as in Mingbird.
pub fn load_todo(workdir: &Path) -> Vec<TodoItem> {
    std::fs::read_to_string(todo_file(workdir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_todo(workdir: &Path, items: &[TodoItem]) -> Result<()> {
    atomic_write_json(&todo_file(workdir), items)
}

/// Write `.tmp` then rename, so a kill mid-write leaves the previous file intact.
pub fn atomic_write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.tmp"),
        None => "tmp".to_string(),
    });
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp, text).map_err(|e| Error::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
}

/// `[x]/[ ] N. item` lines.
pub fn format_todo_lines(items: &[TodoItem]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(i, x)| {
            format!(
                "{} {}. {}",
                if x.done { "[x]" } else { "[ ]" },
                i + 1,
                x.item
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn load_todo_str(workdir: &Path) -> String {
    let t = load_todo(workdir);
    if t.is_empty() {
        "(todo list empty)".to_string()
    } else {
        format_todo_lines(&t)
    }
}

/// `(done, total)`.
pub fn todo_progress(items: &[TodoItem]) -> (usize, usize) {
    (items.iter().filter(|x| x.done).count(), items.len())
}

/// Decoded `todo` tool arguments, after the lenient coercions small models need
/// (`plan`/`steps` aliases, a newline-separated string for items, a list for index, `all`).
#[derive(Debug, Clone, PartialEq)]
pub struct TodoArgs {
    pub action: TodoAction,
    pub items: Vec<String>,
    pub index: Vec<i64>,
    pub all: bool,
    pub done: Option<bool>,
}

/// The `todo` tool. Messages are the ones Mingbird's tests pin.
pub fn run_todo(workdir: &Path, args: &TodoArgs) -> String {
    match args.action {
        TodoAction::Create => {
            let items: Vec<TodoItem> = args
                .items
                .iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| TodoItem::new(s.clone()))
                .collect();
            if items.is_empty() {
                return "[todo create: no items found. Pass items as a list of strings, e.g. items=[\"step 1\", \"step 2\"]]".to_string();
            }
            match save_todo(workdir, &items) {
                Ok(()) => load_todo_str(workdir),
                Err(e) => format!("[tool error: {e}]"),
            }
        }
        TodoAction::Update => {
            let mut t = load_todo(workdir);
            if t.is_empty() {
                return "[todo update: 计划为空。先调用 todo(action=create, items=[...]) 建计划。]"
                    .to_string();
            }
            let done_flag = args.done.unwrap_or(true);
            if args.all {
                for x in &mut t {
                    x.done = done_flag;
                }
                return match save_todo(workdir, &t) {
                    Ok(()) => load_todo_str(workdir),
                    Err(e) => format!("[tool error: {e}]"),
                };
            }
            let mut hit = 0;
            for &j in &args.index {
                if j >= 1 && (j as usize) <= t.len() {
                    t[(j - 1) as usize].done = done_flag;
                    hit += 1;
                }
            }
            if let Err(e) = save_todo(workdir, &t) {
                return format!("[tool error: {e}]");
            }
            if hit == 0 {
                let valid = (1..=t.len())
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return format!(
                    "[todo update: index 无效(计划共 {} 项,可用 index: {valid})。要全部勾选请用 todo(action=update, all=true)。]",
                    t.len()
                );
            }
            load_todo_str(workdir)
        }
        TodoAction::List => load_todo_str(workdir),
    }
}
