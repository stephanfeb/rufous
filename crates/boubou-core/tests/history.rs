//! `compact_history` plan re-injection (`tests/test_todo.py::TestCompactKeepsPlan`),
//! `_dedupe_trailing_assistant`, `sanitize_ckpt`, `_similar`, pending-image attachment.

use boubou_core::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use boubou_core::agent::history::{
    attach_pending_images, compact_history, dedupe_trailing_assistant, sanitize_ckpt, similar,
};
use boubou_core::ollama::{Message, Role};
use serde_json::{json, Value};

struct Summariser;

impl ChatBackend for Summariser {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        Ok(ChatResponse::text("summary"))
    }
}

struct Broken;

impl ChatBackend for Broken {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        Err(ChatError::Status {
            status: 500,
            body: "boom".into(),
        })
    }
}

fn msgs(n: usize) -> Vec<Message> {
    let mut v = vec![Message::system("SYS")];
    v.extend((0..n).map(|k| Message::assistant(format!("turn {k}"))));
    v
}

fn joined(out: &[Message]) -> String {
    out.iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn l3_reinjects_plan() {
    let mut log = Vec::new();
    let c = compact_history(
        &mut Summariser,
        "m",
        msgs(8),
        3,
        Some("[ ] 1. a\n[x] 2. b"),
        None,
        &mut |s| log.push(s),
    );
    let j = joined(&c.messages);
    assert!(j.contains("[Current plan (todo)") && j.contains("[x] 2. b"));
    assert!(c.summarised);
    assert_eq!(c.messages.len(), 5); // system + summary + last 3
    assert!(log.iter().any(|l| l.contains("L3")));
}

#[test]
fn l2_reinjects_plan() {
    let c = compact_history(
        &mut Summariser,
        "m",
        msgs(8),
        2,
        Some("[ ] 1. a"),
        None,
        &mut |_| {},
    );
    assert!(joined(&c.messages).contains("[ ] 1. a"));
    assert_eq!(c.messages.len(), 7); // system + summary + last 5
}

#[test]
fn no_plan_no_marker() {
    let c = compact_history(&mut Summariser, "m", msgs(8), 3, None, None, &mut |_| {});
    assert!(!joined(&c.messages).contains("[Current plan (todo)"));
}

#[test]
fn files_text_rides_along() {
    let c = compact_history(
        &mut Summariser,
        "m",
        msgs(8),
        3,
        None,
        Some("a.md\nb.py"),
        &mut |_| {},
    );
    let j = joined(&c.messages);
    assert!(j.contains("[Files already in the working directory") && j.contains("b.py"));
}

#[test]
fn l1_truncates_old_tool_output_only() {
    let mut v = vec![Message::system("SYS"), Message::user("task")];
    v.push(Message::tool_output("x".repeat(500)));
    v.push(Message::assistant("thinking about it"));
    v.push(Message::tool_output("y".repeat(500)));
    v.push(Message::assistant("done"));
    let c = compact_history(&mut Broken, "m", v, 1, None, None, &mut |_| {});
    assert!(!c.summarised);
    assert!(c.messages[2]
        .content
        .contains("truncated, originally 500 chars"));
    assert_eq!(c.messages[4].content.len(), 500); // last two untouched
}

#[test]
fn failed_summary_leaves_history_untouched_and_logs() {
    let mut log = Vec::new();
    let input = msgs(8);
    let c = compact_history(&mut Broken, "m", input.clone(), 3, None, None, &mut |s| {
        log.push(s)
    });
    assert_eq!(c.messages, input);
    assert!(!c.summarised);
    assert!(log.iter().any(|l| l.contains("failed")));
}

#[test]
fn short_histories_are_left_alone() {
    let c = compact_history(&mut Summariser, "m", msgs(3), 3, None, None, &mut |_| {});
    assert_eq!(c.messages.len(), 4);
    let c = compact_history(&mut Summariser, "m", msgs(5), 2, None, None, &mut |_| {});
    assert_eq!(c.messages.len(), 6);
}

#[test]
fn dedupe_trailing_assistants() {
    let mut v = vec![
        Message::user("q"),
        Message::assistant("real"),
        Message::assistant(""),
    ];
    dedupe_trailing_assistant(&mut v);
    assert_eq!(v.len(), 2);
    assert_eq!(v[1].content, "real");

    let mut v = vec![
        Message::user("q"),
        Message::assistant(""),
        Message::assistant("real"),
    ];
    dedupe_trailing_assistant(&mut v);
    assert_eq!(v.len(), 2);
    assert_eq!(v[1].content, "real");

    let mut v = vec![
        Message::user("q"),
        Message::assistant("a"),
        Message::assistant_call("finish", json!({"summary": "s"})),
    ];
    dedupe_trailing_assistant(&mut v);
    assert_eq!(v.len(), 2);
    assert_eq!(v[1].content, "a\n");
    assert_eq!(v[1].tool_calls.len(), 1);
}

#[test]
fn sanitize_checkpoint_tail() {
    let mut v = vec![
        Message::system("s"),
        Message::user("t"),
        Message::assistant("ok"),
        Message::assistant_call("run_bash", json!({"command": "ls"})),
    ];
    sanitize_ckpt(&mut v);
    assert_eq!(v.len(), 3);
    assert_eq!(v[2].content, "ok");

    let mut v = vec![
        Message::user("t"),
        Message::assistant("response:unknown{...}"),
    ];
    sanitize_ckpt(&mut v);
    assert_eq!(v.len(), 1);

    let mut v = vec![Message::user("t")];
    let mut keep = Message::assistant_call("finish", json!({}));
    keep.content = "kept body".into();
    v.push(keep);
    sanitize_ckpt(&mut v);
    assert_eq!(v.len(), 2);
    assert!(v[1].tool_calls.is_empty());
}

#[test]
fn similar_detects_repeats() {
    assert!(similar(
        "I will start implementing this feature, first creating the file.",
        "I will start implementing this feature,  first creating the file.",
        0.75
    ));
    assert!(!similar(
        "Completely different content A",
        "another thing entirely B",
        0.75
    ));
    assert!(!similar("", "x", 0.75));
}

#[test]
fn pending_images_attach_to_last_tool_message() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(
        d.path().join(".hb_pending_images.json"),
        r#"[{"path":"a.png","b64":"QUJD"}]"#,
    )
    .expect("write");
    let mut v = vec![
        Message::system("s"),
        Message::user("look"),
        Message::assistant_call("read_file", json!({"path": "a.png"})),
        Message::tool_output("[image queued]"),
    ];
    assert_eq!(attach_pending_images(&mut v, d.path()), 1);
    assert_eq!(v[3].images, vec!["QUJD".to_string()]);
    assert_eq!(v[3].role, Role::Tool);
    let left = std::fs::read_to_string(d.path().join(".hb_pending_images.json")).expect("read");
    assert_eq!(left, "[]");
    assert_eq!(attach_pending_images(&mut v, d.path()), 0);
}

#[test]
fn message_json_is_lenient_on_nulls() {
    let m: Message = serde_json::from_str(
        r#"{"role":"assistant","content":null,"tool_calls":null,"reasoning_content":"hm"}"#,
    )
    .expect("json");
    assert_eq!(m.content, "");
    assert!(m.tool_calls.is_empty());
    assert_eq!(m.thinking.as_deref(), Some("hm"));
    let s = serde_json::to_string(&Message::tool_output("r")).expect("json");
    assert_eq!(s, r#"{"role":"tool","content":"r"}"#);
}
