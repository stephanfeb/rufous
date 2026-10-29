//! Context hygiene: `prune_history` (recency pruning, no Mingbird counterpart),
//! `compact_history`, `_estimate_messages_tokens`, `_workdir_files_text`,
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

/// `_workdir_files_text`: sorted directory listing, first `limit` names, or `(empty)`.
pub fn workdir_files_text(workdir: &Path, limit: usize) -> String {
    let mut names: Vec<String> = match std::fs::read_dir(workdir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => return "(empty)".to_string(),
    };
    names.sort();
    names.truncate(limit);
    if names.is_empty() {
        "(empty)".to_string()
    } else {
        names.join("\n")
    }
}

/// Recency pruning (bead boubou-65d.6): how many of the latest tool results stay verbatim
/// when pruning is on (`AGENT_PRUNE_KEEP=5`). Off by default (`RunParams::prune_keep` 0):
/// the first measurement (docs/PORTING.md, 2026-10-29) showed fewer compactions but no
/// score or wall-time gain at n=1.
pub const PRUNE_KEEP: usize = 5;
/// Pre-call estimate fraction of the context at which pruning runs (below the ladder's L1 at 0.60).
pub const PRUNE_AT: f64 = 0.50;
/// Prefix of a pruned tool result; also the idempotence marker.
pub const PRUNED_MARK: &str = "[earlier]";
const PRUNE_ARG_MAX: usize = 160;
const PRUNE_LINE_MAX: usize = 120;

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

fn last_line(s: &str) -> &str {
    s.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

/// The argument that names what a call acted on, for the fact line.
fn call_target(args: &Value) -> Option<String> {
    [
        "path", "command", "pattern", "query", "url", "action", "name",
    ]
    .iter()
    .find_map(|k| args.get(k).and_then(Value::as_str))
    .map(|v| {
        let v = v.lines().next().unwrap_or(v).trim();
        if text::clen(v) > 60 {
            format!("{}…", text::head(v, 60))
        } else {
            v.to_string()
        }
    })
}

/// One line that keeps what a tool result established: the tool and its target, the
/// result's first line and, when different, its last (a test runner's `60 passed, 0 failed`,
/// a command's exit status, an edit's confirmation).
fn fact_line(tool: &str, args: &Value, result: &str) -> String {
    let head = first_line(result);
    let tail = last_line(result);
    let mut out = format!("{PRUNED_MARK} {tool}");
    if let Some(t) = call_target(args) {
        out.push_str(&format!("({t})"));
    }
    out.push_str(" → ");
    out.push_str(text::head(head, PRUNE_LINE_MAX));
    if tail != head && !tail.is_empty() {
        out.push_str(" … ");
        out.push_str(text::head(tail, PRUNE_LINE_MAX));
    }
    out
}

/// Recency pruning: the last `keep` tool results stay verbatim; every older tool result is
/// replaced by its [`fact_line`], and the assistant turn that called it loses its thinking
/// and any long string argument (a `create_file` body, an `edit_file` old/new pair). The
/// history keeps its shape (every call still has its result), so the request stays valid
/// and the model still sees *that* it did each step and what came of it; only the bulk
/// goes. Idempotent: already-pruned results are skipped. Returns how many results were
/// pruned by this call.
pub fn prune_history(messages: &mut [Message], keep: usize) -> usize {
    let n = messages.len();
    // Index of the assistant turn that owns each tool message, and the call's position in it.
    let mut owner: Vec<Option<(usize, usize)>> = vec![None; n];
    let mut last_asst: Option<usize> = None;
    let mut k = 0;
    for i in 0..n {
        match messages[i].role {
            Role::Assistant if !messages[i].tool_calls.is_empty() => {
                last_asst = Some(i);
                k = 0;
            }
            Role::Assistant => last_asst = None,
            Role::Tool => {
                if let Some(a) = last_asst {
                    owner[i] = Some((a, k));
                    k += 1;
                }
            }
            _ => {}
        }
    }
    let tool_idx: Vec<usize> = (0..n).filter(|&i| messages[i].role == Role::Tool).collect();
    if tool_idx.len() <= keep {
        return 0;
    }
    let cutoff = tool_idx[tool_idx.len() - keep]; // first index that stays verbatim
    let mut pruned = 0;
    for &i in tool_idx.iter().filter(|&&i| i < cutoff) {
        if messages[i].content.starts_with(PRUNED_MARK) {
            continue;
        }
        let (tool, args) = match owner[i] {
            Some((a, k)) => {
                let tc = messages[a].tool_calls.get(k);
                (
                    tc.map(|t| t.function.name.clone())
                        .or_else(|| messages[i].tool_name.clone())
                        .unwrap_or_else(|| "tool".to_string()),
                    tc.map(|t| t.function.arguments.clone())
                        .unwrap_or(Value::Null),
                )
            }
            None => (
                messages[i]
                    .tool_name
                    .clone()
                    .unwrap_or_else(|| "tool".to_string()),
                Value::Null,
            ),
        };
        let line = fact_line(&tool, &args, &messages[i].content);
        messages[i].content = line;
        messages[i].images.clear();
        pruned += 1;
        if let Some((a, _)) = owner[i] {
            let m = &mut messages[a];
            m.thinking = None;
            for tc in &mut m.tool_calls {
                if let Some(obj) = tc.function.arguments.as_object_mut() {
                    for v in obj.values_mut() {
                        if let Some(s) = v.as_str() {
                            let len = text::clen(s);
                            if len > PRUNE_ARG_MAX {
                                *v = Value::String(format!(
                                    "{}… ({len} chars)",
                                    text::head(s, PRUNE_ARG_MAX / 2)
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    pruned
}

const SUMMARY_SYSTEM_L2: &str =
    "Compress this into a summary of at most 150 words in English. Keep the goal, key file names, decisions made, errors and unfinished items; drop old logs and full texts already read.";
const SUMMARY_SYSTEM_L3: &str =
    "Compress the following agent conversation into a summary of at most 150 words in English. Keep key file names, code decisions, error messages and unfinished items.";

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
        summary.push_str("\n\n[Current plan (todo); continue from this]\n");
        summary.push_str(t);
    }
    if let Some(f) = files_text.filter(|f| !f.is_empty()) {
        summary.push_str("\n\n[Files already in the working directory (finished artefacts; do not create them again)]\n");
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
                        "{}… (truncated, originally {} chars)",
                        text::head(&m.content, MAX_TOOL),
                        len
                    );
                }
            }
            out.push(m);
        }
        log("[context compaction L1: truncated old tool outputs]".to_string());
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
            out.push(Message::user(format!(
                "{} {summary}",
                crate::guards::qa::CONTEXT_SUMMARY_MARK
            )));
            out.extend_from_slice(&messages[n - keep_tail..]);
            log(format!(
                "[context compaction L{}: {} → {} messages]",
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
                "[context compaction L{} failed, level skipped: {}]",
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
