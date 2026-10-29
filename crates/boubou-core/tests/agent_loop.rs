//! Port of the `_scripted_loop` wiring tests from `tests/test_guards.py` and
//! `tests/test_output_cap.py`: the loop driven by a scripted backend and a scripted tool
//! runner, no network, no model.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use boubou_core::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use boubou_core::agent::{Agent, AgentEvent, RunParams, ToolRunner};
use boubou_core::guards::budget::{elapsed_meta_path, load_elapsed, save_elapsed};
use boubou_core::ollama::{Message, Role};
use serde_json::{json, Value};

type Call = (&'static str, Value, &'static str);

/// Replays `calls` as tool calls, then keeps answering with `finish`.
struct Scripted {
    calls: Vec<Call>,
    i: usize,
    finish_content: String,
    finish_summary: String,
}

impl ChatBackend for Scripted {
    fn chat(
        &mut self,
        _model: &str,
        _messages: &[Message],
        _tools: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        let k = self.i;
        self.i += 1;
        let mut r = if k < self.calls.len() {
            let (name, args, _) = &self.calls[k];
            ChatResponse::call("", name, args.clone())
        } else {
            ChatResponse::call(
                self.finish_content.clone(),
                "finish",
                json!({"summary": self.finish_summary}),
            )
        };
        r.prompt_eval_count = Some(500);
        Ok(r)
    }
}

/// Looks the (name, args) pair up in the script; `finish` always completes.
struct ScriptedTools {
    calls: Vec<Call>,
}

impl ToolRunner for ScriptedTools {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        for (n, a, r) in &self.calls {
            if *n == name && a == args {
                return r.to_string();
            }
        }
        if name == "finish" {
            return "[TASK_COMPLETE] all done".to_string();
        }
        "[tool error: not scripted]".to_string()
    }
}

const TASK: &str = "Fix the bug in m08.py and run the tests to verify.";
const EDIT_OK: &str = "[edited m08.py (backed up); .bak backup available]";

fn agent_for(workdir: &Path, calls: &[Call]) -> Agent<Scripted, ScriptedTools> {
    agent_with(workdir, calls, "done", "all done")
}

fn agent_with(
    workdir: &Path,
    calls: &[Call],
    finish_content: &str,
    finish_summary: &str,
) -> Agent<Scripted, ScriptedTools> {
    let backend = Scripted {
        calls: calls.to_vec(),
        i: 0,
        finish_content: finish_content.to_string(),
        finish_summary: finish_summary.to_string(),
    };
    let tools = ScriptedTools {
        calls: calls.to_vec(),
    };
    // TASK names m08.py as its input; the deliverable gate (integrity, gate 3b) would
    // otherwise ask for it once in every scripted run.
    let _ = std::fs::write(workdir.join("m08.py"), "def f():\n    return 1\n");
    let params = RunParams::plain("gemma4:12b", workdir);
    let mut a = Agent::new(params, backend, tools);
    a.sessions_dir = workdir.join("sessions");
    a
}

fn start(task: &str) -> Vec<Message> {
    vec![Message::system("sys"), Message::user(task)]
}

fn scripted(workdir: &Path, calls: &[Call]) -> Vec<Message> {
    agent_for(workdir, calls)
        .run(start(TASK), None, None)
        .expect("loop")
}

fn user_msgs<'a>(msgs: &'a [Message], needle: &str) -> Vec<&'a Message> {
    msgs.iter()
        .filter(|m| m.role == Role::User && m.content.contains(needle))
        .collect()
}

fn edit(i: usize, ok: bool) -> Call {
    let r = if ok {
        EDIT_OK
    } else {
        "[edit failed: old text not found. Preview of the file start: ...]"
    };
    (
        "edit_file",
        json!({"path": "m08.py", "old": i.to_string(), "new": "x"}),
        r,
    )
}

// ---- shotgun edit guard wiring (Task #72) ----

#[test]
fn shotgun_hint_injected_once_and_not_disabled() {
    let d = tempfile::tempdir().expect("tmp");
    let calls = vec![
        (
            "edit_file",
            json!({"path": "m08.py", "old": "a", "new": "b"}),
            EDIT_OK,
        ),
        (
            "run_bash",
            json!({"command": "python test_suite.py 1"}),
            "[exit 1]\nFAILED test_1",
        ),
        (
            "edit_file",
            json!({"path": "m08.py", "old": "c", "new": "d"}),
            EDIT_OK,
        ),
        ("read_file", json!({"path": "m08.py"}), "code"),
        (
            "edit_file",
            json!({"path": "m08.py", "old": "e", "new": "f"}),
            EDIT_OK,
        ),
        (
            "edit_file",
            json!({"path": "m08.py", "old": "g", "new": "h"}),
            EDIT_OK,
        ),
    ];
    let mut a = agent_for(d.path(), &calls);
    let msgs = a.run(start(TASK), None, None).expect("loop");
    let hints = user_msgs(&msgs, "edited m08.py successfully");
    assert_eq!(hints.len(), 1);
    assert!(hints[0].content.contains("m08.py"));
    assert!(!a.face.is_disabled("edit_file"));
}

#[test]
fn failed_edits_not_counted() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = (0..5).map(|i| edit(i, false)).collect();
    let msgs = scripted(d.path(), &calls);
    assert!(user_msgs(&msgs, "edited m08.py successfully").is_empty());
}

#[test]
fn disabled_edit_hallucination_not_counted() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = (0..5)
        .map(|i| {
            (
                "edit_file",
                json!({"path": "m08.py", "old": i.to_string(), "new": "x"}),
                "[tool edit_file is disabled (repeated-call protection).]",
            )
        })
        .collect();
    let mut a = agent_for(d.path(), &calls);
    a.face.disabled.insert("edit_file".into());
    let msgs = a.run(start(TASK), None, None).expect("loop");
    assert!(user_msgs(&msgs, "edited m08.py successfully").is_empty());
    // The refusal text is the loop's own (the scripted result is never consulted).
    assert!(msgs
        .iter()
        .any(|m| m.role == Role::Tool && m.content.contains("is disabled")));
}

// ---- time budget wiring ----

#[test]
fn budget_off_by_default() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = (0..6).map(|i| edit(i, true)).collect();
    let msgs = scripted(d.path(), &calls);
    assert!(msgs.iter().all(|m| !m.content.contains("⏱")));
    assert!(!elapsed_meta_path(d.path()).exists());
}

#[test]
fn budget_resume_clock_and_nudge_once() {
    let d = tempfile::tempdir().expect("tmp");
    save_elapsed(d.path(), 0.80 * 3600.0);
    let calls = vec![
        (
            "edit_file",
            json!({"path": "m08.py", "old": "a", "new": "b"}),
            EDIT_OK,
        ),
        (
            "run_bash",
            json!({"command": "python test_suite.py"}),
            "[exit 0]\n10 passed",
        ),
    ];
    let mut a = agent_for(d.path(), &calls);
    a.params.budget_env_sec = Some(3600.0);
    let msgs = a.run(start(TASK), None, None).expect("loop");
    let nudges: Vec<&Message> = msgs.iter().filter(|m| m.content.contains("⏱")).collect();
    assert!(!nudges.is_empty());
    assert!(nudges[0].content.contains("75%"));
    assert!(load_elapsed(d.path()) >= 0.80 * 3600.0);
    assert_eq!(
        nudges.iter().filter(|n| n.content.contains("75%")).count(),
        1
    );
}

#[test]
fn explicit_budget_sec_beats_env() {
    let d = tempfile::tempdir().expect("tmp");
    save_elapsed(d.path(), 0.80 * 3600.0);
    let calls = vec![edit(0, true), edit(1, true)];
    let mut a = agent_for(d.path(), &calls);
    a.params.budget_env_sec = Some(36_000.0); // env says 10h: 80% of 1h would be 8%
    let msgs = a.run(start(TASK), None, Some(3600.0)).expect("loop");
    let nudges: Vec<&Message> = msgs.iter().filter(|m| m.content.contains("⏱")).collect();
    assert!(nudges.iter().any(|n| n.content.contains("75%")));
}

// ---- finish gates ----

#[test]
fn finish_reread_injects_task_once_then_passes() {
    let d = tempfile::tempdir().expect("tmp");
    let calls = vec![(
        "create_file",
        json!({"path": "out.txt", "content": "17"}),
        "[created out.txt]",
    )];
    let task = "How many thousand hours did it take? Output just the number.";
    let msgs = agent_for(d.path(), &calls)
        .run(start(task), None, None)
        .expect("loop");
    let rereads = user_msgs(&msgs, "Pre-delivery check");
    assert_eq!(rereads.len(), 1);
    assert!(rereads[0].content.contains(task));
    assert!(rereads[0].content.contains("finish again"));
    assert!(rereads[0].content.starts_with("⚠️"));
    // bead 65d.14: the re-read carries the honest-failure clause gate 5 already has, so
    // "fix the deliverable" cannot be read as "obtain the missing input another way"
    // (profiles qwen3.5:9b loop t06, gemma4:12b loop t05).
    assert!(rereads[0]
        .content
        .contains("do not obtain or create that input another way"));
    assert_eq!(msgs.last().map(|m| &m.role), Some(&Role::Assistant));
}

#[test]
fn finish_reread_skipped_for_qa() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = agent_for(d.path(), &[])
        .run(start("hello"), None, None)
        .expect("loop");
    assert!(user_msgs(&msgs, "Pre-delivery check").is_empty());
}

/// Phase-driven backend: create real.txt, then a finish whose summary lies about report.md.
struct Liar {
    phase: usize,
}

impl ChatBackend for Liar {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        self.phase += 1;
        let mut r = match self.phase {
            1 => ChatResponse::call(
                "",
                "create_file",
                json!({"path": "real.txt", "content": "x"}),
            ),
            2 => ChatResponse::call(
                "",
                "finish",
                json!({"summary": "Done, see report.md and real.txt"}),
            ),
            _ => ChatResponse::call("done", "finish", json!({"summary": "all done"})),
        };
        r.prompt_eval_count = Some(500);
        Ok(r)
    }
}

struct CreateOnly;

impl ToolRunner for CreateOnly {
    fn run_tool(&mut self, name: &str, _args: &Value) -> String {
        match name {
            "create_file" => "[created real.txt]".to_string(),
            "finish" => "[TASK_COMPLETE] done".to_string(),
            _ => "[tool error: not scripted]".to_string(),
        }
    }
}

#[test]
fn finish_reread_only_after_existence_gates_pass() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("real.txt"), "x").expect("write");
    let task = "Write a report to report.md with a purely numeric conclusion.";
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Liar { phase: 0 },
        CreateOnly,
    );
    let msgs = a.run(start(task), None, None).expect("loop");
    assert!(!user_msgs(&msgs, "named in your summary do not exist").is_empty());
    assert!(user_msgs(&msgs, "Pre-delivery check").len() <= 1);
}

#[test]
fn fake_finish_rejection_carries_next_action() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = agent_with(d.path(), &[], "", "")
        .run(start(TASK), None, None)
        .expect("loop");
    let rejects = user_msgs(&msgs, "finish was rejected");
    assert!(!rejects.is_empty());
    let c = &rejects[0].content;
    assert!(c.contains("Pick one of three"));
    assert!(c.contains("create_file") && c.contains("answer.txt"));
    assert!(c.contains("keep going with tools"));
    assert!(c.contains("analysis or explanation task"));
    let abs = std::path::absolute(d.path()).expect("abs");
    assert!(c.contains(&abs.to_string_lossy().into_owned()));
    // The refused call never gets a success receipt.
    assert!(!msgs
        .iter()
        .take(msgs.len() - 1)
        .any(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
}

#[test]
fn fake_finish_allows_text_delivery() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = agent_with(
        d.path(),
        &[],
        "The variables, explained one by one: …",
        "all done",
    )
    .run(start(TASK), None, None)
    .expect("loop");
    assert!(user_msgs(&msgs, "finish was rejected").is_empty());
}

#[test]
fn fake_finish_allows_substantive_summary() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = agent_with(d.path(), &[], "", "Conclusion: 17 thousand hours in total.")
        .run(start(TASK), None, None)
        .expect("loop");
    assert!(user_msgs(&msgs, "finish was rejected").is_empty());
}

#[test]
fn todo_sync_gate_blocks_twice_then_passes() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(
        d.path().join("todo.json"),
        r#"[{"item":"a","done":true},{"item":"b","done":false}]"#,
    )
    .expect("write");
    let calls = vec![(
        "create_file",
        json!({"path": "out.txt", "content": "17"}),
        "[created out.txt]",
    )];
    let msgs = scripted(d.path(), &calls);
    let gates = user_msgs(&msgs, "not marked done");
    assert_eq!(gates.len(), 2);
    assert!(gates[0].content.contains("1/2"));
    assert_eq!(msgs.last().map(|m| &m.role), Some(&Role::Assistant));
}

// ---- integrity gate (Boubou, 2026-10-28; bench/probes.py findings) ----

const CHECK_TASK: &str = "Compute the paid total from data.csv, write it to result.txt, then run `python check.py`; the task is complete only when it prints OK.";

/// A scripted `python check.py` whose result head carries the exit code.
fn check_run(out: &'static str) -> Call {
    ("run_bash", json!({"command": "python check.py"}), out)
}

#[test]
fn claimed_verification_without_a_passing_check_is_rejected_once() {
    // probes_gatefix t02: "check.py printed OK" with zero check.py runs in the turn.
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "80.00").expect("w");
    let calls = vec![(
        "create_file",
        json!({"path": "result.txt", "content": "80.00"}),
        "[created result.txt]",
    )];
    let msgs = agent_with(d.path(), &calls, "", "check.py printed OK")
        .run(start(CHECK_TASK), None, None)
        .expect("loop");
    let rejects = user_msgs(&msgs, "claims a verified result");
    assert_eq!(rejects.len(), 2, "two refusals, then let through");
    assert!(rejects[0].content.contains("no verification command"));
    assert!(rejects[0]
        .content
        .contains("Never report a result you did not observe"));
    assert_eq!(msgs.last().map(|m| &m.role), Some(&Role::Assistant));
}

#[test]
fn claimed_verification_backed_by_a_failed_check_is_rejected_with_the_exit_code() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "80.00").expect("w");
    let calls = vec![check_run(
        "[exit 1]\nSTDOUT:\nFAIL: result.txt says '80.00'",
    )];
    let msgs = agent_with(d.path(), &calls, "", "check.py printed OK")
        .run(start(CHECK_TASK), None, None)
        .expect("loop");
    let rejects = user_msgs(&msgs, "claims a verified result");
    assert_eq!(rejects.len(), 2);
    assert!(rejects[0].content.contains("exited 1"));
}

#[test]
fn honest_failure_report_passes_the_claim_gate() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "80.00").expect("w");
    let calls = vec![check_run("[exit 1]\nSTDOUT:\nFAIL")];
    let msgs = agent_with(
        d.path(),
        &calls,
        "",
        "check.py printed FAIL: the paid total is still wrong.",
    )
    .run(start(CHECK_TASK), None, None)
    .expect("loop");
    assert!(user_msgs(&msgs, "claims a verified result").is_empty());
    assert!(user_msgs(&msgs, "yet the summary reports success").is_empty());
}

#[test]
fn success_summary_over_a_failed_check_is_rejected() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "80.00").expect("w");
    let calls = vec![check_run("[exit 1]\nSTDOUT:\nFAIL")];
    let msgs = agent_with(
        d.path(),
        &calls,
        "",
        "Computed the paid total and wrote result.txt.",
    )
    .run(start(CHECK_TASK), None, None)
    .expect("loop");
    let rejects = user_msgs(&msgs, "yet the summary reports success");
    assert_eq!(rejects.len(), 2);
    assert!(rejects[0].content.contains("python check.py"));
}

#[test]
fn passing_check_backs_the_claim_and_overrides_plan_sync() {
    // probes t09: correct result, check.py OK, finish refused twice for an unticked todo.
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "1234.56").expect("w");
    std::fs::write(
        d.path().join("todo.json"),
        r#"[{"item":"compute","done":true},{"item":"run check.py","done":false}]"#,
    )
    .expect("w");
    let calls = vec![check_run("[exit 0]\nSTDOUT:\nOK")];
    let msgs = agent_with(d.path(), &calls, "", "check.py printed OK")
        .run(start(CHECK_TASK), None, None)
        .expect("loop");
    assert!(user_msgs(&msgs, "claims a verified result").is_empty());
    assert!(
        user_msgs(&msgs, "not marked done").is_empty(),
        "verified deliverable outranks bookkeeping"
    );
    assert!(
        user_msgs(&msgs, "Pre-delivery check").is_empty(),
        "a passing check is the self-check; no re-injection (probes_integrity2 t00/t04)"
    );
    let todo = std::fs::read_to_string(d.path().join("todo.json")).expect("todo");
    assert!(!todo.contains("false"), "todo synced for the model: {todo}");
}

#[test]
fn a_check_that_passed_before_later_file_changes_is_stale_and_rejected() {
    // probes_integrity2 t05: check.py OK, then a failing `awk … > result.txt` emptied the
    // file, then "check.py printed OK" was accepted on the stale pass.
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "").expect("w");
    let calls = vec![
        check_run("[exit 0]\nSTDOUT:\nOK"),
        (
            "run_bash",
            json!({"command": "awk -F, '$2==\"paid\"{s+=$3} END{print s}' data.csv > result.txt"}),
            "[exit 2]\nSTDERR:\nawk: syntax error",
        ),
    ];
    let msgs = agent_with(d.path(), &calls, "", "check.py printed OK")
        .run(start(CHECK_TASK), None, None)
        .expect("loop");
    let rejects = user_msgs(&msgs, "so the verified state may be gone");
    assert_eq!(rejects.len(), 2, "stale evidence is refused, twice at most");
    assert!(rejects[0].content.contains("python check.py"));
    assert!(rejects[0].content.contains("awk"));
    // and the pre-delivery check is NOT skipped for a stale pass
    assert_eq!(user_msgs(&msgs, "Pre-delivery check").len(), 1);
}

#[test]
fn reading_files_after_a_passing_check_keeps_it_current() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "1234.56").expect("w");
    let calls = vec![
        check_run("[exit 0]\nSTDOUT:\nOK"),
        (
            "run_bash",
            json!({"command": "cat result.txt"}),
            "[exit 0]\nSTDOUT:\n1234.56",
        ),
    ];
    let msgs = agent_with(d.path(), &calls, "", "check.py printed OK")
        .run(start(CHECK_TASK), None, None)
        .expect("loop");
    assert!(user_msgs(&msgs, "verified state may be gone").is_empty());
    assert!(user_msgs(&msgs, "Pre-delivery check").is_empty());
}

#[test]
fn task_named_deliverable_missing_is_rejected_once_unless_reported_failed() {
    // LRAB WF-04: "Task complete" with fix_summary.md never written and never mentioned.
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("bug_report.md"), "# bugs").expect("w");
    let task = "Read inventory.py, write bug_report.md, fix the bugs, then write fix_summary.md.";
    std::fs::write(d.path().join("inventory.py"), "x = 1").expect("w");
    let calls = vec![(
        "create_file",
        json!({"path": "bug_report.md", "content": "# bugs"}),
        "[created bug_report.md]",
    )];
    let msgs = agent_with(
        d.path(),
        &calls,
        "",
        "Fixed the bugs and wrote the bug report. Task complete.",
    )
    .run(start(task), None, None)
    .expect("loop");
    let rejects = user_msgs(&msgs, "the task names these files");
    assert_eq!(rejects.len(), 1);
    assert!(rejects[0].content.contains("fix_summary.md"));
    assert!(
        !rejects[0].content.contains("inventory.py"),
        "existing input is not missing"
    );
    assert!(rejects[0].content.contains("say so in the summary"));
    assert_eq!(msgs.last().map(|m| &m.role), Some(&Role::Assistant));

    // The honest path: naming the file as not produced is accepted first time.
    let d2 = tempfile::tempdir().expect("tmp");
    std::fs::write(d2.path().join("bug_report.md"), "# bugs").expect("w");
    std::fs::write(d2.path().join("inventory.py"), "x = 1").expect("w");
    let msgs = agent_with(
        d2.path(),
        &calls,
        "",
        "Wrote bug_report.md. fix_summary.md could not be written: the fix is incomplete.",
    )
    .run(start(task), None, None)
    .expect("loop");
    assert!(user_msgs(&msgs, "the task names these files").is_empty());
}

#[test]
fn honest_failure_about_a_claimed_file_passes_the_artefact_and_plan_gates() {
    // loop probe: "Download of dataset.csv failed" was refused six times for naming the file.
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("summary.md"), "download failed").expect("w");
    std::fs::write(d.path().join("fetch.py"), "raise SystemExit(1)").expect("w");
    std::fs::write(
        d.path().join("todo.json"),
        r#"[{"item":"run fetch.py to download dataset.csv","done":true},{"item":"write summary.md","done":true}]"#,
    )
    .expect("w");
    let task =
        "Run `python fetch.py` to download dataset.csv, then write summary.md with the row count.";
    let calls = vec![
        (
            "run_bash",
            json!({"command": "python fetch.py"}),
            "[exit 1]\nSTDERR:\nConnectionError 503",
        ),
        (
            "create_file",
            json!({"path": "summary.md", "content": "download failed"}),
            "[created summary.md]",
        ),
    ];
    let msgs = agent_with(
        d.path(),
        &calls,
        "",
        "Download of dataset.csv failed (HTTP 503), so the row count could not be determined. Wrote summary.md saying so.",
    )
    .run(start(task), None, None)
    .expect("loop");
    assert!(user_msgs(&msgs, "named in your summary do not exist").is_empty());
    assert!(user_msgs(&msgs, "names these output files").is_empty());
    assert!(user_msgs(&msgs, "the task names these files").is_empty());
    assert!(user_msgs(&msgs, "finish was rejected").is_empty());
}

// ---- research streak, fingerprint level ----

#[test]
fn research_streak_ignores_new_queries() {
    let d = tempfile::tempdir().expect("tmp");
    let mut calls: Vec<Call> = vec![(
        "enable_tools",
        json!({"tools": ["web_search"]}),
        "[enabled]",
    )];
    let queries: Vec<String> = (0..8).map(|i| format!("different query {i}")).collect();
    for q in &queries {
        calls.push(("web_search", json!({"query": q}), "• r"));
    }
    let msgs = scripted(d.path(), &calls);
    assert!(user_msgs(&msgs, "disabled").is_empty());
    // enable_tools really put web_search on the face: no "is not available" refusals.
    assert!(!msgs.iter().any(|m| m.content.contains("is not available")));
}

#[test]
fn research_streak_bans_repeated_fingerprints() {
    let d = tempfile::tempdir().expect("tmp");
    let mut calls: Vec<Call> = vec![(
        "enable_tools",
        json!({"tools": ["web_search"]}),
        "[enabled]",
    )];
    for i in 0..12 {
        let q = if i % 2 == 0 { "A" } else { "B" };
        calls.push(("web_search", json!({"query": q}), "• r"));
    }
    let mut a = agent_for(d.path(), &calls);
    let msgs = a.run(start(TASK), None, None).expect("loop");
    assert!(!user_msgs(&msgs, "repeated the exact same web query").is_empty());
    assert!(a.face.is_disabled("web_search"));
}

#[test]
fn research_streak_covers_multi() {
    let d = tempfile::tempdir().expect("tmp");
    let mut calls: Vec<Call> = vec![(
        "enable_tools",
        json!({"tools": ["web_search_multi"]}),
        "[enabled]",
    )];
    for i in 0..12 {
        let q = if i % 2 == 0 { "A" } else { "B" };
        calls.push(("web_search_multi", json!({"queries": [q]}), "• r"));
    }
    let msgs = scripted(d.path(), &calls);
    assert!(!user_msgs(&msgs, "repeated the exact same web query").is_empty());
}

#[test]
fn enable_tools_unlocks_a_disabled_tool() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = vec![
        (
            "enable_tools",
            json!({"tools": ["web_search"]}),
            "[enabled]",
        ),
        ("web_search", json!({"query": "x"}), "• r"),
    ];
    let mut a = agent_for(d.path(), &calls);
    a.face.disable("web_search");
    let msgs = a.run(start(TASK), None, None).expect("loop");
    assert!(!a.face.is_disabled("web_search"));
    assert!(!msgs.iter().any(|m| m.content.contains("is not available")));
}

// ---- duplicate-call interception ----

#[test]
fn identical_consecutive_call_is_intercepted() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = vec![
        ("read_file", json!({"path": "m08.py"}), "code"),
        ("read_file", json!({"path": "m08.py"}), "code"),
    ];
    let msgs = scripted(d.path(), &calls);
    let blocked: Vec<&Message> = msgs
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.starts_with("[blocked:"))
        .collect();
    assert_eq!(blocked.len(), 1);
    assert!(!user_msgs(&msgs, "Do not retry it unchanged").is_empty());
}

// ---- output cap: truncation feedback and the empty-turn ladder ----

fn debris() -> ChatResponse {
    let mut r = ChatResponse::text("<think>\nI need to write the whole app.py now");
    r.done_reason = Some("length".into());
    r.eval_count = Some(8192);
    r
}

struct Responder<F: FnMut(usize) -> ChatResponse> {
    n: usize,
    f: F,
}

impl<F: FnMut(usize) -> ChatResponse> ChatBackend for Responder<F> {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        self.n += 1;
        Ok((self.f)(self.n))
    }
}

struct OkTools;

impl ToolRunner for OkTools {
    fn run_tool(&mut self, name: &str, _args: &Value) -> String {
        if name == "finish" {
            "[TASK_COMPLETE] ok".to_string()
        } else {
            "[ok]".to_string()
        }
    }
}

fn run_responder<F: FnMut(usize) -> ChatResponse>(workdir: &Path, f: F) -> Vec<Message> {
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", workdir),
        Responder { n: 0, f },
        OkTools,
    );
    a.run(start("Write a large file and verify it."), None, None)
        .expect("loop")
}

#[test]
fn truncation_feedback_injected_once_with_limit() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_responder(d.path(), |n| match n {
        1 => debris(),
        2 => ChatResponse::call(
            "",
            "create_file",
            json!({"path": "app.py", "content": "print(1)"}),
        ),
        _ => ChatResponse::call("done", "finish", json!({"summary": "all done"})),
    });
    let advice = user_msgs(&out, "append_file in pieces");
    assert_eq!(advice.len(), 1);
    assert!(advice[0].content.contains("8192"));
}

#[test]
fn hard_reset_reachable_after_six_empties() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_responder(d.path(), |n| {
        if n <= 6 {
            debris()
        } else {
            ChatResponse::call("done", "finish", json!({"summary": "all done"}))
        }
    });
    let joined: String = out
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(joined.contains("the context has been reset"));
    assert!(out
        .iter()
        .any(|m| m.content.contains("Write a large file and verify")));
}

#[test]
fn empty_ladder_exits_after_three_resets() {
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |_| debris(),
        },
        OkTools,
    );
    let err = a
        .run(start("Write a large file and verify it."), None, None)
        .err();
    assert!(matches!(err, Some(boubou_core::Error::Aborted(_))));
}

// ---- repeat-output detector / Q&A wrap-up ----

#[test]
fn qa_text_answer_is_force_closed() {
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |_| ChatResponse::text("Hello! I'm Boubou."),
        },
        OkTools,
    );
    let out = a.run(start("hello"), None, None).expect("loop");
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
    assert!(!user_msgs(&out, "Your answer is already sufficient").is_empty());
    assert!(out.len() < 8);
}

#[test]
fn repeated_text_is_a_dead_loop() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_responder(d.path(), |n| {
        if n <= 3 {
            ChatResponse::text("I will start implementing this feature by creating the file.")
        } else {
            ChatResponse::call("done", "finish", json!({"summary": "all done"}))
        }
    });
    assert!(!user_msgs(&out, "counts as a dead loop").is_empty());
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
}

#[test]
fn cycling_text_outputs_are_a_dead_loop() {
    // gemma4 alternated three different sentences forever: a window of recent texts catches
    // the cycle where comparing only with the previous turn did not.
    let d = tempfile::tempdir().expect("tmp");
    const CYCLE: [&str; 3] = [
        "What would you like to do next?",
        "I can help you with software development tasks. What are you working on?",
        "I can help you with writing code, explaining concepts, debugging, and generating code snippets.",
    ];
    let out = run_responder(d.path(), |n| ChatResponse::text(CYCLE[(n - 1) % 3]));
    assert!(!user_msgs(&out, "counts as a dead loop").is_empty());
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
    let texts = out
        .iter()
        .filter(|m| m.role == Role::Assistant && m.tool_calls.is_empty())
        .count();
    assert!(texts <= 5, "{texts} text turns before the loop was closed");
}

#[test]
fn text_only_task_turns_are_capped() {
    // Distinct text every turn, never a tool call: the sharper note at the second turn, the
    // turn ended at the fourth instead of nagging until max_iterations.
    let d = tempfile::tempdir().expect("tmp");
    const PROSE: [&str; 5] = [
        "First I would sketch the overall structure of the file.",
        "The data model needs a header section and several numbered entries.",
        "Validation could run as a separate pass once everything is written.",
        "Let me think about how large the output is going to be.",
        "There are a few ways to approach the verification step.",
    ];
    let out = run_responder(d.path(), |n| ChatResponse::text(PROSE[(n - 1) % 5]));
    assert_eq!(user_msgs(&out, "Do not reply with more text").len(), 1);
    assert_eq!(user_msgs(&out, "The turn ends here").len(), 1);
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
    let texts = out
        .iter()
        .filter(|m| m.role == Role::Assistant && m.tool_calls.is_empty())
        .count();
    assert_eq!(texts, 4);
    // The generic continue note fired for the first and third text turns.
    assert_eq!(user_msgs(&out, "Continue: keep making").len(), 2);
}

#[test]
fn chat_mode_text_reply_ends_the_turn() {
    // `boubou chat`: a plain text reply is the answer even when the message reads like a task.
    let d = tempfile::tempdir().expect("tmp");
    let mut params = RunParams::plain("gemma4:12b", d.path());
    params.chat_mode = true;
    let mut a = Agent::new(
        params,
        Responder {
            n: 0,
            f: |_| {
                ChatResponse::text(
                    "Rust in the morning / borrow checker hums along / fearless, we compile",
                )
            },
        },
        OkTools,
    );
    let out = a
        .run(start("Write me a haiku about Rust"), None, None)
        .expect("loop");
    assert!(user_msgs(&out, "Continue: keep making").is_empty());
    assert!(!user_msgs(&out, "Your answer is already sufficient").is_empty());
    assert_eq!(
        out.iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_empty())
            .count(),
        1
    );
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
}

#[test]
fn trailing_question_mark_is_qa_in_the_loop() {
    // The transcript's opening line: 41 characters, no question word, a space before the `?`.
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |_| {
                ChatResponse::text(
                    "I can help with writing code, debugging and explaining concepts.",
                )
            },
        },
        OkTools,
    );
    let out = a
        .run(
            start("Are you capable of software development ?"),
            None,
            None,
        )
        .expect("loop");
    assert!(user_msgs(&out, "Continue: keep making").is_empty());
    assert!(!user_msgs(&out, "Your answer is already sufficient").is_empty());
    assert!(out.len() < 8);
}

#[test]
fn qa_turn_can_run_the_web_tools_it_is_offered() {
    // Web UI session of 2026-10-28: a question turn listed web_search/web_fetch in the tool
    // definitions, the model called them, and the gate answered "not available" because the
    // Q&A tool set was never activated. The model then told the user it cannot browse.
    struct ToolsSeen<F: FnMut(usize) -> ChatResponse> {
        n: usize,
        f: F,
        offered: Arc<Mutex<Vec<Vec<String>>>>,
    }
    impl<F: FnMut(usize) -> ChatResponse> ChatBackend for ToolsSeen<F> {
        fn chat(
            &mut self,
            _m: &str,
            _msgs: &[Message],
            t: &[Value],
            _on: &mut dyn FnMut(StreamPiece<'_>),
        ) -> Result<ChatResponse, ChatError> {
            self.n += 1;
            let names = t
                .iter()
                .filter_map(|d| d["function"]["name"].as_str().map(String::from))
                .collect();
            self.offered.lock().expect("lock").push(names);
            Ok((self.f)(self.n))
        }
    }
    let d = tempfile::tempdir().expect("tmp");
    let offered = Arc::new(Mutex::new(Vec::new()));
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        ToolsSeen {
            n: 0,
            offered: offered.clone(),
            f: |n| match n {
                1 => ChatResponse::call(
                    "",
                    "web_search",
                    json!({"query": "Flint model Recursive AI"}),
                ),
                2 => ChatResponse::call("", "web_fetch", json!({"url": "https://recursive.com"})),
                _ => ChatResponse::text("Flint is fast because it samples in parallel."),
            },
        },
        OkTools,
    );
    let out = a
        .run(
            start("Can you explain how the Flint model from Recursive AI achieves its speed?"),
            None,
            None,
        )
        .expect("loop");
    // It was a Q&A turn (chat prompt) and web_search was in the definitions sent.
    assert!(out[0].role == Role::System && out[0].content.contains("chatting with the user"));
    let first = offered.lock().expect("lock")[0].clone();
    assert!(first.iter().any(|n| n == "web_search"), "{first:?}");
    // Both calls ran (OkTools answers "[ok]"); nothing was refused as unavailable.
    let tool_msgs: Vec<&Message> = out.iter().filter(|m| m.role == Role::Tool).collect();
    assert!(
        tool_msgs
            .iter()
            .all(|m| !m.content.contains("is not available")),
        "{tool_msgs:?}"
    );
    assert_eq!(tool_msgs.iter().filter(|m| m.content == "[ok]").count(), 2);
    assert!(a.face.is_active("web_search") && a.face.is_active("web_fetch"));
}

// ---- every injected user message starts with a harness prefix ----

#[test]
fn injected_user_messages_keep_the_harness_prefix_invariant() {
    let d = tempfile::tempdir().expect("tmp");
    let mut calls: Vec<Call> = vec![(
        "enable_tools",
        json!({"tools": ["web_search"]}),
        "[enabled]",
    )];
    for i in 0..12 {
        let q = if i % 2 == 0 { "A" } else { "B" };
        calls.push(("web_search", json!({"query": q}), "• r"));
    }
    for i in 0..5 {
        calls.push(edit(i, false));
    }
    calls.push(("read_file", json!({"path": "m08.py"}), "code"));
    calls.push(("read_file", json!({"path": "m08.py"}), "code"));
    let msgs = agent_with(d.path(), &calls, "", "")
        .run(start(TASK), None, None)
        .expect("loop");
    let offenders: Vec<&str> = msgs
        .iter()
        .skip(2) // system + the task itself
        .filter(|m| m.role == Role::User)
        .map(|m| m.content.as_str())
        .filter(|c| !boubou_core::guards::qa::is_harness_note(c))
        .collect();
    assert!(offenders.is_empty(), "{offenders:?}");
}

// ---- checkpoint and events ----

#[test]
fn checkpoint_relocates_out_of_the_workdir_with_its_elapsed_sidecar() {
    // boubou-ll7: an embedding whose workdir is the user's project (ACP) keeps the live
    // state beside the session files; nothing of the harness lands in the workdir.
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path().join("project");
    std::fs::create_dir_all(&wd).expect("mkdir");
    let sessions = d.path().join("home").join("sessions"); // not created: the loop must
    let state = boubou_core::agent::session::state_path(&sessions, "acp_1029_094732");
    let meta = boubou_core::guards::budget::elapsed_meta_for(&state);
    assert_eq!(
        meta.file_name().and_then(|n| n.to_str()),
        Some("acp_1029_094732.state.meta.json")
    );
    assert_eq!(
        boubou_core::guards::budget::elapsed_meta_for(
            &elapsed_meta_path(&wd).with_file_name(".agent_state.json")
        ),
        elapsed_meta_path(&wd),
        "the default checkpoint keeps its Mingbird sidecar name"
    );
    let calls = vec![
        (
            "edit_file",
            json!({"path": "m08.py", "old": "a", "new": "b"}),
            EDIT_OK,
        ),
        (
            "run_bash",
            json!({"command": "python test_suite.py"}),
            "[exit 0]\n10 passed",
        ),
    ];
    // A resumed clock is read from the sidecar beside the relocated checkpoint.
    boubou_core::guards::budget::save_elapsed_at(&meta, 100.0);
    let mut a = agent_for(&wd, &calls);
    a.checkpoint = Some(state.clone());
    a.params.budget_env_sec = Some(3600.0);
    let msgs = a.run(start(TASK), None, None).expect("loop");
    assert!(state.is_file(), "missing {}", state.display());
    let saved: Vec<Message> =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("read")).expect("json");
    assert!(saved.len() >= 3 && saved.len() <= msgs.len());
    assert!(meta.is_file(), "elapsed sidecar beside the checkpoint");
    assert!(boubou_core::guards::budget::load_elapsed_at(&meta) >= 100.0);
    let left: Vec<String> = std::fs::read_dir(&wd)
        .expect("ls")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".agent_state"))
        .collect();
    assert!(left.is_empty(), "harness files in the workdir: {left:?}");
}

#[test]
fn checkpoint_written_and_events_emitted() {
    let d = tempfile::tempdir().expect("tmp");
    let calls = vec![(
        "create_file",
        json!({"path": "out.txt", "content": "17"}),
        "[created out.txt]",
    )];
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let sink = Arc::clone(&events);
    let msgs = agent_for(d.path(), &calls)
        .with_sink(move |e| sink.lock().expect("lock").push(e))
        .run(start(TASK), None, None)
        .expect("loop");
    let ckpt = d.path().join(".agent_state.json");
    assert!(ckpt.exists());
    let saved: Vec<Message> =
        serde_json::from_str(&std::fs::read_to_string(ckpt).expect("read")).expect("json");
    assert!(saved.len() <= msgs.len() && saved.len() >= 3);
    let ev = events.lock().expect("lock");
    assert!(ev
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolResult { name, .. } if name == "create_file")));
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::Finished { .. })));
    assert!(ev
        .iter()
        .any(|e| matches!(e, AgentEvent::ContextUsage { used: 500, .. })));
}

#[test]
fn session_files_are_written_in_mingbird_shapes() {
    let d = tempfile::tempdir().expect("tmp");
    let calls = vec![(
        "create_file",
        json!({"path": "out.txt", "content": "17"}),
        "[created out.txt]",
    )];
    let mut a = agent_for(d.path(), &calls);
    let msgs = a.run(start(TASK), Some("s1"), None).expect("loop");
    let sd = d.path().join("sessions");
    let ctx: Vec<Message> =
        serde_json::from_str(&std::fs::read_to_string(sd.join("s1.json")).expect("read"))
            .expect("json");
    assert_eq!(ctx, msgs);
    let tl = std::fs::read_to_string(sd.join("s1.transcript.jsonl")).expect("read");
    assert_eq!(tl.lines().count(), msgs.len());
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(sd.join("s1.meta.json")).expect("read"))
            .expect("json");
    assert_eq!(meta["status"], "done");
    assert_eq!(meta["msgs"], msgs.len());
    assert_eq!(meta["task"], TASK);
    assert!(meta["workdir"].as_str().is_some_and(|w| !w.is_empty()));
}

#[test]
fn stop_flag_ends_the_run_before_the_next_turn_and_saves() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let d = tempfile::tempdir().expect("tmp");
    let calls = vec![edit(0, true)];
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let flag = Arc::new(AtomicBool::new(true)); // already raised: the loop must not take a turn
    let mut agent = agent_for(d.path(), &calls)
        .with_sink(move |e| sink.lock().expect("lock").push(e))
        .with_stop(flag.clone());
    assert!(agent.stop_flag().load(Ordering::SeqCst));
    let out = agent
        .run(start(TASK), Some("s1"), None)
        .expect("stop is a clean exit");
    assert_eq!(out.len(), 2, "no assistant turn was taken");
    let ev = events.lock().expect("lock");
    assert!(ev
        .iter()
        .any(|e| matches!(e, AgentEvent::Log(l) if l.contains("[stopped]"))));
    assert!(!ev.iter().any(|e| matches!(e, AgentEvent::ToolCall(_))));
    assert!(!ev.iter().any(|e| matches!(e, AgentEvent::MaxIterations)));
    assert!(
        d.path().join("sessions").join("s1.json").exists(),
        "session saved on stop"
    );
}

// ---- missing-argument ladder and truncated tool calls (LRAB WF-08 post-mortem) ----

const MISSING_PATH: &str = "[tool error: create_file is missing argument 'path'. Required arguments: [\"path\"]. You passed: [\"content\"]. Correct example: create_file(path=\"app.py\", content=\"...file content...\") Add the missing argument and retry with the same content (change nothing else).]";

fn pathless_calls(msgs: &[Message]) -> Vec<String> {
    msgs.iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| m.tool_calls.iter())
        .filter(|tc| {
            tc.function.name == "create_file" && tc.function.arguments.get("path").is_none()
        })
        .map(|tc| {
            tc.function.arguments["content"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

#[test]
fn repeated_missing_path_prunes_history_and_prescribes_chunked_shape() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = vec![
        ("create_file", json!({"content": "A"}), MISSING_PATH),
        ("create_file", json!({"content": "B"}), MISSING_PATH),
        ("create_file", json!({"content": "C"}), MISSING_PATH),
        (
            "create_file",
            json!({"path": "design_notes.md", "content": "D"}),
            "[created design_notes.md (1 bytes)]",
        ),
    ];
    let msgs = scripted(d.path(), &calls);
    // Only the most recent failed call survives; A and B (and their errors) were pruned.
    assert_eq!(pathless_calls(&msgs), vec!["C".to_string()]);
    let errs = msgs
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.contains("is missing argument 'path'"))
        .count();
    assert_eq!(errs, 1, "one missing-arg error left as the single example");
    // One note (the earlier one went with its pair), prescribing a different call shape.
    let notes = user_msgs(&msgs, "[missing argument]");
    assert_eq!(notes.len(), 1);
    assert!(notes[0].content.contains("append_file"));
    assert!(notes[0].content.contains("3 times in a row"));
    assert!(
        !notes[0]
            .content
            .contains("Files in the plan not yet produced"),
        "no plan → no plan hint"
    );
    // The generic failure ladder did not fire for this class of error.
    assert!(user_msgs(
        &msgs,
        "Check the real state with list_dir / read_file first"
    )
    .is_empty());
    // Order: kept failed call → its error → the note; system + task untouched.
    assert_eq!(msgs[0].role, Role::System);
    assert_eq!(msgs[1].content, TASK);
    let c_idx = msgs
        .iter()
        .position(|m| {
            m.role == Role::Assistant
                && !m.tool_calls.is_empty()
                && m.tool_calls[0].function.arguments.get("content") == Some(&json!("C"))
        })
        .expect("call C");
    assert!(msgs[c_idx + 1].content.contains("missing argument"));
    assert!(msgs[c_idx + 2].content.starts_with("⚠️ [missing argument]"));
    // The successful write landed afterwards.
    assert!(msgs
        .iter()
        .any(|m| m.content.starts_with("[created design_notes.md")));
}

#[test]
fn complete_call_resets_missing_arg_ladder() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = vec![
        ("create_file", json!({"content": "A"}), MISSING_PATH),
        (
            "create_file",
            json!({"path": "a.md", "content": "ok"}),
            "[created a.md (2 bytes)]",
        ),
        ("create_file", json!({"content": "B"}), MISSING_PATH),
    ];
    let msgs = scripted(d.path(), &calls);
    // Two isolated misses are not a streak: nothing pruned, no note.
    assert_eq!(
        pathless_calls(&msgs),
        vec!["A".to_string(), "B".to_string()]
    );
    assert!(user_msgs(&msgs, "[missing argument]").is_empty());
}

struct Recording(Arc<Mutex<Vec<String>>>);

impl ToolRunner for Recording {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        self.0.lock().expect("lock").push(name.to_string());
        match name {
            "finish" => "[TASK_COMPLETE] ok".to_string(),
            "create_file" => format!(
                "[created {} (1 bytes)]",
                args.get("path").and_then(Value::as_str).unwrap_or("?")
            ),
            _ => "[ok]".to_string(),
        }
    }
}

fn truncated(args: Value) -> ChatResponse {
    let mut r = ChatResponse::call("", "create_file", args);
    r.done_reason = Some("length".into());
    r.eval_count = Some(8192);
    r
}

fn run_recording<F: FnMut(usize) -> ChatResponse>(
    workdir: &Path,
    f: F,
) -> (Vec<Message>, Vec<String>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", workdir),
        Responder { n: 0, f },
        Recording(seen.clone()),
    );
    let msgs = a
        .run(start("Write a large file and verify it."), None, None)
        .expect("loop");
    let calls = seen.lock().expect("lock").clone();
    (msgs, calls)
}

#[test]
fn truncated_tool_call_gets_chunking_advice_instead_of_verbatim_retry() {
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, calls) = run_recording(d.path(), |n| match n {
        1 => truncated(json!({"content": "x".repeat(300)})),
        2 => ChatResponse::call(
            "",
            "create_file",
            json!({"path": "app.py", "content": "print(1)"}),
        ),
        _ => ChatResponse::call("done", "finish", json!({"summary": "wrote app.py"})),
    });
    // The truncated call was never executed; only the complete one reached the tools.
    assert_eq!(calls.iter().filter(|c| *c == "create_file").count(), 1);
    let err = msgs
        .iter()
        .find(|m| m.role == Role::Tool && m.content.contains("truncated"))
        .expect("truncation tool error");
    assert!(err.content.contains("done_reason=length"));
    assert!(err.content.contains("\"path\""));
    assert!(err.content.contains("8192"), "{}", err.content);
    let note = user_msgs(&msgs, "hit the generation token limit");
    assert_eq!(note.len(), 1);
    assert!(note[0].content.contains("append_file"));
    assert!(!msgs
        .iter()
        .any(|m| m.content.contains("retry with the same content")));
}

#[test]
fn truncated_response_with_complete_call_runs_normally() {
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, calls) = run_recording(d.path(), |n| match n {
        1 => truncated(json!({"path": "app.py", "content": "print(1)"})),
        _ => ChatResponse::call("done", "finish", json!({"summary": "wrote app.py"})),
    });
    assert_eq!(calls.iter().filter(|c| *c == "create_file").count(), 1);
    assert!(!msgs.iter().any(|m| m.content.contains("truncated")));
    assert!(msgs
        .iter()
        .any(|m| m.content.starts_with("[created app.py")));
}

// ---- isolated corrector (Boubou, bead boubou-65d.7; bench/probes.py loop probe) ----

const FETCH_TASK: &str = "1. Run `python fetch.py` to download dataset.csv from the data mirror.\n2. Write summary.md with one line: the number of rows in dataset.csv.";
const FETCH_FAIL: &str = "[exit 1]\nSTDOUT:\nconnecting to mirror.example.net ...\nSTDERR:\nConnectionError: dataset mirror unreachable (HTTP 503 Service Unavailable)";

/// A backend that records how many messages each call saw, and answers from a closure.
struct Lens<F: FnMut(usize) -> ChatResponse> {
    n: usize,
    f: F,
    seen: Arc<Mutex<Vec<(usize, String)>>>,
}

impl<F: FnMut(usize) -> ChatResponse> ChatBackend for Lens<F> {
    fn chat(
        &mut self,
        _m: &str,
        msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        self.n += 1;
        let joined: String = msgs
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        self.seen.lock().expect("lock").push((msgs.len(), joined));
        Ok((self.f)(self.n))
    }
}

/// fetch.py always fails; `python3 fetch.py --mirror b` works; create_file lands on disk.
struct FetchTools {
    workdir: PathBuf,
}

impl ToolRunner for FetchTools {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        let a = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        match name {
            "run_bash" if a("command").contains("--mirror") => {
                let _ = std::fs::write(self.workdir.join("dataset.csv"), "a,b\n1,2\n");
                "[exit 0]\nSTDOUT:\nsaved dataset.csv (2 rows)".to_string()
            }
            "run_bash" if a("command").contains("fetch.py") => FETCH_FAIL.to_string(),
            "run_bash" => "[exit 0]\nSTDOUT:\n".to_string(),
            "list_dir" => "fetch.py".to_string(),
            "create_file" => {
                let _ = std::fs::write(self.workdir.join(a("path")), a("content"));
                format!("[created {}]", a("path"))
            }
            "finish" => "[TASK_COMPLETE] done".to_string(),
            _ => "[ok]".to_string(),
        }
    }
}

fn fetch_call() -> ChatResponse {
    ChatResponse::call("", "run_bash", json!({"command": "python fetch.py"}))
}

fn run_fetch<F: FnMut(usize) -> ChatResponse>(
    d: &Path,
    ablation: &[&str],
    f: F,
) -> (Vec<Message>, Vec<(usize, String)>) {
    run_fetch_as(d, "gemma4:12b", ablation, f)
}

fn run_fetch_as<F: FnMut(usize) -> ChatResponse>(
    d: &Path,
    model: &str,
    ablation: &[&str],
    f: F,
) -> (Vec<Message>, Vec<(usize, String)>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    // The task names fetch.py: it must exist or gate 3b asks for it.
    std::fs::write(d.join("fetch.py"), "import sys; sys.exit(1)\n").expect("fixture");
    let mut params = RunParams::plain(model, d);
    params
        .ablation
        .extend(ablation.iter().map(|s| s.to_string()));
    let mut a = Agent::new(
        params,
        Lens {
            n: 0,
            f,
            seen: seen.clone(),
        },
        FetchTools {
            workdir: d.to_path_buf(),
        },
    );
    a.sessions_dir = d.join("sessions");
    // `BOUBOU_TEST_LOG=1 cargo test …` prints the loop's log lines.
    let mut a = a.with_sink(|e| {
        if std::env::var_os("BOUBOU_TEST_LOG").is_some() {
            if let AgentEvent::Log(l) = e {
                eprintln!("{l}");
            }
        }
    });
    let msgs = a.run(start(FETCH_TASK), None, None).expect("loop");
    let seen = seen.lock().expect("lock").clone();
    (msgs, seen)
}

#[test]
fn second_identical_failure_consults_the_corrector_in_isolation_and_a_give_up_blocks_the_call() {
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, seen) = run_fetch(d.path(), &[], |n| match n {
        1 => fetch_call(),
        2 => ChatResponse::call("", "list_dir", json!({"path": "."})),
        3 => fetch_call(), // second identical failure → corrector is call 4
        4 => ChatResponse::text(
            "GIVE UP: the mirror answers HTTP 503; no change to the command fixes a remote outage.",
        ),
        5 => fetch_call(), // the model tries once more: blocked, not run
        6 => ChatResponse::call(
            "",
            "create_file",
            json!({"path": "summary.md", "content": "dataset.csv: download failed (HTTP 503); row count unknown."}),
        ),
        _ => ChatResponse::call(
            "",
            "finish",
            json!({"summary": "dataset.csv could not be downloaded (HTTP 503 from the mirror); summary.md records the failure."}),
        ),
    });
    // The corrector saw two messages and none of the conversation.
    let (len, text) = &seen[3];
    assert_eq!(*len, 2, "fresh context: system + facts");
    assert!(
        text.contains("python fetch.py")
            && text.contains("503")
            && text.contains(FETCH_TASK.lines().next().expect("task line"))
    );
    assert!(
        !text.contains("list_dir"),
        "no history leaks into the corrector"
    );
    // The give-up note spells out the honest exit, naming the missing file.
    let notes = user_msgs(&msgs, "Corrector verdict");
    assert_eq!(notes.len(), 1);
    assert!(notes[0]
        .content
        .contains("dataset.csv: could not be produced"));
    assert!(notes[0].content.contains("do NOT create the missing input"));
    // The third identical call never ran.
    let blocked: Vec<&Message> = msgs
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.starts_with("[blocked: the corrector"))
        .collect();
    assert_eq!(blocked.len(), 1);
    assert_eq!(
        msgs.iter()
            .filter(|m| m.role == Role::Tool && m.content.starts_with("[exit 1]"))
            .count(),
        2,
        "fetch.py ran exactly twice"
    );
    // One consultation only (the extra model call is gate 6's pre-delivery re-read), and
    // the honest finish went through.
    assert_eq!(
        seen.iter()
            .filter(|(_, t)| t.contains("You are the corrector"))
            .count(),
        1
    );
    assert!(user_msgs(&msgs, "claims a verified result").is_empty());
    assert!(
        user_msgs(&msgs, "finish was rejected").is_empty(),
        "honest summary excuses dataset.csv"
    );
}

#[test]
fn corrector_retry_runs_now_and_is_recorded_as_a_call() {
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, seen) = run_fetch(d.path(), &[], |n| match n {
        1 => fetch_call(),
        2 => ChatResponse::call("", "list_dir", json!({"path": "."})),
        3 => fetch_call(),
        4 => ChatResponse::call(
            "",
            "run_bash",
            json!({"command": "python3 fetch.py --mirror b"}),
        ),
        5 => ChatResponse::call(
            "",
            "create_file",
            json!({"path": "summary.md", "content": "2 rows"}),
        ),
        _ => ChatResponse::call(
            "",
            "finish",
            json!({"summary": "dataset.csv has 2 rows; summary.md written."}),
        ),
    });
    assert_eq!(seen[3].0, 2, "the retry came from the isolated call");
    let i = msgs
        .iter()
        .position(|m| {
            m.role == Role::Assistant
                && m.tool_calls
                    .iter()
                    .any(|t| t.function.arguments["command"] == "python3 fetch.py --mirror b")
        })
        .expect("corrected call in history");
    assert!(msgs[i + 1].role == Role::Tool && msgs[i + 1].content.starts_with("[exit 0]"));
    let note = user_msgs(&msgs, "an isolated retry ran");
    assert_eq!(note.len(), 1);
    assert!(note[0].content.contains("Continue from that result"));
    assert!(d.path().join("dataset.csv").exists());
    assert!(
        user_msgs(&msgs, "Corrector verdict").is_empty(),
        "no give-up note on a fix"
    );
}

#[test]
fn corrector_is_consulted_once_per_signature_and_not_when_ablated() {
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, seen) = run_fetch(d.path(), &["corrector"], |n| match n {
        1 => fetch_call(),
        2 => ChatResponse::call("", "list_dir", json!({"path": "."})),
        3 => fetch_call(),
        _ => ChatResponse::call(
            "",
            "finish",
            json!({"summary": "fetch.py failed (503); giving up."}),
        ),
    });
    assert!(
        seen.iter()
            .all(|(_, t)| !t.contains("You are the corrector")),
        "no isolated call"
    );
    assert!(user_msgs(&msgs, "Corrector").is_empty());
    assert!(msgs
        .iter()
        .all(|m| !m.content.starts_with("[blocked: the corrector")));
}

#[test]
fn honest_failure_summary_is_not_bounced_into_ticking_the_plan() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(
        d.path().join("todo.json"),
        r#"[{"item":"run fetch.py","done":false},{"item":"write summary.md","done":false}]"#,
    )
    .expect("w");
    let (msgs, _seen) = run_fetch(d.path(), &[], |n| match n {
        1 => ChatResponse::call(
            "",
            "create_file",
            json!({"path": "summary.md", "content": "dataset.csv: download failed (HTTP 503)."}),
        ),
        _ => ChatResponse::call(
            "",
            "finish",
            json!({"summary": "The dataset download failed with HTTP 503; summary.md records it and dataset.csv was not produced."}),
        ),
    });
    assert!(
        user_msgs(&msgs, "not marked done").is_empty(),
        "no plan-sync refusal"
    );
    let todo = std::fs::read_to_string(d.path().join("todo.json")).expect("todo");
    assert!(
        todo.contains("false"),
        "failed steps are not ticked for the model"
    );
    assert!(
        user_msgs(&msgs, "finish was rejected").is_empty(),
        "stem mention excuses dataset.csv"
    );
}

fn corrector_calls(seen: &[(usize, String)]) -> Vec<usize> {
    seen.iter()
        .enumerate()
        .filter(|(_, (_, t))| t.contains("You are the corrector"))
        .map(|(i, _)| i + 1)
        .collect()
}

fn summary_write() -> ChatResponse {
    ChatResponse::call(
        "",
        "create_file",
        json!({"path": "summary.md", "content": "dataset.csv: download failed (HTTP 503); row count unknown."}),
    )
}

#[test]
fn a_successful_write_between_two_identical_failures_resets_the_corrector_count() {
    // 65d.13: fetch fails, the model changes something, fetch fails again — that second
    // failure is a fresh attempt (count 1), not the second strike. Only a third identical
    // failure with nothing changed in between consults the corrector (chat call 6).
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, seen) = run_fetch(d.path(), &[], |n| match n {
        1 => fetch_call(),
        2 => summary_write(),
        3 => fetch_call(),
        4 => ChatResponse::call("", "list_dir", json!({"path": "."})),
        5 => fetch_call(),
        6 => ChatResponse::text("GIVE UP: the mirror answers HTTP 503 whatever the call."),
        _ => ChatResponse::call(
            "",
            "finish",
            json!({"summary": "fetch.py failed with HTTP 503 twice; dataset.csv was not produced, summary.md says so."}),
        ),
    });
    assert_eq!(
        corrector_calls(&seen),
        vec![6],
        "consulted once, after the third failure"
    );
    assert_eq!(user_msgs(&msgs, "Corrector").len(), 1);
}

#[test]
fn a_successful_write_lifts_a_corrector_block() {
    // The block says "this exact call cannot succeed here"; once a write changed the world
    // the call is allowed to run again (a verification command after a fix, in the probe).
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, seen) = run_fetch(d.path(), &[], |n| match n {
        1 => fetch_call(),
        2 => ChatResponse::call("", "list_dir", json!({"path": "."})),
        3 => fetch_call(), // second strike → corrector is chat call 4
        4 => ChatResponse::text("GIVE UP: the mirror answers HTTP 503 whatever the call."),
        5 => summary_write(), // the world changed
        6 => fetch_call(),    // runs again instead of being blocked
        _ => ChatResponse::call(
            "",
            "finish",
            json!({"summary": "fetch.py failed with HTTP 503; dataset.csv was not produced, summary.md says so."}),
        ),
    });
    assert_eq!(corrector_calls(&seen), vec![4]);
    assert!(
        msgs.iter()
            .all(|m| !m.content.starts_with("[blocked: the corrector")),
        "no blocked output after the write"
    );
    let fetch_runs = msgs
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.contains("503"))
        .count();
    assert_eq!(fetch_runs, 3, "the third fetch ran for real");
}

#[test]
fn gemma4_profile_reads_a_trailing_finish_line_as_the_finish_call() {
    // profiles/gemma4_e2b loop t03/t04/t05: after the give-up note the model wrote
    // "Finish: …" as prose and the dead-loop guard ended the run. Under the gemma4 profile
    // the line is the finish call and goes through the gates; the honest summary passes.
    let prose = "I cannot fix a remote outage from here.\nFinish: dataset.csv could not be produced (HTTP 503 from the mirror); summary.md records the failure.";
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, seen) = run_fetch_as(d.path(), "gemma4:e2b", &[], |n| match n {
        1 => summary_write(),
        _ => ChatResponse::text(prose),
    });
    // Call 2 is rescued and bounced once by the pre-delivery re-read (gate 6, no
    // verification ran); call 3 is rescued again and accepted.
    assert_eq!(seen.len(), 3, "prose turns ended the run through the gates");
    assert!(msgs
        .iter()
        .any(|m| m.tool_calls.iter().any(|t| t.function.name == "finish")));
    // An accepted finish ends the run on the assistant turn that carried it.
    assert!(
        msgs.last().is_some_and(|m| m.role == Role::Assistant
            && m.tool_calls.iter().any(|t| t.function.name == "finish")),
        "the rescued finish was accepted"
    );

    // A model without the profile flag gets the ordinary "continue with tools" turn instead.
    let d2 = tempfile::tempdir().expect("tmp");
    let (msgs2, seen2) = run_fetch_as(d2.path(), "qwen3.5:9b", &[], |n| match n {
        1 => summary_write(),
        2 => ChatResponse::text(prose),
        _ => ChatResponse::call(
            "",
            "finish",
            json!({"summary": "dataset.csv could not be produced (HTTP 503); summary.md records the failure."}),
        ),
    });
    // Prose → "continue with tools" nudge → real finish → gate 6 → finish again.
    assert_eq!(
        seen2.len(),
        4,
        "prose did not end the run without the profile"
    );
    assert!(
        msgs2
            .iter()
            .filter(|m| m.role == Role::Assistant && m.content.contains("Finish:"))
            .all(|m| m.tool_calls.is_empty()),
        "the prose turn stayed prose"
    );
}

// ---- the system prompt follows the route, every turn (bead boubou-c4b) ----

#[test]
fn task_turn_after_a_question_gets_the_task_prompt_back() {
    // Turn 1 is a question: the loop swaps in the chat prompt and the history keeps it,
    // which is what the web UI saves and continues from.
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |_| ChatResponse::text("A distributed hash table spreads keys over nodes."),
        },
        OkTools,
    );
    let mut hist = a
        .run(
            start("Can you tell me how a distributed hashtable works ?"),
            None,
            None,
        )
        .expect("loop");
    assert!(hist[0].role == Role::System && hist[0].content.contains("chatting with the user"));

    // Turn 2 is a task on the same history: the task prompt must be back, once, in front.
    hist.push(Message::user(
        "Write a three-bullet summary of that explanation for me",
    ));
    let mut b = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |_| {
                ChatResponse::call(
                    "- keys hash to nodes\n- lookups hop closer\n- replication for churn",
                    "finish",
                    json!({"summary": "Summarised the DHT explanation in three bullets in the reply text."}),
                )
            },
        },
        OkTools,
    );
    let out = b.run(hist, None, None).expect("loop");
    let sys: Vec<&Message> = out.iter().filter(|m| m.role == Role::System).collect();
    assert_eq!(sys.len(), 1, "exactly one system message");
    assert!(
        sys[0].content.contains("autonomous AI agent"),
        "task prompt expected, got: {}",
        &sys[0].content[..80.min(sys[0].content.len())]
    );
    assert!(!sys[0].content.contains("chatting with the user"));
    assert!(
        out.iter()
            .any(|m| m.role == Role::User && m.content.contains("Can you tell me how")),
        "history kept"
    );
    // The question turn before it ran under the chat prompt and said so: no task-mode nudges.
    assert!(user_msgs(&out, "Continue: keep making").is_empty());
}

#[test]
fn question_turn_after_a_task_gets_the_chat_prompt() {
    // The inverse already worked; pin it so the two branches stay symmetric.
    let d = tempfile::tempdir().expect("tmp");
    let _ = std::fs::write(d.path().join("m08.py"), "def f():\n    return 1\n");
    let mut hist = scripted(d.path(), &[]);
    assert!(hist[0].role == Role::System && hist[0].content == "sys");
    hist.push(Message::user("Are you confident about that ?"));
    let mut b = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |_| ChatResponse::text("Yes: the test passed afterwards."),
        },
        OkTools,
    );
    let out = b.run(hist, None, None).expect("loop");
    let sys: Vec<&Message> = out.iter().filter(|m| m.role == Role::System).collect();
    assert_eq!(sys.len(), 1);
    assert!(sys[0].content.contains("chatting with the user"));
}

#[test]
fn a_question_may_run_a_command_but_not_write_a_file() {
    // Bead boubou-o5a: "what should we do next?" is a `bd ready` away; with no shell on the
    // question route the model used web_search as one. run_bash is offered and runs (the
    // safety gate vets it); the file mutators stay blocked by the Q&A guard.
    let d = tempfile::tempdir().expect("tmp");
    let mut b = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |n| match n {
                1 => ChatResponse::call("", "run_bash", json!({"command": "git status --short"})),
                2 => ChatResponse::call(
                    "",
                    "create_file",
                    json!({"path": "notes.txt", "content": "x"}),
                ),
                _ => ChatResponse::text("The tree is clean; nothing is pending."),
            },
        },
        OkTools,
    );
    let out = b
        .run(
            vec![Message::user("What is the state of the repo ?")],
            None,
            None,
        )
        .expect("loop");
    let sys: Vec<&Message> = out.iter().filter(|m| m.role == Role::System).collect();
    assert!(
        sys[0].content.contains("chatting with the user"),
        "question route"
    );
    let tools: Vec<&Message> = out.iter().filter(|m| m.role == Role::Tool).collect();
    assert!(
        tools.iter().any(|m| m.content == "[ok]"),
        "run_bash ran on the question turn: {tools:#?}"
    );
    assert!(
        tools.iter().any(|m| m
            .content
            .starts_with("[blocked: the user is asking a question; create_file")),
        "create_file still blocked: {tools:#?}"
    );
}

// ---- recency pruning in the loop (bead boubou-65d.6) ----

#[test]
fn pruning_runs_from_half_the_window_before_the_ladder() {
    // Twelve 6,000-char results against a 40k window: the pre-call estimate crosses 50%
    // before the finish, pruning collapses the oldest results to fact lines, and the
    // ladder (L1 at 60%) never has to fire.
    let d = tempfile::tempdir().expect("tmp");
    let results: Vec<String> = (0..12)
        .map(|i| {
            format!(
                "head of file {i}\n{}\nlast line of file {i}",
                "y".repeat(6000)
            )
        })
        .collect();
    let calls: Vec<Call> = (0..12)
        .map(|i| {
            (
                "read_file",
                json!({"path": format!("f{i}.txt")}),
                Box::leak(results[i].clone().into_boxed_str()) as &'static str,
            )
        })
        .collect();
    let mut a = agent_for(d.path(), &calls);
    a.params.num_ctx = 40_000;
    a.params.prune_keep = boubou_core::agent::history::PRUNE_KEEP;
    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let sink = Arc::clone(&events);
    let mut a = a.with_sink(move |e| sink.lock().expect("lock").push(e));
    let out = a.run(start(TASK), None, None).expect("loop");
    let logs: Vec<String> = events
        .lock()
        .expect("lock")
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Log(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(
        logs.iter().any(|l| l.starts_with("[context pruning:")),
        "{logs:#?}"
    );
    assert!(
        !logs.iter().any(|l| l.contains("context compaction")),
        "pruning alone kept the estimate under L1: {logs:#?}"
    );
    let tools: Vec<&Message> = out
        .iter()
        .filter(|m| m.role == Role::Tool && !m.content.contains("TASK_COMPLETE"))
        .collect();
    let pruned: Vec<&&Message> = tools
        .iter()
        .filter(|m| m.content.starts_with("[earlier] read_file("))
        .collect();
    assert!(!pruned.is_empty());
    assert!(
        pruned[0].content.contains("head of file 0")
            && pruned[0].content.contains("last line of file 0")
    );
    // (the loop's own output cap may shorten a 6,000-char result; a fact line never has 1,000 y's)
    let last_read = tools
        .iter()
        .rev()
        .find(|m| m.content.starts_with("head of file"))
        .expect("a verbatim read result");
    assert!(
        last_read.content.contains(&"y".repeat(1000)),
        "the latest result is verbatim"
    );
}

#[test]
fn prune_keep_zero_is_the_default_and_disables_pruning() {
    let d = tempfile::tempdir().expect("tmp");
    let results: Vec<String> = (0..12).map(|_| "y".repeat(6000)).collect();
    let calls: Vec<Call> = (0..12)
        .map(|i| {
            (
                "read_file",
                json!({"path": format!("f{i}.txt")}),
                Box::leak(results[i].clone().into_boxed_str()) as &'static str,
            )
        })
        .collect();
    let mut a = agent_for(d.path(), &calls);
    a.params.num_ctx = 40_000;
    assert_eq!(a.params.prune_keep, 0);
    let out = a.run(start(TASK), None, None).expect("loop");
    assert!(out.iter().all(|m| !m.content.starts_with("[earlier]")));
}
