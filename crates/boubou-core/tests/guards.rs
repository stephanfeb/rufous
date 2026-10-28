//! Port of `tests/test_guards.py` (pure-logic parts), `test_output_cap.py` (same) and the
//! todo-related heuristics from `test_todo.py`. Loop-wiring tests wait for the agent loop.

use std::path::Path;

use boubou_core::guards::artifacts::{claimed_missing_files, plan_named_missing};
use boubou_core::guards::edit::EditConvergenceGuard;
use boubou_core::guards::leak::try_parse_tool_calls;
use boubou_core::guards::qa::{has_task_progress, is_qa, task_original_text};
use boubou_core::guards::{
    estimate_tokens, format_bash_out, is_small_model, model_params_b, semantically_empty,
};
use boubou_core::ollama::Message;
use boubou_core::todo::{save_todo, TodoItem};
use boubou_core::tools;
use serde_json::{json, Map, Value};

fn touch(dir: &Path, rel: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(p, "x").expect("write");
}

fn call(name: &str, args: Value) -> (String, Map<String, Value>) {
    (
        name.to_string(),
        args.as_object().cloned().unwrap_or_default(),
    )
}

// ---- _model_params_b ----

#[test]
fn params_parsing() {
    assert_eq!(model_params_b("gemma4:e2b"), Some(2.0));
    assert_eq!(model_params_b("qwen3.5:4b"), Some(4.0));
    assert_eq!(model_params_b("gemma4:12b"), Some(12.0));
    assert_eq!(model_params_b("ornith-1.5:35b"), Some(35.0));
    assert_eq!(model_params_b("unknown-model"), None);
}

#[test]
fn small_model_boundary() {
    assert!(is_small_model("gemma4:e2b") && is_small_model("qwen3.5:4b"));
    assert!(!is_small_model("gemma4:12b") && !is_small_model("ornith-1.5:35b"));
    assert!(!is_small_model("unknown-model")); // unparseable → treated as large
}

// ---- _claimed_missing_files ----

#[test]
fn claim_missing_detected() {
    let d = tempfile::tempdir().expect("tmp");
    touch(d.path(), "report.md");
    assert_eq!(
        claimed_missing_files("wrote report.md and anomalies.md", d.path()),
        vec!["anomalies.md"]
    );
}

#[test]
fn claim_all_exist_subdir_and_glued_prefix() {
    let d = tempfile::tempdir().expect("tmp");
    touch(d.path(), "report.md");
    assert!(claimed_missing_files("wrote report.md", d.path()).is_empty());
    touch(d.path(), "sub/fig1.png");
    assert!(claimed_missing_files("generated sub/fig1.png", d.path()).is_empty());
    // A basename glued to a word without a space still resolves (suffix match).
    touch(d.path(), "plot.png");
    assert!(claimed_missing_files("generated:plot.png", d.path()).is_empty());
}

#[test]
fn claim_no_false_positive_numbers() {
    let d = tempfile::tempdir().expect("tmp");
    assert!(
        claimed_missing_files("accuracy 95.3%, version v1.2.3, took 2.5s", d.path()).is_empty()
    );
}

#[test]
fn claim_input_fixture_mention() {
    // Mentioning an input file that exists is not a missing artefact.
    let d = tempfile::tempdir().expect("tmp");
    touch(d.path(), "data/input.csv");
    assert!(claimed_missing_files(
        "read input.csv and generated statistics in data/input.csv",
        d.path()
    )
    .is_empty());
}

// ---- _plan_named_missing ----

#[test]
fn plan_named_missing_checks_filesystem_not_checkboxes() {
    let d = tempfile::tempdir().expect("tmp");
    assert!(plan_named_missing(d.path()).is_empty()); // no todo.json
    save_todo(
        d.path(),
        &[
            TodoItem::new("write report.md"),
            TodoItem::new("run the tests to verify"),
        ],
    )
    .expect("save");
    assert_eq!(plan_named_missing(d.path()), vec!["report.md"]);
    touch(d.path(), "report.md");
    assert!(plan_named_missing(d.path()).is_empty());

    let d2 = tempfile::tempdir().expect("tmp");
    save_todo(
        d2.path(),
        &[TodoItem {
            item: "generate change_log.md".into(),
            done: true,
        }],
    )
    .expect("save");
    assert_eq!(plan_named_missing(d2.path()), vec!["change_log.md"]);

    let d3 = tempfile::tempdir().expect("tmp");
    save_todo(
        d3.path(),
        &[TodoItem::new(
            "verify the 3.5b model output in v1.1.0 format",
        )],
    )
    .expect("save");
    assert!(plan_named_missing(d3.path()).is_empty());
}

// ---- _EditConvergenceGuard ----

#[test]
fn shotgun_hint_at_4th_then_capped_at_two() {
    let mut g = EditConvergenceGuard::default();
    let outs: Vec<Option<String>> = (0..9).map(|_| g.observe("m08.py")).collect();
    assert!(outs[0].is_none() && outs[1].is_none() && outs[2].is_none());
    assert!(outs[3]
        .as_deref()
        .is_some_and(|h| h.contains("successfully 4 times")));
    assert!(outs[4]
        .as_deref()
        .is_some_and(|h| h.contains("root-cause hypothesis")));
    assert!(outs[5..].iter().all(Option::is_none));
    assert_eq!(outs.iter().flatten().count(), 2);
}

#[test]
fn shotgun_alternating_and_reset() {
    let mut g = EditConvergenceGuard::default();
    assert!(["a.py", "b.py"]
        .iter()
        .cycle()
        .take(10)
        .all(|p| g.observe(p).is_none()));
    let mut g = EditConvergenceGuard::default();
    for _ in 0..3 {
        assert!(g.observe("p.py").is_none());
    }
    assert!(g.observe("q.py").is_none());
    assert!(g.observe("p.py").is_none()); // sequence restarted
}

#[test]
fn shotgun_path_normalization() {
    let mut g = EditConvergenceGuard::default();
    let outs: Vec<_> = ["m08.py", "./m08.py", "M08.PY", "m08.py"]
        .iter()
        .map(|p| g.observe(p))
        .collect();
    assert!(outs[..3].iter().all(Option::is_none));
    assert!(outs[3].is_some());
    let mut g2 = EditConvergenceGuard::default();
    assert!(["sub\\a.py", "sub/a.py", "./sub/a.py"]
        .iter()
        .all(|p| g2.observe(p).is_none()));
    assert!(g2.observe("SUB\\A.py").is_some());
}

#[test]
fn shotgun_per_file_hint_budget() {
    let mut g = EditConvergenceGuard::default();
    let first = (0..4)
        .map(|_| g.observe("p.py"))
        .last()
        .flatten()
        .expect("first hint");
    assert!(!first.contains("root-cause hypothesis"));
    assert!((0..4).map(|_| g.observe("q.py")).last().flatten().is_some());
    for _ in 0..3 {
        assert!(g.observe("p.py").is_none());
    }
    let second = g.observe("p.py").expect("second hint");
    assert!(second.contains("root-cause hypothesis"));
    assert!(g.observe("p.py").is_none());
    assert!(g.observe("q.py").is_none());
}

#[test]
fn shotgun_empty_path_safe_and_hint_actionable() {
    let mut g = EditConvergenceGuard::default();
    assert!(g.observe("").is_none() && g.observe("").is_none());
    let mut g = EditConvergenceGuard::default();
    let h = (0..4)
        .map(|_| g.observe("m08.py"))
        .last()
        .flatten()
        .expect("hint");
    for kw in ["run_bash", "root", ".bak"] {
        assert!(h.contains(kw), "{kw}");
    }
    assert!(!h.contains("disabled"));
}

// ---- _format_bash_out ----

#[test]
fn bash_out_formats() {
    assert_eq!(
        format_bash_out(0, "hello", "warn", false),
        "[exit 0]\nSTDOUT:\nhello\nSTDERR:\nwarn"
    );
    assert_eq!(
        format_bash_out(1, "short", "err", true),
        format_bash_out(1, "short", "err", false)
    );
    let x = "x".repeat(9000);
    let y = "y".repeat(3000);
    let full = format_bash_out(0, &x, &y, false);
    let slim = format_bash_out(0, &x, &y, true);
    assert!(slim.len() < full.len());
    assert!(slim.contains("trimmed") && slim.contains("read_file"));
    assert!(slim.contains(&"y".repeat(1200)));
    assert!(slim.contains(&"x".repeat(2500)) && !slim.contains(&"x".repeat(2501)));
    assert!(slim.ends_with("in parts with read_file.]"));
}

#[test]
fn exit_9009_receipt_hint() {
    let out = format_bash_out(9009, "", "", false);
    assert!(out.contains("9009") && out.contains("python3"));
    assert!(
        !format_bash_out(9009, "some real output", "", false).contains("cannot find the command")
    );
    assert!(!format_bash_out(0, "", "", false).contains("cannot find the command"));
}

// ---- _semantically_empty ----

#[test]
fn semantically_empty_cases() {
    assert!(semantically_empty(
        "<think>\nI need to continue implementing app.py"
    ));
    assert!(semantically_empty(
        "  <think>partial thought, cut off by length"
    ));
    assert!(!semantically_empty(
        "Write the plan first <think>think about the details"
    ));
    assert!(semantically_empty("<think>\n\n</think>"));
    assert!(semantically_empty("  <think></think>  "));
    assert!(semantically_empty(
        "<think>only thinking, no answer</think>"
    ));
    assert!(semantically_empty("<THINK></THINK>"));
    assert!(semantically_empty("") && semantically_empty("   "));
    assert!(!semantically_empty(
        "<think>reasoning</think>\nThe answer is 42"
    ));
    assert!(!semantically_empty("I will read the file"));
    assert!(!semantically_empty(
        "Write the plan first <think>truncated thinking, not closed"
    ));
    assert!(semantically_empty("<think>truncated thinking, not closed"));
}

// ---- try_parse_tool_calls ----

#[test]
fn parse_placeholder_quote_forms() {
    assert_eq!(
        try_parse_tool_calls(r#"finish(summary:<|"|>Task aborted: source file missing.<|"|>)"#),
        Some(vec![call(
            "finish",
            json!({"summary": "Task aborted: source file missing."})
        )])
    );
    assert_eq!(
        try_parse_tool_calls("I have already completed the task.\n\nfinish(summary:<|\"|>Task already completed: no input.<|\"|>)"),
        Some(vec![call("finish", json!({"summary": "Task already completed: no input."}))])
    );
    assert_eq!(
        try_parse_tool_calls("finish(summary:<|\"|>line1\nline2 with, commas<|\"|>)"),
        Some(vec![call(
            "finish",
            json!({"summary": "line1\nline2 with, commas"})
        )])
    );
    assert_eq!(
        try_parse_tool_calls(
            r#"finish{summary:<|"|>Cannot complete the calculation from the search results.<|"|>}"#
        ),
        Some(vec![call(
            "finish",
            json!({"summary": "Cannot complete the calculation from the search results."})
        )])
    );
    assert_eq!(
        try_parse_tool_calls("finish{summary:<|\"|>line1\nline2<|\"|>}"),
        Some(vec![call("finish", json!({"summary": "line1\nline2"}))])
    );
    // placeholder form keeps priority over the function-expression parser
    assert_eq!(
        try_parse_tool_calls(r#"finish(summary:<|"|>Task aborted.<|"|>)"#),
        Some(vec![call("finish", json!({"summary": "Task aborted."}))])
    );
}

#[test]
fn parse_json_and_shells() {
    assert_eq!(
        try_parse_tool_calls("```json\n{\"name\": \"create_file\", \"arguments\": {\"path\": \"a.py\", \"content\": \"x\"}}\n```"),
        Some(vec![call("create_file", json!({"path": "a.py", "content": "x"}))])
    );
    assert_eq!(
        try_parse_tool_calls("<tool_call>\n{\"create_file\": {\"path\": \"b.md\", \"content\": \"y\"}}\n</tool_call>"),
        Some(vec![call("create_file", json!({"path": "b.md", "content": "y"}))])
    );
    assert_eq!(
        try_parse_tool_calls(r#"{"finish": {"summary": "done"}}"#),
        Some(vec![call("finish", json!({"summary": "done"}))])
    );
    assert_eq!(
        try_parse_tool_calls(r#"{"name":"todo","arguments":{"action":"add","item":"x"}}"#),
        Some(vec![call("todo", json!({"action": "add", "item": "x"}))])
    );
}

#[test]
fn parse_prose_not_salvaged() {
    assert_eq!(
        try_parse_tool_calls("Let me look at the directory structure first."),
        None
    );
    assert_eq!(
        try_parse_tool_calls("Just a normal answer, no calls here."),
        None
    );
    assert_eq!(
        try_parse_tool_calls("call foo(bar=1) without placeholders"),
        None
    );
    assert_eq!(
        try_parse_tool_calls(r#"I will finish(summary="0") now"#),
        None
    );
    assert_eq!(
        try_parse_tool_calls(r#"finish(summary="0") as requested"#),
        None
    );
    assert_eq!(try_parse_tool_calls(r#"foo(bar="x")"#), None);
    assert_eq!(try_parse_tool_calls("evaluate(x=1)"), None);
    assert_eq!(
        try_parse_tool_calls(r#"batch_tools(calls=[{"tool": "read_file"}])"#),
        None
    );
}

#[test]
fn parse_func_expr_forms() {
    assert_eq!(
        try_parse_tool_calls(r#"finish(summary="0")"#),
        Some(vec![call("finish", json!({"summary": "0"}))])
    );
    assert_eq!(
        try_parse_tool_calls("finish(summary='0')"),
        Some(vec![call("finish", json!({"summary": "0"}))])
    );
    assert_eq!(
        try_parse_tool_calls(r#"create_file(path="answer.txt", content="1")"#),
        Some(vec![call(
            "create_file",
            json!({"path": "answer.txt", "content": "1"})
        )])
    );
    assert_eq!(
        try_parse_tool_calls("finish()"),
        Some(vec![call("finish", json!({}))])
    );
    assert_eq!(
        try_parse_tool_calls(r#"todo(action="update", index=1, done=true)"#),
        Some(vec![call(
            "todo",
            json!({"action": "update", "index": 1, "done": true})
        )])
    );
    assert_eq!(
        try_parse_tool_calls(r#"web_search(query="x", max_results=3)"#),
        Some(vec![call(
            "web_search",
            json!({"query": "x", "max_results": 3})
        )])
    );
    assert_eq!(
        try_parse_tool_calls(r#"memory_recall(query="x", limit=0.5)"#),
        Some(vec![call(
            "memory_recall",
            json!({"query": "x", "limit": 0.5})
        )])
    );
    assert_eq!(
        try_parse_tool_calls(r#"finish(summary="a, b and c")"#),
        Some(vec![call("finish", json!({"summary": "a, b and c"}))])
    );
    assert_eq!(
        try_parse_tool_calls("finish(summary=\"line1\nline2\")"),
        Some(vec![call("finish", json!({"summary": "line1\nline2"}))])
    );
    assert_eq!(
        try_parse_tool_calls(r#"tavily.search(query="x")"#),
        Some(vec![call("tavily.search", json!({"query": "x"}))])
    );
}

// ---- _task_original_text ----

#[test]
fn task_original_text_takes_last_real_user_message() {
    let msgs = vec![
        Message::system("s"),
        Message::user("Original task: output just the number."),
        Message::assistant(""),
        Message::user("⚠️ harness injection, should be skipped"),
        Message::user("⏱ Time budget: about 75% used, should be skipped too"),
        Message::user("[Earlier context summary] should be skipped too"),
        Message::user("Resume follow-up: this delivery answers this one"),
    ];
    assert_eq!(
        task_original_text(&msgs, 2000),
        "Resume follow-up: this delivery answers this one"
    );
    assert_eq!(
        task_original_text(&msgs[..2], 2000),
        "Original task: output just the number."
    );
    assert_eq!(task_original_text(&[Message::system("s")], 2000), "");
    assert_eq!(task_original_text(&[], 2000), "");
}

#[test]
fn task_original_text_truncates_long_task() {
    let long = format!("{}{}", "A".repeat(2000), "B".repeat(2000));
    let t = task_original_text(&[Message::user(long)], 2000);
    assert!(t.chars().count() <= 2100);
    assert!(t.contains("(omitted)") && t.starts_with('A') && t.ends_with('B'));
    assert!(!task_original_text(&[Message::user("short task")], 2000).contains("(omitted)"));
}

// ---- _has_task_progress / _is_qa (test_todo.py) ----

fn a_tool(name: &str) -> Message {
    Message::assistant_call(name, json!({}))
}

#[test]
fn has_task_progress_counts_only_mutating_or_mcp_tools() {
    assert!(has_task_progress(&[
        Message::user("have a look at this directory for me"),
        a_tool("list_dir"),
        a_tool("run_bash")
    ]));
    assert!(has_task_progress(&[
        Message::user("x"),
        a_tool("create_file")
    ]));
    assert!(!has_task_progress(&[
        Message::user("x"),
        a_tool("read_file"),
        a_tool("list_dir"),
        a_tool("web_search")
    ]));
    assert!(has_task_progress(&[
        Message::user("x"),
        a_tool("acme.render_docx")
    ]));
    assert!(!has_task_progress(&[Message::user("hello")]));
}

#[test]
fn trailing_question_mark_and_question_openers() {
    use boubou_core::guards::qa::ends_with_question;
    assert!(ends_with_question(
        "Are you capable of software development ?"
    ));
    assert!(ends_with_question("is that right?\""));
    assert!(ends_with_question("really?)"));
    assert!(!ends_with_question(
        "How many hours did it take? Output just the number."
    ));
    assert!(!ends_with_question("write a report"));
    // In the classifier: a trailing `?` is Q&A unless a task verb wins.
    assert!(is_qa(
        &[Message::user("Are you capable of software development ?")],
        false
    ));
    assert!(is_qa(
        &[Message::user(
            "Is there a config file somewhere in this repository?"
        )],
        false
    ));
    assert!(is_qa(
        &[Message::user("Do you know what day it is today")],
        false
    ));
    assert!(!is_qa(
        &[Message::user(
            "How many thousand hours did it take? Output just the number."
        )],
        false
    ));
    assert!(!is_qa(
        &[Message::user(
            "Can you write a script that parses the logs in this directory?"
        )],
        false
    ));
}

#[test]
fn is_qa_followup_fix_and_regressions() {
    let real_task = "This directory holds a batch of data files awaiting processing. Please check these files carefully for basic mistakes and verify the details one by one.";
    let followup = "there are other tools for these files";
    let msgs = vec![
        Message::user(real_task),
        a_tool("run_bash"),
        Message::user(followup),
    ];
    assert!(!is_qa(&msgs, has_task_progress(&msgs)));
    assert!(is_qa(&[Message::user(followup)], false));
    assert!(is_qa(
        &[
            Message::user(real_task),
            a_tool("run_bash"),
            Message::user("thanks")
        ],
        true
    ));
    assert!(!is_qa(
        &[Message::user("help me check the code for bugs")],
        false
    ));
    assert!(!is_qa(
        &[
            Message::user("please fix the failing tests in app.py"),
            a_tool("create_file"),
            Message::user("now run them")
        ],
        true
    ));
}

// ---- prefill budget of the todo schema (test_todo.py) ----

#[test]
fn todo_schema_stays_within_prefill_budget() {
    let todo = tools::core_tools()
        .into_iter()
        .find(|t| t["function"]["name"] == "todo")
        .expect("todo");
    let f = &todo["function"];
    assert!(estimate_tokens(f["description"].as_str().expect("desc")) <= 29);
    let props: std::collections::BTreeSet<&str> = f["parameters"]["properties"]
        .as_object()
        .expect("props")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        props,
        ["action", "items", "index", "done"].into_iter().collect()
    );
    assert_eq!(
        f["parameters"]["properties"]["index"],
        json!({"type": "number"})
    );
}
