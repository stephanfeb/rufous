//! Sessions, transcript and checkpoint, byte-compatible with Mingbird so an interrupted
//! Mingbird task resumes under Boubou and vice versa.
//!
//! - `<workdir>/.agent_state.json`: the live message list, rewritten atomically every turn.
//!   An embedding whose workdir is the user's own project (the ACP server: the editor's
//!   cwd) relocates it to `<sessions>/<name>.state.json` via [`Agent::checkpoint`] so the
//!   model never finds the harness's history in a directory listing and the user's git
//!   status stays clean (bead boubou-ll7).
//! - `<sessions>/<name>.json`: the saved context (what the model sees).
//! - `<sessions>/<name>.transcript.jsonl`: every message as it happened; compaction only
//!   touches the model context, the GUI renders history from here.
//! - `<sessions>/<name>.meta.json`: merged metadata (`updated`, `task`, `status`, `msgs`,
//!   `workdir`, plus whatever the GUI wrote, e.g. `title`).
//!   Boubou adds `transcript_next`, the transcript cursor (see [`Transcript::flush`]).
//! - `<sessions>/<name>.todo.json`: snapshot of the workdir plan, bound to the session.
//! - `<sessions>/<name>.decisions.jsonl`: one line per System One decision when the decider
//!   is in `shadow` or `on` mode (Boubou addition, bead boubou-acy.3); the labelled-set
//!   builder ingests these as `shadow:<name>` candidates.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::ollama::{Message, Role};
use crate::text;
use crate::todo::{atomic_write_json, todo_file};

pub fn sessions_dir(agent_home: &Path) -> PathBuf {
    agent_home.join("sessions")
}

pub fn checkpoint_path(workdir: &Path) -> PathBuf {
    workdir.join(".agent_state.json")
}

/// The relocated checkpoint for a named session: `<sessions>/<name>.state.json`, beside
/// the session's other files. Its elapsed-time sidecar is `<name>.state.meta.json`
/// ([`crate::guards::budget::elapsed_meta_for`]).
pub fn state_path(sessions_dir: &Path, name: &str) -> PathBuf {
    sessions_dir.join(format!("{name}.state.json"))
}

/// Write the checkpoint atomically (tmp + rename) so a hard kill never leaves half a JSON.
pub fn write_checkpoint(workdir: &Path, messages: &[Message]) {
    write_checkpoint_to(&checkpoint_path(workdir), messages);
}

/// [`write_checkpoint`] to an explicit path; the parent directory is created.
pub fn write_checkpoint_to(p: &Path, messages: &[Message]) {
    let tmp = p.with_extension("json.tmp");
    let Ok(s) = serde_json::to_string(messages) else {
        return;
    };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&tmp, s).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

/// Load and sanitise a checkpoint. A corrupt file is renamed `.corrupt` and `None` returned.
pub fn load_checkpoint(workdir: &Path) -> Option<Vec<Message>> {
    load_checkpoint_from(&checkpoint_path(workdir))
}

/// [`load_checkpoint`] from an explicit path.
pub fn load_checkpoint_from(p: &Path) -> Option<Vec<Message>> {
    let raw = std::fs::read_to_string(p).ok()?;
    match serde_json::from_str::<Vec<Message>>(&raw) {
        Ok(mut msgs) => {
            super::history::sanitize_ckpt(&mut msgs);
            Some(msgs)
        }
        Err(_) => {
            let _ = std::fs::rename(p, p.with_extension("json.corrupt"));
            None
        }
    }
}

/// The decider sidecar of a session.
pub fn decisions_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.decisions.jsonl"))
}

/// Append one decision record to the session's sidecar. Best effort: a failure to write the
/// labelling trace must never touch the run.
pub fn append_decision(dir: &Path, name: &str, record: &Value) {
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(decisions_path(dir, name))
    {
        let _ = writeln!(f, "{record}");
    }
}

pub fn load_session(dir: &Path, name: &str) -> Option<Vec<Message>> {
    let raw = std::fs::read_to_string(dir.join(format!("{name}.json"))).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn load_session_meta(dir: &Path, name: &str) -> Map<String, Value> {
    std::fs::read_to_string(dir.join(format!("{name}.meta.json")))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// `_merge_session_meta`: update fields without clobbering what others wrote.
pub fn merge_session_meta(dir: &Path, name: &str, extra: Map<String, Value>) {
    let mut meta = load_session_meta(dir, name);
    meta.extend(extra);
    let _ = std::fs::create_dir_all(dir);
    let _ = atomic_write_json(&dir.join(format!("{name}.meta.json")), &meta);
}

/// Transcript cursor state (`_TL_NEXT`, `_TL_SESSION`, `_TL_COMPACTED`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    session: Option<String>,
    next: usize,
    compacted: bool,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// `_tl_mark_compacted`: the next shortening is a compaction/reset, not a dedupe.
    pub fn mark_compacted(&mut self) {
        self.compacted = true;
    }

    /// `_transcript_flush`: append new messages; on a compaction-shortening also append the
    /// new summary (index 1); on any other shortening just realign the cursor.
    ///
    /// The cursor (`next`: how many leading context messages are on disk) is persisted to
    /// the session meta as `transcript_next` whenever it moves, and a fresh `Transcript`
    /// starts from it. It used to start from the transcript's line count, which is wrong
    /// as soon as the context has ever been shortened: a continued session (the editor, the
    /// web page and `--append` build a new `Transcript` per turn) then has a context
    /// shorter than the transcript, the comparison read the new user turn as a shortening
    /// and never logged it, nor any later turn's user message (review 2026-10-29).
    /// Mingbird's `_transcript_flush` has the same flaw; its v3.1.4 fix for `--append`
    /// counted lines and does not cover this case.
    pub fn flush(&mut self, dir: &Path, session: Option<&str>, messages: &[Message]) {
        let Some(session) = session else {
            return;
        };
        let p = dir.join(format!("{session}.transcript.jsonl"));
        let n = messages.len();
        if self.session.as_deref() != Some(session) {
            self.session = Some(session.to_string());
            self.next = initial_cursor(dir, session, &p, n);
        }
        let before = self.next;
        let mut wrote = false;
        match n.cmp(&self.next) {
            std::cmp::Ordering::Greater => {
                wrote = append_lines(dir, &p, messages[self.next..].iter());
                self.next = n;
            }
            std::cmp::Ordering::Less | std::cmp::Ordering::Equal => {
                // A compaction leaves a new summary at index 1 whatever the new length.
                if self.compacted && n > 1 {
                    wrote = append_lines(dir, &p, std::iter::once(&messages[1]));
                }
                self.compacted = false;
                self.next = n;
            }
        }
        if wrote || self.next != before {
            let mut upd = Map::new();
            upd.insert("transcript_next".into(), Value::from(self.next));
            merge_session_meta(dir, session, upd);
        }
    }
}

/// Append `msgs` as JSON lines; `true` when at least one line was written.
fn append_lines<'a>(dir: &Path, p: &Path, msgs: impl Iterator<Item = &'a Message>) -> bool {
    let _ = std::fs::create_dir_all(dir);
    let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(p) else {
        return false;
    };
    let mut wrote = false;
    for m in msgs {
        if let Ok(line) = serde_json::to_string(m) {
            wrote |= writeln!(f, "{line}").is_ok();
        }
    }
    wrote
}

/// Where a fresh [`Transcript`] starts for a session that already has files: the cursor
/// the last flush persisted, bounded by the lines on disk (a transcript trimmed by hand).
/// A meta without the key, written before the cursor was persisted, is read as a continued
/// session when the context is the saved one (`msgs`) plus one turn; any other shape
/// (a kill-resume from the checkpoint with a stale save) keeps the line count, which can
/// only lose a turn, never duplicate a run.
fn initial_cursor(dir: &Path, session: &str, transcript: &Path, n: usize) -> usize {
    let lines = std::fs::read_to_string(transcript)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    let meta = load_session_meta(dir, session);
    let get = |k: &str| meta.get(k).and_then(Value::as_u64).map(|v| v as usize);
    match get("transcript_next") {
        Some(c) => c.min(lines),
        None => match get("msgs") {
            Some(m) if m <= lines && n == m + 1 => m,
            _ => lines,
        },
    }
}

/// Session status derived from the tail of the conversation.
fn session_status(msgs: &[Message]) -> &'static str {
    for m in msgs.iter().rev() {
        if m.content.contains("TASK_COMPLETE") {
            return "done";
        }
        if m.content.contains("MAX ITERATIONS") {
            return "max-iter";
        }
        if m.tool_calls.iter().any(|tc| tc.function.name == "finish") {
            return "done";
        }
    }
    "running"
}

/// `save_session`: transcript first, then the context JSON, the todo snapshot and meta.
pub fn save_session(
    dir: &Path,
    name: Option<&str>,
    msgs: &[Message],
    workdir: Option<&Path>,
    transcript: &mut Transcript,
) {
    let Some(name) = name else {
        return;
    };
    transcript.flush(dir, Some(name), msgs);
    let _ = std::fs::create_dir_all(dir);
    let _ = atomic_write_json(&dir.join(format!("{name}.json")), msgs);

    let snap = dir.join(format!("{name}.todo.json"));
    match workdir.map(todo_file) {
        Some(tj) if tj.exists() => {
            let _ = std::fs::copy(&tj, &snap);
        }
        _ => {
            if snap.exists() {
                let _ = std::fs::remove_file(&snap);
            }
        }
    }

    let task = msgs
        .iter()
        .find(|m| m.role == Role::User && m.tool_calls.is_empty())
        .map(|m| text::head(&m.content, 200).to_string())
        .unwrap_or_default();
    let mut upd = Map::new();
    upd.insert(
        "updated".into(),
        Value::String(chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()),
    );
    upd.insert("task".into(), Value::String(task));
    upd.insert("status".into(), Value::String(session_status(msgs).into()));
    upd.insert("msgs".into(), Value::from(msgs.len()));
    if let Some(wd) = workdir {
        let abs = std::path::absolute(wd).unwrap_or_else(|_| wd.to_path_buf());
        upd.insert(
            "workdir".into(),
            Value::String(abs.to_string_lossy().into_owned()),
        );
    }
    merge_session_meta(dir, name, upd);
}
