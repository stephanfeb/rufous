//! Sessions, transcript and checkpoint, byte-compatible with Mingbird so an interrupted
//! Mingbird task resumes under rufous and vice versa.
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
//!   rufous adds `transcript_next`, the transcript cursor (see [`Transcript::flush`]), and
//!   `policy`, the name of the turn policy the last turn ran under (see
//!   [`recorded_policy`]). Mingbird merges into this file rather than rewriting it, so
//!   both keys survive a Mingbird turn on the same session.
//! - `<sessions>/<name>.todo.json`: snapshot of the plan, bound to the session.
//! - `<sessions>/<name>.files/`: the harness directory of a relocated session (ACP):
//!   `todo.json`, `backups/` (previous versions of edited files, the CLI's `<file>.bak`),
//!   `trash/` (deleted files, the CLI's `.mingbird_trash/`) and the image queue.
//! - `<sessions>/<name>.decisions.jsonl`: one line per System One decision when the decider
//!   is in `shadow` or `on` mode (rufous addition, bead boubou-acy.3); the labelled-set
//!   builder ingests these as `shadow:<name>` candidates.
//! - `<sessions>/<name>.log`: the loop's protocol lines (`⚙` calls, `[ctx: …]`, `🐚`, `⚠️`,
//!   `[prompt]`, `[hooks]`), each behind a wall-clock timestamp, plus warnings mirrored
//!   from tracing; written by [`SessionLog`] on every surface that names its session
//!   (rufous addition, bead boubou-bmq), so a live editor session is read by the same
//!   bench parser as a CLI run. `AGENT_SESSION_LOG=0` switches it off.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use serde_json::{Map, Value};

use super::AgentEvent;

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

/// The harness directory of a relocated session: `<sessions>/<name>.files/`
/// ([`RunParams::harness_dir`](crate::agent::RunParams::harness_dir)).
pub fn harness_dir(sessions_dir: &Path, name: &str) -> PathBuf {
    sessions_dir.join(format!("{name}.files"))
}

/// `<sessions>/<name>.log`: the session's protocol log ([`SessionLog`]).
pub fn log_path(sessions_dir: &Path, name: &str) -> PathBuf {
    sessions_dir.join(format!("{name}.log"))
}

/// The switch for the session log: `AGENT_SESSION_LOG` (`1/0`, `on/off`, …), on by default.
pub fn session_log_enabled() -> bool {
    super::parse_switch(
        "AGENT_SESSION_LOG",
        std::env::var("AGENT_SESSION_LOG").ok().as_deref(),
    )
    .unwrap_or(true)
}

struct LogFile {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

/// Every open session log, so a warning raised anywhere in the process (tracing) can be
/// mirrored into the session it belongs to; dead entries are pruned on each broadcast.
static OPEN_LOGS: Mutex<Vec<Weak<LogFile>>> = Mutex::new(Vec::new());

/// The session's protocol log: the lines the CLI prints and the bench parses (`⚙`, `[ctx:
/// …]`, `🐚`, `⚠️`, the compaction and finish banners), appended to `<sessions>/<name>.log`
/// behind a `YYYY-MM-DD HH:MM:SS` timestamp, flushed per line. Under an editor nothing
/// else keeps these lines (the ACP bridge has no channel for them and tracing is at WARN),
/// so this file is what makes a live session measurable (bead boubou-bmq). Cloning shares
/// the file.
#[derive(Clone)]
pub struct SessionLog(Arc<LogFile>);

impl std::fmt::Debug for SessionLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLog")
            .field("path", &self.0.path)
            .finish()
    }
}

impl SessionLog {
    /// Open (append) the log for `name`; `None` when the switch is off or the file cannot
    /// be opened (a warning says why; the run goes on without it).
    pub fn open(sessions_dir: &Path, name: &str) -> Option<Self> {
        if !session_log_enabled() {
            return None;
        }
        Self::open_always(sessions_dir, name)
    }

    /// [`SessionLog::open`] without the switch (tests).
    pub fn open_always(sessions_dir: &Path, name: &str) -> Option<Self> {
        let path = log_path(sessions_dir, name);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(file) => {
                let inner = Arc::new(LogFile {
                    path,
                    file: Mutex::new(file),
                });
                if let Ok(mut open) = OPEN_LOGS.lock() {
                    open.retain(|w| w.strong_count() > 0);
                    open.push(Arc::downgrade(&inner));
                }
                Some(Self(inner))
            }
            Err(e) => {
                tracing::warn!(target: "rufous_kernel::session", "session log {}: {e}", path.display());
                None
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// Append one entry: each of its non-empty lines behind a timestamp.
    pub fn line(&self, text: &str) {
        self.0.write(text);
    }

    /// What one [`AgentEvent`] contributes: the protocol lines (`Log`, which already carry
    /// the `⚙` calls, `[ctx: …]` and the end banners) and errors. Streamed text, tool
    /// receipts and context usage are not written twice.
    pub fn record(&self, ev: &AgentEvent) {
        match ev {
            AgentEvent::Log(s) => self.line(s),
            AgentEvent::Error(e) => self.line(&format!("[error] {e}")),
            _ => {}
        }
    }

    /// A sink that records each event here and then hands it to `inner`.
    pub fn tee(
        self,
        mut inner: impl FnMut(AgentEvent) + Send + 'static,
    ) -> impl FnMut(AgentEvent) + Send + 'static {
        move |ev| {
            self.record(&ev);
            inner(ev);
        }
    }
}

impl LogFile {
    fn write(&self, text: &str) {
        if let Ok(mut f) = self.file.lock() {
            // Each non-empty line of a multi-line entry (the finish banner) gets the
            // stamp, so the file stays parseable line by line.
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            for l in text.lines().filter(|l| !l.trim().is_empty()) {
                let _ = writeln!(f, "{ts} {l}");
            }
            let _ = f.flush();
        }
    }
}

/// Mirror a warning (tracing `WARN`/`ERROR`, formatted by the caller) into every open
/// session log as `[warn] …`, so the skills and fs messages sit in sequence with the calls.
pub fn mirror_warning(line: &str) {
    let Ok(mut open) = OPEN_LOGS.lock() else {
        return;
    };
    open.retain(|w| w.strong_count() > 0);
    for w in open.iter() {
        if let Some(l) = w.upgrade() {
            l.write(&format!("[warn] {line}"));
        }
    }
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

/// The turn policy the session last ran under, as `save_session` recorded it in the meta
/// file. `None` for a session Mingbird wrote or one saved before the key existed. The loop
/// compares it with the policy of the turn about to run and says so when they differ, so a
/// session resumed on another surface (web chat, editor, `rufous run`) never changes
/// harness silently.
pub fn recorded_policy(dir: &Path, name: &str) -> Option<String> {
    load_session_meta(dir, name)
        .get("policy")
        .and_then(Value::as_str)
        .map(str::to_string)
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
    let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(p)
    else {
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

/// Session status: the loop's own word on how it ended (`done`, `max-iter`), or `done` when
/// the model called `finish`, else `running`. Until 2026-10-31 (bead boubou-bj4) this
/// searched every message for the substrings `TASK_COMPLETE` and `MAX ITERATIONS`, so a
/// pasted log or an echoed banner set the status.
fn session_status(end: Option<&str>, msgs: &[Message]) -> String {
    if let Some(e) = end {
        return e.to_string();
    }
    if msgs
        .iter()
        .any(|m| m.tool_calls.iter().any(|tc| tc.function.name == "finish"))
    {
        return "done".to_string();
    }
    "running".to_string()
}

/// `save_session`: transcript first, then the context JSON, the todo snapshot and meta.
/// `policy` is the name of the turn policy this turn ran under, recorded in the meta file;
/// `end` is how the loop ended (`done`, `max-iter`), `None` while it runs.
#[allow(clippy::too_many_arguments)]
pub fn save_session(
    dir: &Path,
    name: Option<&str>,
    msgs: &[Message],
    workdir: Option<&Path>,
    plan_dir: Option<&Path>,
    policy: &str,
    end: Option<&str>,
    transcript: &mut Transcript,
) {
    let Some(name) = name else {
        return;
    };
    transcript.flush(dir, Some(name), msgs);
    let _ = std::fs::create_dir_all(dir);
    let _ = atomic_write_json(&dir.join(format!("{name}.json")), msgs);

    let snap = dir.join(format!("{name}.todo.json"));
    match plan_dir.map(todo_file) {
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
    upd.insert("status".into(), Value::String(session_status(end, msgs)));
    upd.insert("msgs".into(), Value::from(msgs.len()));
    upd.insert("policy".into(), Value::String(policy.to_string()));
    if let Some(wd) = workdir {
        let abs = std::path::absolute(wd).unwrap_or_else(|_| wd.to_path_buf());
        upd.insert(
            "workdir".into(),
            Value::String(abs.to_string_lossy().into_owned()),
        );
    }
    merge_session_meta(dir, name, upd);
}

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn the_session_log_keeps_the_protocol_lines_behind_a_timestamp() {
        let d = tempfile::tempdir().expect("tmp");
        let log = SessionLog::open_always(d.path(), "acp_test").expect("open");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let inner = seen.clone();
        let mut sink = log
            .clone()
            .tee(move |ev| inner.lock().expect("lock").push(ev));
        sink(AgentEvent::Log(
            "[0|+1s] ⚙ list_dir {\"path\":\".\"} -> a".into(),
        ));
        sink(AgentEvent::Text("streamed".into()));
        sink(AgentEvent::ContextUsage {
            used: 10,
            limit: 100,
        });
        sink(AgentEvent::Error("boom".into()));
        sink(AgentEvent::Finished {
            summary: "done".into(),
        });
        sink(AgentEvent::Log(
            "\n===== TASK COMPLETE =====\n[TASK_COMPLETE] done".into(),
        ));
        mirror_warning("skills: skipping x");
        let text = std::fs::read_to_string(log_path(d.path(), "acp_test")).expect("read");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5, "{text}");
        for l in &lines {
            assert!(
                l.len() > 20 && l.as_bytes()[4] == b'-' && l.as_bytes()[19] == b' ',
                "timestamp prefix: {l}"
            );
        }
        assert!(lines[0].ends_with("⚙ list_dir {\"path\":\".\"} -> a"));
        assert!(lines[1].ends_with("[error] boom"));
        assert!(lines[2].ends_with("===== TASK COMPLETE ====="));
        assert!(lines[3].ends_with("[TASK_COMPLETE] done"));
        assert!(lines[4].ends_with("[warn] skills: skipping x"));
        assert!(!text.contains("streamed"), "streamed text is not logged");
        assert_eq!(
            seen.lock().expect("lock").len(),
            6,
            "every event still reaches the inner sink"
        );
        drop(sink);
        drop(log);
        mirror_warning("after close");
        let text = std::fs::read_to_string(log_path(d.path(), "acp_test")).expect("read");
        assert!(
            !text.contains("after close"),
            "a closed log gets no mirrored warnings"
        );
    }

    #[test]
    fn the_switch_reads_the_usual_words() {
        assert_eq!(
            super::super::parse_switch("AGENT_SESSION_LOG", Some("0")),
            Some(false)
        );
        assert_eq!(
            super::super::parse_switch("AGENT_SESSION_LOG", Some("off")),
            Some(false)
        );
        assert_eq!(super::super::parse_switch("AGENT_SESSION_LOG", None), None);
    }
}
