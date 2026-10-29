//! `compact_history` plan re-injection (`tests/test_todo.py::TestCompactKeepsPlan`),
//! `_dedupe_trailing_assistant`, `sanitize_ckpt`, `_similar`, pending-image attachment.

use boubou_core::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use boubou_core::agent::history::{
    attach_pending_images, compact_history, dedupe_trailing_assistant, prune_history,
    sanitize_ckpt, similar, PRUNED_MARK,
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

// ---- recency pruning (bead boubou-65d.6) ----

fn call_pair(i: usize, tool: &str, args: Value, result: &str) -> [Message; 2] {
    let mut a = Message::assistant_call(tool, args);
    a.content = format!("step {i}");
    a.thinking = Some("long private reasoning".repeat(20));
    [a, Message::tool_output(result)]
}

fn history_with_pairs(n: usize) -> Vec<Message> {
    let mut msgs = vec![Message::system("sys"), Message::user("task")];
    for i in 0..n {
        let big = format!(
            "line one of result {i}\n{}\n60 passed, 0 failed",
            "x".repeat(2000)
        );
        msgs.extend(call_pair(
            i,
            "run_bash",
            json!({"command": format!("python run_tests.py # {i}")}),
            &big,
        ));
    }
    msgs
}

#[test]
fn pruning_keeps_the_last_k_results_verbatim_and_collapses_older_ones_to_facts() {
    let mut msgs = history_with_pairs(8);
    let before = msgs.len();
    assert_eq!(prune_history(&mut msgs, 5), 3);
    assert_eq!(
        msgs.len(),
        before,
        "the shape stays: every call keeps its result"
    );
    let tools: Vec<&Message> = msgs.iter().filter(|m| m.role == Role::Tool).collect();
    for (i, t) in tools.iter().enumerate().take(3) {
        assert!(t.content.starts_with(PRUNED_MARK), "{i}: {}", t.content);
        assert!(
            t.content.contains("run_bash(python run_tests.py"),
            "{}",
            t.content
        );
        assert!(t.content.contains("line one of result"), "{}", t.content);
        assert!(
            t.content.contains("60 passed, 0 failed"),
            "the last line survives: {}",
            t.content
        );
        assert!(t.content.len() < 400);
    }
    for t in &tools[3..] {
        assert!(
            t.content.contains(&"x".repeat(2000)),
            "recent results are verbatim"
        );
    }
    // The owning assistant turns lost their thinking, the recent ones kept it.
    let assts: Vec<&Message> = msgs.iter().filter(|m| m.role == Role::Assistant).collect();
    assert!(assts[..3].iter().all(|a| a.thinking.is_none()));
    assert!(assts[3..].iter().all(|a| a.thinking.is_some()));
    assert!(
        assts.iter().all(|a| a.content.starts_with("step ")),
        "prose stays"
    );
}

#[test]
fn pruning_is_idempotent_and_leaves_short_histories_alone() {
    let mut msgs = history_with_pairs(5);
    assert_eq!(prune_history(&mut msgs, 5), 0);
    let mut msgs = history_with_pairs(9);
    assert_eq!(prune_history(&mut msgs, 5), 4);
    let snapshot = msgs.clone();
    assert_eq!(prune_history(&mut msgs, 5), 0);
    assert_eq!(msgs, snapshot);
    // One more pair later: exactly one more result is pruned, the facts already made stay.
    msgs.extend(call_pair(9, "list_dir", json!({"path": "."}), "a.py\nb.py"));
    assert_eq!(prune_history(&mut msgs, 5), 1);
    assert_eq!(
        msgs.iter()
            .filter(|m| m.content.starts_with(PRUNED_MARK))
            .count(),
        5
    );
}

#[test]
fn pruning_shrinks_long_arguments_of_old_calls_and_keeps_facts_for_each_tool() {
    let mut msgs = vec![Message::system("sys"), Message::user("task")];
    let body = "def f():\n    return 1\n".repeat(50);
    msgs.extend(call_pair(
        0,
        "create_file",
        json!({"path": "report.md", "content": body}),
        "[created report.md (1200 bytes)]",
    ));
    msgs.extend(call_pair(
        1,
        "edit_file",
        json!({"path": "m.py", "old": "a".repeat(500), "new": "b".repeat(500)}),
        "[edited m.py (backed up); .bak backup available]",
    ));
    msgs.extend(call_pair(
        2,
        "search_files",
        json!({"path": ".", "pattern": "def tier"}),
        "pricing.py:1425: def tier_for_volume",
    ));
    for i in 3..8 {
        msgs.extend(call_pair(
            i,
            "list_dir",
            json!({"path": format!("d{i}")}),
            "f.txt",
        ));
    }
    assert_eq!(prune_history(&mut msgs, 5), 3);
    let tools: Vec<&Message> = msgs.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(
        tools[0].content,
        "[earlier] create_file(report.md) → [created report.md (1200 bytes)]"
    );
    assert_eq!(
        tools[1].content,
        "[earlier] edit_file(m.py) → [edited m.py (backed up); .bak backup available]"
    );
    assert_eq!(
        tools[2].content,
        "[earlier] search_files(.) → pricing.py:1425: def tier_for_volume"
    );
    let assts: Vec<&Message> = msgs.iter().filter(|m| m.role == Role::Assistant).collect();
    let content = assts[0].tool_calls[0].function.arguments["content"]
        .as_str()
        .expect("string arg");
    assert!(
        content.ends_with("… (1100 chars)") && content.len() < 120,
        "{content}"
    );
    assert_eq!(
        assts[0].tool_calls[0].function.arguments["path"], "report.md",
        "short args untouched"
    );
    let old = assts[1].tool_calls[0].function.arguments["old"]
        .as_str()
        .expect("string arg");
    assert!(old.ends_with("… (500 chars)"));
    // A parallel-dispatch turn: two results under one assistant message get their own names.
    let mut msgs = vec![Message::system("sys"), Message::user("task")];
    let mut a = Message::assistant_call("read_file", json!({"path": "a.txt"}));
    a.tool_calls.push(boubou_core::ollama::ToolCall {
        function: boubou_core::ollama::FunctionCall {
            name: "read_file".into(),
            arguments: json!({"path": "b.txt"}),
        },
    });
    msgs.push(a);
    msgs.push(Message::tool_output("A".repeat(300)));
    msgs.push(Message::tool_output("B".repeat(300)));
    for i in 0..5 {
        msgs.extend(call_pair(
            i,
            "list_dir",
            json!({"path": format!("d{i}")}),
            "f.txt",
        ));
    }
    assert_eq!(prune_history(&mut msgs, 5), 2);
    assert!(msgs[3]
        .content
        .starts_with("[earlier] read_file(a.txt) → AAAA"));
    assert!(msgs[4]
        .content
        .starts_with("[earlier] read_file(b.txt) → BBBB"));
}
