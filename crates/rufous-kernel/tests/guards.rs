//! Port of `tests/test_guards.py` (pure-logic parts), `test_output_cap.py` (same) and the
//! todo-related heuristics from `test_todo.py`. Loop-wiring tests wait for the agent loop.

use std::path::Path;

use rufous_kernel::guards::artifacts::{claimed_missing_files, plan_named_missing};
use rufous_kernel::guards::edit::EditConvergenceGuard;
use rufous_kernel::guards::leak::{parse_finish_prose, try_parse_tool_calls};
use rufous_kernel::guards::qa::{
    has_task_progress, intent_heuristic, is_qa, task_original_text, Intent,
};
use rufous_kernel::guards::{estimate_tokens, format_bash_out, model_params_b, semantically_empty};
use rufous_kernel::ollama::Message;
use rufous_kernel::todo::{save_todo, TodoItem};
use rufous_kernel::tools;
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
    assert!(plan_named_missing(d.path(), d.path()).is_empty()); // no todo.json
    save_todo(
        d.path(),
        &[
            TodoItem::new("write report.md"),
            TodoItem::new("run the tests to verify"),
        ],
    )
    .expect("save");
    assert_eq!(plan_named_missing(d.path(), d.path()), vec!["report.md"]);
    touch(d.path(), "report.md");
    assert!(plan_named_missing(d.path(), d.path()).is_empty());

    let d2 = tempfile::tempdir().expect("tmp");
    save_todo(
        d2.path(),
        &[TodoItem {
            item: "generate change_log.md".into(),
            done: true,
        }],
    )
    .expect("save");
    assert_eq!(
        plan_named_missing(d2.path(), d2.path()),
        vec!["change_log.md"]
    );

    let d3 = tempfile::tempdir().expect("tmp");
    save_todo(
        d3.path(),
        &[TodoItem::new(
            "verify the 3.5b model output in v1.1.0 format",
        )],
    )
    .expect("save");
    assert!(plan_named_missing(d3.path(), d3.path()).is_empty());
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
    use rufous_kernel::guards::qa::ends_with_question;
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

#[test]
fn a_sentence_or_two_after_progress_is_a_follow_up() {
    // boubou-he2: 51 cue-free characters after real work steer that work; the same words
    // before any work are a task by default, and a cue still wins.
    let steer = "I think we already did that one. It can be closed.";
    let h = intent_heuristic(
        &[
            Message::user("Work through the task list"),
            a_tool("run_bash"),
            Message::user(steer),
        ],
        true,
    );
    assert_eq!(h.intent, Intent::FollowUp, "{h:?}");
    assert_eq!(h.confidence, 0.5);
    assert_eq!(
        intent_heuristic(&[Message::user(steer)], false).intent,
        Intent::Task
    );
    let long = "I think we already did that one, the mDNS discovery, and the UDX transport one as well, so both can be closed now.";
    assert_eq!(
        intent_heuristic(&[Message::user(long)], true).intent,
        Intent::Task,
        "past the follow-up length the long-message default stands"
    );
    assert_eq!(
        intent_heuristic(&[Message::user("now fix the import too")], true).intent,
        Intent::Task,
        "a task hint wins"
    );
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

// ---------------- profile-gated finish-prose rescue (gemma4) ----------------

fn rescued_summary(text: &str) -> Option<String> {
    let calls = parse_finish_prose(text)?;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "finish");
    Some(calls[0].1["summary"].as_str().unwrap_or("").to_string())
}

#[test]
fn finish_prose_rescue_reads_the_last_line_and_the_bare_brace_form() {
    // profiles/gemma4_e2b loop t05: reasoning, then the summary as prose on the last line.
    let t05 = "1. **Analyze the Request:** The user wants me to continue.\n\
               7. **Final Decision:** the data is unobtainable.\n\
               Finish: The task could not be completed because `dataset.csv` could not be downloaded.";
    assert_eq!(
        rescued_summary(t05).as_deref(),
        Some("The task could not be completed because `dataset.csv` could not be downloaded.")
    );
    // loop t03: "finish." runs straight into "Finish:" with no newline.
    let t03 = "I will proceed to write the required output and finish.Finish: The data download failed due to a connection error (HTTP 503).";
    assert_eq!(
        rescued_summary(t03).as_deref(),
        Some("The data download failed due to a connection error (HTTP 503).")
    );
    // probes_corrector2 t00: bare brace form without placeholder quotes.
    assert_eq!(
        rescued_summary("finish{summary: check.py printed FAIL; the correct sum is 1234.56}")
            .as_deref(),
        Some("check.py printed FAIL; the correct sum is 1234.56")
    );
    assert_eq!(
        rescued_summary(r#"finish(summary: "dataset.csv could not be produced (HTTP 503)")"#)
            .as_deref(),
        Some("dataset.csv could not be produced (HTTP 503)")
    );
    // The generic parser does not read any of these (that is why the rescue exists).
    assert_eq!(try_parse_tool_calls(t05), None);
    assert_eq!(try_parse_tool_calls(t03), None);
}

#[test]
fn finish_prose_rescue_leaves_ordinary_prose_alone() {
    assert_eq!(parse_finish_prose(""), None);
    assert_eq!(
        parse_finish_prose("Let me look at the directory structure first."),
        None
    );
    // "finish" as a verb, not a label.
    assert_eq!(
        parse_finish_prose("I will finish the report after the tests pass."),
        None
    );
    // The label on an earlier line is planning, not the exit.
    assert_eq!(
        parse_finish_prose("Finish: write the summary.\nFirst I need to read the data file."),
        None
    );
    // Too short to be a summary.
    assert_eq!(parse_finish_prose("Finish: done"), None);
    // Other tools in brace prose stay prose (the over-capture that sank the generic version).
    assert_eq!(
        parse_finish_prose(
            "I could use batch_tools{tools: [...]} to speed this up, as the docs say."
        ),
        None
    );
    assert_eq!(parse_finish_prose("Finish:"), None);
}

#[test]
fn a_verification_command_runs_a_check_rather_than_mentioning_one() {
    use rufous_kernel::guards::integrity::is_verification_command;
    for cmd in [
        "python check.py",
        "python3 -m pytest -q",
        "python3.12 -m unittest discover",
        "pytest tests/",
        "py.test",
        "python tests/run_all.py",
        "python verify_output.py",
        "node test/index.js",
        "bash ./run_tests.sh",
        "./check.sh",
        "./scripts/verify.sh --strict",
        "cargo test --lib",
        "cargo check",
        "cargo clippy --all-targets",
        "go test ./...",
        "npm test",
        "npm run lint",
        "yarn test:unit",
        "make check",
        "dart analyze lib",
        "flutter test",
        "ruff check .",
        "mypy src",
        "tsc --noEmit",
        "uv run pytest",
        "poetry run pytest -x",
        "npx jest",
        "timeout 60 python -m pytest",
        "FOO=1 python check.py",
        "cd tests && python -m pytest -q",
        "python -m pytest; echo done",
    ] {
        assert!(is_verification_command(cmd), "{cmd}");
    }
    // The old regex matched every one of these on the bare word.
    for cmd in [
        "cat test_parse.py",
        "ls tests/",
        "grep -n assert tests/x.py",
        "git checkout -b fix",
        "shasum -c checksums.txt",
        "sed -n 1,20p test.py",
        "echo \"tests pass\"",
        "pip install pytest",
        "touch test_x.py",
        "rm check.py",
        "cd tests && ls",
        "python fetch.py",
        "python -c \"print('check')\"",
        "python -m http.server",
        "cargo build --release",
        "npm run build",
        "npm install",
        "ruff format .",
        "tsc",
        "ls -la",
        "cat result.txt",
        "git status --short",
        "bd done 9h3",
    ] {
        assert!(!is_verification_command(cmd), "{cmd}");
    }
}

#[test]
fn the_follow_up_nudges_are_harness_notes_not_user_input() {
    use rufous_kernel::guards::qa::{
        is_harness_note, task_routing_text, FOLLOW_UP_DONE_NOTE, FOLLOW_UP_REPORT_NOTE,
    };
    for note in [FOLLOW_UP_DONE_NOTE, FOLLOW_UP_REPORT_NOTE] {
        assert!(is_harness_note(note), "{note}");
        let history = vec![
            Message::system("sys"),
            Message::user("Work through the task list."),
            Message::assistant("Task 1.1 is done."),
            Message::user("close that one please"),
            Message::assistant(""),
            Message::user(note),
        ];
        assert_eq!(task_routing_text(&history), "close that one please");
        let h = intent_heuristic(&history, true);
        assert_eq!(h.text, "close that one please", "{h:?}");
    }
}
