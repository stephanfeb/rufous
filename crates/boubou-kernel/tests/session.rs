//! The transcript cursor across process boundaries: a continued session (editor, web page,
//! `--append`) builds a new `Transcript` per turn, and the cursor must survive a context
//! that has been shortened by compaction (review 2026-10-29).

use std::path::Path;

use boubou_kernel::agent::session::{load_session_meta, save_session, Transcript};
use boubou_kernel::ollama::{Message, Role};
use serde_json::json;

const S: &str = "acp_test";

fn lines(dir: &Path) -> Vec<Message> {
    std::fs::read_to_string(dir.join(format!("{S}.transcript.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("line"))
        .collect()
}

fn cursor(dir: &Path) -> Option<u64> {
    load_session_meta(dir, S)
        .get("transcript_next")
        .and_then(serde_json::Value::as_u64)
}

/// A first turn: six messages flushed, then compacted to four with a summary at index 1.
fn first_turn_compacted(dir: &Path) -> Vec<Message> {
    let mut t = Transcript::new();
    let full = vec![
        Message::system("sys"),
        Message::user("task"),
        Message::assistant_call("read_file", json!({"path": "a"})),
        Message::tool_output("a-contents"),
        Message::assistant_call("read_file", json!({"path": "b"})),
        Message::tool_output("b-contents"),
    ];
    t.flush(dir, Some(S), &full);
    assert_eq!(lines(dir).len(), 6);
    t.mark_compacted();
    let compacted = vec![
        Message::system("sys"),
        Message::user("[Earlier context summary] read a and b"),
        full[4].clone(),
        full[5].clone(),
    ];
    t.flush(dir, Some(S), &compacted);
    assert_eq!(lines(dir).len(), 7, "the summary is logged");
    save_session(dir, Some(S), &compacted, None, None, &mut t);
    assert_eq!(cursor(dir), Some(4));
    compacted
}

#[test]
fn a_continued_session_after_compaction_still_logs_the_new_user_turn() {
    let d = tempfile::tempdir().expect("tmp");
    let mut ctx = first_turn_compacted(d.path());
    // Next turn, new process: the saved context plus the user's follow-up.
    ctx.push(Message::user("now do the same for c"));
    let mut fresh = Transcript::new();
    fresh.flush(d.path(), Some(S), &ctx);
    let on_disk = lines(d.path());
    assert_eq!(on_disk.len(), 8, "{on_disk:#?}");
    assert_eq!(on_disk[7].role, Role::User);
    assert_eq!(on_disk[7].content, "now do the same for c");
    assert_eq!(cursor(d.path()), Some(5));
    // The model's reply lands after it, in order.
    ctx.push(Message::assistant("done with c"));
    fresh.flush(d.path(), Some(S), &ctx);
    assert_eq!(
        lines(d.path()).last().map(|m| m.content.as_str()),
        Some("done with c")
    );
}

#[test]
fn a_compaction_to_the_same_length_still_logs_the_summary() {
    let d = tempfile::tempdir().expect("tmp");
    let mut t = Transcript::new();
    let full = vec![
        Message::system("sys"),
        Message::user("task"),
        Message::assistant("thinking aloud"),
        Message::assistant("more"),
    ];
    t.flush(d.path(), Some(S), &full);
    t.mark_compacted();
    let same_len = vec![
        Message::system("sys"),
        Message::user("[Earlier context summary] x"),
        full[2].clone(),
        full[3].clone(),
    ];
    t.flush(d.path(), Some(S), &same_len);
    let on_disk = lines(d.path());
    assert_eq!(on_disk.len(), 5);
    assert!(on_disk[4].content.starts_with("[Earlier context summary]"));
}

#[test]
fn a_legacy_meta_without_the_cursor_is_read_as_saved_context_plus_one_turn() {
    let d = tempfile::tempdir().expect("tmp");
    let mut ctx = first_turn_compacted(d.path());
    // A meta written before the cursor existed: `msgs` only.
    let meta_path = d.path().join(format!("{S}.meta.json"));
    std::fs::write(&meta_path, json!({"msgs": 4}).to_string()).expect("meta");
    ctx.push(Message::user("follow-up"));
    Transcript::new().flush(d.path(), Some(S), &ctx);
    assert_eq!(lines(d.path()).len(), 8);
    assert_eq!(lines(d.path())[7].content, "follow-up");
}

#[test]
fn a_kill_resume_with_a_stale_save_does_not_duplicate_the_run() {
    let d = tempfile::tempdir().expect("tmp");
    // Seven lines on disk, a save from long ago (msgs 2), and a checkpoint of nine messages:
    // only the two unflushed messages may be appended.
    let mut t = Transcript::new();
    let mut ctx: Vec<Message> = (0..7).map(|i| Message::user(format!("m{i}"))).collect();
    t.flush(d.path(), Some(S), &ctx);
    std::fs::write(
        d.path().join(format!("{S}.meta.json")),
        json!({"msgs": 2}).to_string(),
    )
    .expect("meta");
    ctx.push(Message::user("m7"));
    ctx.push(Message::user("m8"));
    Transcript::new().flush(d.path(), Some(S), &ctx);
    let on_disk = lines(d.path());
    assert_eq!(on_disk.len(), 9, "{on_disk:#?}");
    assert_eq!(on_disk[8].content, "m8");
}

#[test]
fn a_persisted_cursor_beats_the_line_count_on_a_checkpoint_resume() {
    let d = tempfile::tempdir().expect("tmp");
    let mut t = Transcript::new();
    let mut ctx: Vec<Message> = (0..6).map(|i| Message::user(format!("m{i}"))).collect();
    t.flush(d.path(), Some(S), &ctx);
    assert_eq!(cursor(d.path()), Some(6));
    // Killed after two more messages reached the checkpoint but before the next flush.
    ctx.push(Message::assistant("a"));
    ctx.push(Message::tool_output("t"));
    Transcript::new().flush(d.path(), Some(S), &ctx);
    assert_eq!(lines(d.path()).len(), 8);
    assert_eq!(cursor(d.path()), Some(8));
}
