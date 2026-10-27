//! Context hygiene: `compact_history`, `_estimate_messages_tokens`, `_workdir_files_text`,
//! `_dedupe_trailing_assistant`, `sanitize_ckpt`, `_similar`, `_attach_pending_images`.

use std::path::Path;

use serde_json::Value;

use super::chat::{ChatBackend, StreamPiece};
use crate::guards::estimate_tokens;
use crate::ollama::{Message, Role};
use crate::text;

/// `_estimate_messages_tokens`: messages + tool schemas, same rough calibration.
pub fn estimate_messages_tokens(messages: &[Message], tools: &[Value]) -> usize {
    let mut total = 0;
    for m in messages {
        total += estimate_tokens(&m.content);
        total += m.images.iter().map(String::len).sum::<usize>() / 4;
        for tc in &m.tool_calls {
            total += estimate_tokens(&tc.function.name);
            total += estimate_tokens(&tc.function.arguments.to_string());
        }
    }
    for t in tools {
        let f = &t["function"];
        total += estimate_tokens(f["name"].as_str().unwrap_or(""));
        total += estimate_tokens(f["description"].as_str().unwrap_or(""));
        total += estimate_tokens(&f["parameters"].to_string());
    }
    total
}

/// `_workdir_files_text`: sorted directory listing, first `limit` names, or `(空)`.
pub fn workdir_files_text(workdir: &Path, limit: usize) -> String {
    let mut names: Vec<String> = match std::fs::read_dir(workdir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => return "(空)".to_string(),
    };
    names.sort();
    names.truncate(limit);
    if names.is_empty() {
        "(空)".to_string()
    } else {
        names.join("\n")
    }
}

const SUMMARY_SYSTEM_L2: &str =
    "用中文压缩成 ≤150 字摘要,保留目标、关键文件名、已做决定、错误和未完成事项,丢弃旧日志与已读全文。";
const SUMMARY_SYSTEM_L3: &str =
    "用中文把下面这段 agent 对话压缩成 ≤150 字摘要,保留关键文件名、代码决策、错误信息和未完成事项。";

/// Result of a compaction attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct Compaction {
    pub messages: Vec<Message>,
    /// A summary replaced history (L2/L3 succeeded): the transcript layer must be told.
    pub summarised: bool,
}

fn zip_text(msgs: &[Message]) -> String {
    msgs.iter()
        .map(|m| format!("[{}]: {}", m.role.as_str(), text::head(&m.content, 400)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn summarise<B: ChatBackend>(
    backend: &mut B,
    model: &str,
    system: &str,
    body: &str,
    todo_text: Option<&str>,
    files_text: Option<&str>,
) -> Result<String, String> {
    let req = [Message::system(system), Message::user(body)];
    let r = backend
        .chat(model, &req, &[], &mut |_: StreamPiece<'_>| {})
        .map_err(|e| e.to_string())?;
    let mut summary = r.message.content;
    if summary.is_empty() {
        summary = "(summary failed)".to_string();
    }
    if let Some(t) = todo_text.filter(|t| !t.is_empty()) {
        summary.push_str("\n\n[当前计划(todo),以此为准继续]\n");
        summary.push_str(t);
    }
    if let Some(f) = files_text.filter(|f| !f.is_empty()) {
        summary.push_str("\n\n[工作目录已有文件(已完成的产物,不要重复创建)]\n");
        summary.push_str(f);
    }
    Ok(summary)
}

/// `compact_history`: tiered compaction.
///
/// - L1: truncate old tool outputs to 120 chars, keep every role, keep the last 2 as is.
/// - L2: summarise everything between the system message and the last 5 messages.
/// - L3: summarise everything between the system message and the last 3 messages.
///
/// `todo_text` / `files_text` ride along after the summary so the plan and the produced
/// files survive a 150-char summary. On a failed model call the input is returned as is
/// and the failure is logged (callers consume the compaction ladder regardless).
pub fn compact_history<B: ChatBackend>(
    backend: &mut B,
    model: &str,
    messages: Vec<Message>,
    level: u8,
    todo_text: Option<&str>,
    files_text: Option<&str>,
    log: &mut dyn FnMut(String),
) -> Compaction {
    let n = messages.len();
    let unchanged = |messages| Compaction {
        messages,
        summarised: false,
    };
    if n <= 4 {
        return unchanged(messages);
    }
    if level <= 1 {
        const MAX_TOOL: usize = 120;
        let mut out = Vec::with_capacity(n);
        for (i, mut m) in messages.into_iter().enumerate() {
            let is_recent = i >= n - 2;
            if i > 0 && m.role == Role::Tool && !is_recent {
                let len = text::clen(&m.content);
                if len > MAX_TOOL {
                    m.content = format!(
                        "{}…(已截断,原 {} 字符)",
                        text::head(&m.content, MAX_TOOL),
                        len
                    );
                }
            }
            out.push(m);
        }
        log("[上下文压缩 L1: 截断旧工具输出]".to_string());
        return unchanged(out);
    }
    let (keep_tail, system) = if level <= 2 {
        if n <= 6 {
            return unchanged(messages);
        }
        (5, SUMMARY_SYSTEM_L2)
    } else {
        (3, SUMMARY_SYSTEM_L3)
    };
    let to_zip = &messages[1..n - keep_tail];
    if to_zip.is_empty() {
        return unchanged(messages);
    }
    let body = zip_text(to_zip);
    match summarise(backend, model, system, &body, todo_text, files_text) {
        Ok(summary) => {
            let mut out = Vec::with_capacity(keep_tail + 2);
            out.push(messages[0].clone());
            out.push(Message::user(format!("[先前上下文摘要] {summary}")));
            out.extend_from_slice(&messages[n - keep_tail..]);
            log(format!(
                "[上下文压缩 L{}: {} → {} 条消息]",
                level.min(3),
                n,
                out.len()
            ));
            Compaction {
                messages: out,
                summarised: true,
            }
        }
        Err(e) => {
            log(format!(
                "[上下文压缩 L{} 失败,本层跳过: {}]",
                level.min(3),
                text::head(&e, 120)
            ));
            unchanged(messages)
        }
    }
}

/// `_dedupe_trailing_assistant`: collapse consecutive assistant messages at the tail so
/// Ollama does not reject the request with "2 or more assistant messages at the end".
pub fn dedupe_trailing_assistant(messages: &mut Vec<Message>) {
    while messages.len() >= 2 {
        let n = messages.len();
        if messages[n - 1].role != Role::Assistant || messages[n - 2].role != Role::Assistant {
            break;
        }
        let last_empty = messages[n - 1].is_blank();
        let prev_empty = messages[n - 2].is_blank();
        if last_empty {
            messages.pop();
        } else if prev_empty {
            messages.remove(n - 2);
        } else {
            let last = messages.pop().unwrap_or_else(|| Message::assistant(""));
            let prev = messages.pop().unwrap_or_else(|| Message::assistant(""));
            let mut merged = prev.clone();
            merged.content = format!("{}\n{}", prev.content, last.content);
            merged.tool_calls = if last.tool_calls.is_empty() {
                prev.tool_calls
            } else {
                last.tool_calls
            };
            messages.push(merged);
        }
    }
}

const CKPT_GARBAGE: [&str; 3] = ["response:unknown", "<|endoftext|>", "`$`"];

/// `sanitize_ckpt`: before resuming, drop a dangling tool-calling assistant turn at the
/// tail (the crash point), empty assistant tails, and garbage-only assistant tails.
pub fn sanitize_ckpt(msgs: &mut Vec<Message>) {
    let mut changed = true;
    while changed {
        changed = false;
        while let Some(last) = msgs.last_mut() {
            if last.role != Role::Assistant || last.tool_calls.is_empty() {
                break;
            }
            last.tool_calls.clear();
            if last.content.trim().is_empty() {
                msgs.pop();
            }
            changed = true;
        }
        while msgs
            .last()
            .is_some_and(|m| m.role == Role::Assistant && m.content.trim().is_empty())
        {
            msgs.pop();
            changed = true;
        }
        while msgs.last().is_some_and(|m| {
            m.role == Role::Assistant
                && m.tool_calls.is_empty()
                && CKPT_GARBAGE.iter().any(|g| m.content.contains(g))
        }) {
            msgs.pop();
            changed = true;
        }
    }
}

/// `_similar`: repeat-output detector. Whitespace is stripped, the first 200 chars are
/// compared with a difflib-style matching-blocks ratio, threshold 0.75.
pub fn similar(a: &str, b: &str, thresh: f64) -> bool {
    let a: Vec<char> = a.chars().filter(|c| !c.is_whitespace()).take(200).collect();
    let b: Vec<char> = b.chars().filter(|c| !c.is_whitespace()).take(200).collect();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let matches = matching_chars(&a, &b);
    let ratio = 2.0 * matches as f64 / (a.len() + b.len()) as f64;
    ratio > thresh
}

/// Sum of matching-block lengths à la `difflib.SequenceMatcher` (longest common
/// substring, recursing left and right; no junk heuristics).
fn matching_chars(a: &[char], b: &[char]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (mut best_i, mut best_j, mut best_len) = (0, 0, 0);
    let mut prev = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, &cb) in b.iter().enumerate() {
            if ca == cb {
                let l = prev[j] + 1;
                cur[j + 1] = l;
                if l > best_len {
                    best_len = l;
                    best_i = i + 1 - l;
                    best_j = j + 1 - l;
                }
            }
        }
        prev = cur;
    }
    if best_len == 0 {
        return 0;
    }
    best_len
        + matching_chars(&a[..best_i], &b[..best_j])
        + matching_chars(&a[best_i + best_len..], &b[best_j + best_len..])
}

/// How many recent messages keep their images (`_IMG_KEEP_MSGS`).
pub const IMG_KEEP_MSGS: usize = 2;

pub fn pending_images_path(workdir: &Path) -> std::path::PathBuf {
    workdir.join(".hb_pending_images.json")
}

/// `_attach_pending_images`: strip images from older messages, then hang the queued images
/// (`.hb_pending_images.json`, written by `read_file`) on the last tool/user message.
/// Returns how many were attached.
pub fn attach_pending_images(messages: &mut [Message], workdir: &Path) -> usize {
    let path = pending_images_path(workdir);
    let pending: Vec<Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut seen = 0;
    for m in messages.iter_mut().rev() {
        if matches!(m.role, Role::User | Role::Tool | Role::Assistant) && !m.images.is_empty() {
            seen += 1;
            if seen > IMG_KEEP_MSGS {
                m.images.clear();
            }
        }
    }
    if pending.is_empty() {
        return 0;
    }
    let mut attached = 0;
    if let Some(m) = messages
        .iter_mut()
        .rev()
        .find(|m| matches!(m.role, Role::Tool | Role::User))
    {
        for it in &pending {
            if let Some(b64) = it.get("b64").and_then(Value::as_str) {
                m.images.push(b64.to_string());
                attached += 1;
            }
        }
    }
    let _ = std::fs::write(&path, "[]");
    attached
}
