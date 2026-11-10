//! Port of the `_scripted_loop` wiring tests from `tests/test_guards.py` and
//! `tests/test_output_cap.py`: the loop driven by a scripted backend and a scripted tool
//! runner, no network, no model.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rufous_kernel::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use rufous_kernel::agent::{Agent, AgentEvent, Gate, Gates, RunParams, ToolRunner, TurnPolicy};
use rufous_kernel::guards::budget::{elapsed_meta_path, load_elapsed, save_elapsed};
use rufous_kernel::guards::idle::{QA_IDLE_CAP, QA_IDLE_NUDGE};
use rufous_kernel::ollama::{FunctionCall, Message, Role, ToolCall};
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

/// Looks the (name, args) pair up in the script; `finish` always completes. A pair listed
/// in `repeat` answers with that result from its second run on (the finish gate re-executes
/// a check through this same path).
struct ScriptedTools {
    calls: Vec<Call>,
    repeat: Vec<Call>,
    served: Vec<Value>,
}

impl ToolRunner for ScriptedTools {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        let key = json!([name, args]);
        let again = self.served.contains(&key);
        self.served.push(key);
        if again {
            for (n, a, r) in &self.repeat {
                if *n == name && a == args {
                    return r.to_string();
                }
            }
        }
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
        repeat: Vec::new(),
        served: Vec::new(),
    };
    // TASK names m08.py as its input; the deliverable gate (integrity, gate 3b) would
    // otherwise ask for it once in every scripted run.
    let _ = std::fs::write(workdir.join("m08.py"), "def f():\n    return 1\n");
    let params = RunParams::plain("gemma4:12b", workdir);
    let mut a = Agent::new(params, backend, tools);
    a.sessions_dir = workdir.join("sessions");
    a
}

/// `agent_for` under another model name and the bench's ablation switches.
fn agent_as(
    workdir: &Path,
    calls: &[Call],
    model: &str,
    ablation: &[&str],
) -> Agent<Scripted, ScriptedTools> {
    let mut a = agent_for(workdir, calls);
    a.params.model = model.to_string();
    a.params
        .ablation
        .extend(ablation.iter().map(|s| s.to_string()));
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

// ---- integrity gate (rufous, 2026-10-28; bench/probes.py findings) ----

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
    assert!(
        todo.contains("\"done\":false"),
        "the plan is the model's record: the harness must not tick steps it did not see happen: {todo}"
    );
}

const STALE_AWK: &str = "awk -F, '$2==\"paid\"{s+=$3} END{print s}' data.csv > result.txt";

/// check.py OK, then a failing `awk … > result.txt` emptied the file (probes_integrity2
/// t05), then "check.py printed OK" on the stale pass.
fn stale_calls() -> Vec<Call> {
    vec![
        check_run("[exit 0]\nSTDOUT:\nOK"),
        (
            "run_bash",
            json!({"command": STALE_AWK}),
            "[exit 2]\nSTDERR:\nawk: syntax error",
        ),
    ]
}

#[test]
fn a_check_that_passed_before_later_file_changes_is_rerun_and_accepted_when_it_still_passes() {
    // The gate re-executes the model's own `python check.py` through the tool path instead
    // of refusing on suspicion; the scripted tool answers OK again, so the finish stands and
    // the fresh pass is the self-check.
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "").expect("w");
    let msgs = agent_with(d.path(), &stale_calls(), "", "check.py printed OK")
        .run(start(CHECK_TASK), None, None)
        .expect("loop");
    assert!(
        user_msgs(&msgs, "verified state may be gone").is_empty(),
        "{msgs:#?}"
    );
    assert!(user_msgs(&msgs, "fails now").is_empty());
    assert!(user_msgs(&msgs, "Pre-delivery check").is_empty());
    assert_eq!(msgs.last().map(|m| &m.role), Some(&Role::Assistant));
}

#[test]
fn a_check_that_passed_before_later_file_changes_is_rerun_and_refused_when_it_fails_now() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "").expect("w");
    let mut a = agent_with(d.path(), &stale_calls(), "", "check.py printed OK");
    a.tools.repeat = vec![check_run("[exit 1]\nSTDOUT:\nFAIL: result.txt is empty")];
    let msgs = a.run(start(CHECK_TASK), None, None).expect("loop");
    let rejects = user_msgs(&msgs, "fails now");
    assert_eq!(
        rejects.len(),
        1,
        "the re-executed check fails: refused once, with its output: {msgs:#?}"
    );
    // The failed re-run is now on the record, so the claims gate carries on with the truth
    // ("the last verification command exited 1") rather than this gate refusing again.
    let claims = user_msgs(
        &msgs,
        "the last verification command `python check.py` exited 1",
    );
    assert_eq!(claims.len(), 2, "{msgs:#?}");
    assert!(rejects[0].content.contains("`python check.py`"));
    assert!(rejects[0].content.contains("exit 1"));
    assert!(rejects[0].content.contains("FAIL: result.txt is empty"));
    assert!(
        !rejects[0].content.contains("pytest") && !rejects[0].content.contains("unittest"),
        "the gate names the model's command, never a runner of its own"
    );
    assert!(user_msgs(&msgs, "verified state may be gone").is_empty());
    assert_eq!(msgs.last().map(|m| &m.role), Some(&Role::Assistant));
}

#[test]
fn a_stale_check_the_harness_must_not_re_execute_is_left_to_the_claims_gate() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("result.txt"), "").expect("w");
    let calls = vec![
        (
            "run_bash",
            json!({"command": "python check.py && git commit -am ok"}),
            "[exit 0]\nSTDOUT:\nOK",
        ),
        (
            "run_bash",
            json!({"command": STALE_AWK}),
            "[exit 2]\nSTDERR:\nawk: syntax error",
        ),
    ];
    let msgs = agent_with(d.path(), &calls, "", "check.py printed OK")
        .run(start(CHECK_TASK), None, None)
        .expect("loop");
    let rejects = user_msgs(&msgs, "so the verified state may be gone");
    assert_eq!(
        rejects.len(),
        2,
        "a committing command is not re-run; the lexical stale rule stands"
    );
    assert!(user_msgs(&msgs, "fails now").is_empty());
}

#[test]
fn changing_files_in_a_tested_workdir_without_running_a_check_is_asked_for_one() {
    // TASK edits m08.py; the workdir has test_m08.py; the model never runs anything.
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(
        d.path().join("test_m08.py"),
        "def test_f():\n    assert f() == 1\n",
    )
    .expect("w");
    let calls = vec![edit(1, true)];
    let msgs = agent_with(d.path(), &calls, "", "Fixed the bug in m08.py.")
        .run(start(TASK), None, None)
        .expect("loop");
    let asks = user_msgs(&msgs, "no test or check command ran");
    assert_eq!(
        asks.len(),
        3,
        "asked for a check, three times at most, then let through: {msgs:#?}"
    );
    assert!(
        !asks[0].content.contains("pytest") && !asks[0].content.contains("unittest"),
        "the gate asks for the project's tests and names no runner"
    );
    // The same turn, with the summary admitting the tests were not run: honesty passes.
    let d2 = tempfile::tempdir().expect("tmp");
    std::fs::write(
        d2.path().join("test_m08.py"),
        "def test_f():\n    assert f() == 1\n",
    )
    .expect("w");
    let msgs = agent_with(
        d2.path(),
        &calls,
        "",
        "Edited m08.py; I could not run the tests, so the fix is unverified and may fail.",
    )
    .run(start(TASK), None, None)
    .expect("loop");
    assert!(
        user_msgs(&msgs, "no test or check command ran").is_empty(),
        "{msgs:#?}"
    );
    // And a workdir without tests asks for nothing.
    let d3 = tempfile::tempdir().expect("tmp");
    let msgs = agent_with(d3.path(), &calls, "", "Fixed the bug in m08.py.")
        .run(start(TASK), None, None)
        .expect("loop");
    assert!(user_msgs(&msgs, "no test or check command ran").is_empty());
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

#[test]
fn a_duplicate_call_runs_again_when_the_loop_ladders_are_ablated() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = vec![
        ("read_file", json!({"path": "m08.py"}), "code"),
        ("read_file", json!({"path": "m08.py"}), "code"),
    ];
    let msgs = agent_as(d.path(), &calls, "gemma4:12b", &["anti_loop"])
        .run(start(TASK), None, None)
        .expect("loop");
    let served: Vec<&Message> = msgs
        .iter()
        .filter(|m| m.role == Role::Tool && m.content == "code")
        .collect();
    assert_eq!(served.len(), 2, "both reads reach the tool");
    assert!(msgs
        .iter()
        .all(|m| !(m.role == Role::Tool && m.content.starts_with("[blocked:"))));
    assert!(user_msgs(&msgs, "Do not retry it unchanged").is_empty());
}

// ---- no tool is withheld by parameter count ----

#[test]
fn a_two_billion_model_gets_edit_file_like_any_other() {
    // Mingbird's `_small_model` hid edit_file from every ≤4B model before the first step.
    // Measured (bead boubou-6uj), the rule lost the rename cell six times out of six and
    // won nothing, so the loop no longer looks at the parameter count: the first edit_file
    // call from a 2B model runs, and the tool is advertised to it on the flat path.
    let calls: Vec<Call> = vec![edit(1, true)];
    for model in ["gemma4:e2b", "qwen3.5:4b", "gemma4:12b"] {
        let d = tempfile::tempdir().expect("tmp");
        let msgs = agent_as(d.path(), &calls, model, &[])
            .run(start(TASK), None, None)
            .expect("loop");
        assert!(
            msgs.iter()
                .any(|m| m.role == Role::Tool && m.content == EDIT_OK),
            "{model}: the edit must run"
        );
        assert!(msgs
            .iter()
            .all(|m| !m.content.starts_with("[tool edit_file is disabled")));
    }
}

// ---- the profile's workdir-prefix rule reaches the executor through the loop ----

fn run_e2b_read(workdir: &Path, ablation: &[&str]) -> Vec<Message> {
    use rufous_kernel::exec::Executor;
    use rufous_kernel::safety::SafetyPolicy;
    let mut params = RunParams::plain("gemma4:e2b", workdir);
    params
        .ablation
        .extend(ablation.iter().map(|s| s.to_string()));
    params.apply_profile(&Default::default());
    let tools = Executor::with_policy(workdir, SafetyPolicy::default());
    let mut a = Agent::new(
        params,
        Responder {
            n: 0,
            f: |n| match n {
                1 => ChatResponse::call("", "read_file", json!({"path": "workdir/slug.py"})),
                _ => ChatResponse::call("done", "finish", json!({"summary": "read it"})),
            },
        },
        tools,
    );
    a.run(
        start("What does slug.py do? Don't change any files."),
        None,
        None,
    )
    .expect("loop")
}

#[test]
fn gemma_e2b_gets_the_workdir_prefix_rule_from_its_profile_until_it_is_ablated() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path().join("workdir");
    std::fs::create_dir(&wd).expect("mkdir");
    std::fs::write(wd.join("slug.py"), "def slugify(t):\n    return t\n").expect("fixture");

    let msgs = run_e2b_read(&wd, &[]);
    let tool: Vec<&Message> = msgs.iter().filter(|m| m.role == Role::Tool).collect();
    assert!(
        tool[0].content.contains("def slugify"),
        "{}",
        tool[0].content
    );
    assert!(
        tool[0]
            .content
            .contains("(path rebased: workdir/slug.py → slug.py;"),
        "{}",
        tool[0].content
    );

    let msgs = run_e2b_read(&wd, &["path_rebase"]);
    let tool: Vec<&Message> = msgs.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(tool[0].content, "[not found: workdir/slug.py]");
}

// ---- edit_file answers with the truth about the file (bead boubou-bk9.3) ----

#[test]
fn a_stale_second_edit_is_no_change_and_the_first_edit_stands() {
    // The model edits, then re-issues the edit against the text it already changed, quoting
    // `old` a little differently (so the duplicate block does not take the call first) and
    // the same `new`. With the matcher on the second call is no-change, not a failure; off,
    // it is the old miss.
    use rufous_kernel::exec::Executor;
    use rufous_kernel::safety::SafetyPolicy;
    fn run(workdir: &Path, ablation: &[&str]) -> Vec<Message> {
        let mut params = RunParams::plain("qwen3.5:9b", workdir);
        params
            .ablation
            .extend(ablation.iter().map(|s| s.to_string()));
        let tools = Executor::with_policy(workdir, SafetyPolicy::default());
        let mut a = Agent::new(
            params,
            Responder {
                n: 0,
                f: |n| match n {
                    1 => ChatResponse::call(
                        "",
                        "edit_file",
                        json!({"path": "repo.py", "old": "def fetchAll(conn):", "new": "def fetch_all(conn):"}),
                    ),
                    2 => ChatResponse::call(
                        "",
                        "edit_file",
                        json!({"path": "repo.py", "old": "def fetchAll(conn):\n", "new": "def fetch_all(conn):"}),
                    ),
                    _ => ChatResponse::call("done", "finish", json!({"summary": "renamed"})),
                },
            },
            tools,
        );
        a.run(
            start("Rename fetchAll to fetch_all in repo.py."),
            None,
            None,
        )
        .expect("loop")
    }
    for (ablation, second) in [
        (
            &[][..],
            "[no change: this edit is already done. repo.py lines 1–1",
        ),
        (
            &["edit_match"][..],
            "[edit failed: old text not found. Preview",
        ),
    ] {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::write(d.path().join("repo.py"), "def fetchAll(conn):\n    pass\n")
            .expect("fixture");
        let msgs = run(d.path(), ablation);
        let tool: Vec<&Message> = msgs.iter().filter(|m| m.role == Role::Tool).collect();
        assert!(
            tool[0].content.starts_with("[edited repo.py"),
            "{ablation:?}: {}",
            tool[0].content
        );
        assert!(
            tool[1].content.starts_with(second),
            "{ablation:?}: {}",
            tool[1].content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("repo.py")).expect("read"),
            "def fetch_all(conn):\n    pass\n"
        );
    }
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
    assert!(matches!(err, Some(rufous_kernel::Error::Aborted(_))));
}

// ---- repeat-output detector / Q&A wrap-up ----

#[test]
fn qa_text_answer_is_force_closed() {
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |_| ChatResponse::text("Hello! I'm rufous."),
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
fn general_policy_text_reply_ends_the_turn_even_for_a_task_shaped_message() {
    // `rufous chat` and the web UI's chat: a plain text reply is the answer even when the
    // message reads like a task, and nothing is appended for the next turn to see.
    let d = tempfile::tempdir().expect("tmp");
    let mut params = RunParams::plain("gemma4:12b", d.path());
    params.policy = TurnPolicy::conversation();
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
    assert!(user_msgs(&out, "Your answer is already sufficient").is_empty());
    assert_eq!(
        out.iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_empty())
            .count(),
        1
    );
    assert!(
        out.last().is_some_and(|m| m.role == Role::Assistant),
        "the history ends on the reply: {out:#?}"
    );
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
        .filter(|c| !rufous_kernel::guards::qa::is_harness_note(c))
        .collect();
    assert!(offenders.is_empty(), "{offenders:?}");
}

// ---- malformed tool names ----

#[test]
fn a_leaked_template_fragment_as_a_tool_name_gets_the_invalid_name_correction() {
    // boubou-1sm: the model emitted the raw text of a shell command plus a closing XML tag
    // as the call's name. The note names the fault and where the command belongs, instead
    // of the generic "not available" text, and the leaked text does not sprawl.
    let d = tempfile::tempdir().expect("tmp");
    let leaked = "bd show initial-filesharing-app-setup/1-1-dependencies\n</parameter";
    let calls = vec![
        (leaked, json!({}), "unused"),
        edit(0, true),
        (
            "run_bash",
            json!({"command": "python test_suite.py"}),
            "[exit 0]\n10 passed",
        ),
    ];
    let msgs = scripted(d.path(), &calls);
    let note = msgs
        .iter()
        .find(|m| m.role == Role::Tool && m.content.starts_with("[invalid tool name"))
        .map(|m| m.content.as_str())
        .expect("the correction is the call's result");
    assert!(note.contains("run_bash"), "{note}");
    assert!(!note.contains("is not available"), "{note}");
    assert!(!note.contains('\n'), "whitespace collapsed: {note}");
    assert!(
        msgs.iter().all(|m| !m.content.contains("[tool bd show")),
        "no generic not-available text for the leaked name"
    );
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
    let state = rufous_kernel::agent::session::state_path(&sessions, "acp_1029_094732");
    let meta = rufous_kernel::guards::budget::elapsed_meta_for(&state);
    assert_eq!(
        meta.file_name().and_then(|n| n.to_str()),
        Some("acp_1029_094732.state.meta.json")
    );
    assert_eq!(
        rufous_kernel::guards::budget::elapsed_meta_for(
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
    rufous_kernel::guards::budget::save_elapsed_at(&meta, 100.0);
    let mut a = agent_for(&wd, &calls);
    a.checkpoint = Some(state.clone());
    a.params.budget_env_sec = Some(3600.0);
    let msgs = a.run(start(TASK), None, None).expect("loop");
    assert!(state.is_file(), "missing {}", state.display());
    let saved: Vec<Message> =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("read")).expect("json");
    assert!(saved.len() >= 3 && saved.len() <= msgs.len());
    assert!(meta.is_file(), "elapsed sidecar beside the checkpoint");
    assert!(rufous_kernel::guards::budget::load_elapsed_at(&meta) >= 100.0);
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

// ---- isolated corrector (rufous, bead boubou-65d.7; bench/probes.py loop probe) ----

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
    // `RUFOUS_TEST_LOG=1 cargo test …` prints the loop's log lines.
    let mut a = a.with_sink(|e| {
        if std::env::var_os("RUFOUS_TEST_LOG").is_some() {
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

// ---- follow-up turns end when acted on and reported (bead boubou-he2) ----

/// A session with real work behind it, then a one-action steer.
fn follow_up_history(steer: &str) -> Vec<Message> {
    let mut call = Message::assistant("");
    call.tool_calls = vec![ToolCall {
        function: FunctionCall {
            name: "run_bash".into(),
            arguments: json!({"command": "flutter pub get"}),
        },
    }];
    vec![
        Message::system("sys"),
        Message::user("Work through the openspec task list for this project."),
        call,
        Message::tool_output("[exit 0]\nGot dependencies."),
        Message::assistant("Dependencies are in. Task 1.1 is done."),
        Message::user(steer),
    ]
}

#[test]
fn a_follow_up_ends_once_acted_on_and_reported() {
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |n| match n {
                1 => ChatResponse::call("", "run_bash", json!({"command": "bd done 9h3"})),
                2 => ChatResponse::text("Closed overshare-9h3 (mDNS peer discovery)."),
                _ => ChatResponse::call("", "read_file", json!({"path": "tasks.md"})),
            },
        },
        OkTools,
    );
    let out = a
        .run(
            follow_up_history("I think we already did that one. It can be closed."),
            None,
            None,
        )
        .expect("loop");
    let ran = out
        .iter()
        .filter(|m| m.role == Role::Tool && m.content == "[ok]")
        .count();
    assert_eq!(ran, 1, "only the close ran: {out:#?}");
    assert!(user_msgs(&out, "Continue").is_empty(), "no continue note");
    assert_eq!(user_msgs(&out, "reports the follow-up").len(), 1);
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
    let sys: Vec<&Message> = out.iter().filter(|m| m.role == Role::System).collect();
    assert!(
        !sys[0].content.contains("chatting with the user"),
        "a follow-up still runs under the task prompt"
    );
}

#[test]
fn a_follow_up_that_goes_quiet_is_asked_for_the_report_once() {
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |n| match n {
                1 => ChatResponse::call("", "run_bash", json!({"command": "bd done 9h3"})),
                2 => ChatResponse::text(""),
                _ => ChatResponse::text("Done: overshare-9h3 is closed."),
            },
        },
        OkTools,
    );
    let out = a
        .run(
            follow_up_history("that one is done, close it please"),
            None,
            None,
        )
        .expect("loop");
    assert_eq!(user_msgs(&out, "say so in one line").len(), 1);
    assert!(user_msgs(&out, "do real work now").is_empty());
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
}

#[test]
fn a_follow_up_without_an_action_keeps_the_task_rules() {
    // Text before any producing call is not a report: the continue note still applies.
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |n| match n {
                1 => ChatResponse::text("Sure, I will close it."),
                2 => ChatResponse::call("", "run_bash", json!({"command": "bd done 9h3"})),
                _ => ChatResponse::text("Closed."),
            },
        },
        OkTools,
    );
    let out = a
        .run(follow_up_history("close that one"), None, None)
        .expect("loop");
    assert_eq!(user_msgs(&out, "Continue").len(), 1, "{out:#?}");
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
}

#[test]
fn a_fresh_task_still_gets_the_continue_note_after_one_write() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_responder(d.path(), |n| match n {
        1 => ChatResponse::call(
            "",
            "create_file",
            json!({"path": "big.txt", "content": "x"}),
        ),
        2 => ChatResponse::text("I have written the file."),
        _ => ChatResponse::call("done", "finish", json!({"summary": "done"})),
    });
    assert!(!user_msgs(&out, "Continue").is_empty());
}

// ---- idle-streak guard (bead boubou-9c5) ----

/// A distinct read-only call per step: reads, listings, read-only commands, a plan update.
fn idle_call(n: usize) -> ChatResponse {
    match n % 4 {
        0 => ChatResponse::call("", "read_file", json!({"path": format!("f{n}.md")})),
        1 => ChatResponse::call("", "list_dir", json!({"path": format!("d{n}")})),
        2 => ChatResponse::call(
            "",
            "run_bash",
            json!({"command": format!("find specs -name '*{n}*' 2>/dev/null | sort")}),
        ),
        _ => ChatResponse::call("", "run_bash", json!({"command": format!("bd show x{n}")})),
    }
}

#[test]
fn a_question_turn_is_nudged_at_eight_reads_and_capped_at_twelve() {
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |n| {
                if n <= 16 {
                    idle_call(n)
                } else {
                    ChatResponse::text("The next item is overshare-2fp, the content cache.")
                }
            },
        },
        OkTools,
    );
    let out = a
        .run(
            vec![Message::user("What is the next item for us to work on ?")],
            None,
            None,
        )
        .expect("loop");
    let nudges = user_msgs(&out, "read-only calls on this question");
    assert_eq!(nudges.len(), 1, "one nudge at {}", QA_IDLE_NUDGE);
    let ran = out
        .iter()
        .filter(|m| m.role == Role::Tool && m.content == "[ok]")
        .count();
    assert_eq!(ran, QA_IDLE_CAP, "reads executed before the cap");
    let refused: Vec<&Message> = out
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.starts_with("[blocked: 12 read-only calls"))
        .collect();
    assert_eq!(
        refused.len(),
        2,
        "two refusals then the turn ends: {refused:#?}"
    );
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
}

#[test]
fn a_task_turn_is_nudged_at_twelve_and_twenty_four_reads_but_never_refused() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_responder(d.path(), |n| {
        if n <= 26 {
            idle_call(n)
        } else {
            ChatResponse::call("done", "finish", json!({"summary": "closed overshare-9h3"}))
        }
    });
    let nudges = user_msgs(&out, "calls only read");
    assert_eq!(nudges.len(), 2, "{nudges:#?}");
    assert!(nudges[0].content.contains("The last 12 calls"));
    assert!(nudges[1].content.contains("The last 24 calls"));
    assert!(nudges[0].content.contains("call finish now"));
    assert!(out
        .iter()
        .all(|m| !(m.role == Role::Tool && m.content.starts_with("[blocked: "))));
    let ran = out
        .iter()
        .filter(|m| m.role == Role::Tool && m.content == "[ok]")
        .count();
    assert_eq!(ran, 26, "every read ran");
}

#[test]
fn a_write_or_a_real_command_resets_the_idle_streak() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_responder(d.path(), |n| match n {
        9 => ChatResponse::call(
            "",
            "create_file",
            json!({"path": "lib/content/cache.dart", "content": "class Cache {}"}),
        ),
        18 => ChatResponse::call("", "run_bash", json!({"command": "bd done 9h3"})),
        n if n <= 26 => idle_call(n),
        _ => ChatResponse::call("done", "finish", json!({"summary": "done"})),
    });
    assert!(
        user_msgs(&out, "calls only read").is_empty(),
        "no run of 12 idle calls: {:#?}",
        user_msgs(&out, "calls only read")
    );
}

#[test]
fn two_empty_web_searches_in_a_row_draw_one_note() {
    struct NoResults;
    impl ToolRunner for NoResults {
        fn run_tool(&mut self, name: &str, args: &Value) -> String {
            match name {
                "finish" => "[TASK_COMPLETE] ok".into(),
                "web_search" => format!(
                    "[web_search: no relevant results for {:?} (0 hits). Rephrase the query with distinctive words, or web_fetch the site directly.]",
                    args["query"]
                ),
                _ => "[ok]".into(),
            }
        }
    }
    let d = tempfile::tempdir().expect("tmp");
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Responder {
            n: 0,
            f: |n| match n {
                1..=3 => ChatResponse::call(
                    "",
                    "web_search",
                    json!({"query": format!("bd ready overshare {n}")}),
                ),
                _ => ChatResponse::text("I could not find it on the web; run `bd ready` locally."),
            },
        },
        NoResults,
    );
    let out = a
        .run(
            vec![Message::user("Which bead is ready next on the web ?")],
            None,
            None,
        )
        .expect("loop");
    assert_eq!(user_msgs(&out, "Two web searches in a row").len(), 1);
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
    a.params.prune_keep = rufous_kernel::agent::history::PRUNE_KEEP;
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

// ---- the coding/general policy (editor and chat semantics), review 2026-10-29 ----

#[test]
fn interactive_text_reply_ends_the_turn_without_a_harness_note() {
    let d = tempfile::tempdir().expect("tmp");
    let mut params = RunParams::plain("gemma4:12b", d.path());
    params.policy = TurnPolicy::conversation();
    let mut a = Agent::new(
        params,
        Responder {
            n: 0,
            f: |_| ChatResponse::text("The parser lives in src/parse.rs; I would start there."),
        },
        OkTools,
    );
    let out = a
        .run(start("Refactor the parser and add tests"), None, None)
        .expect("loop");
    assert!(user_msgs(&out, "Continue: keep making").is_empty());
    assert!(user_msgs(&out, "Your answer is already sufficient").is_empty());
    assert!(user_msgs(&out, "replied in text").is_empty());
    assert_eq!(
        out.iter().filter(|m| m.role == Role::Assistant).count(),
        1,
        "{out:#?}"
    );
    assert!(
        out.last().is_some_and(|m| m.role == Role::Assistant),
        "the history ends on the reply, nothing appended for the next turn to see"
    );
}

#[test]
fn interactive_finish_is_accepted_without_gates() {
    let d = tempfile::tempdir().expect("tmp");
    // A bare finish with nothing delivered: gate 1 bounces it in task mode.
    let mut a = agent_with(d.path(), &[], "", "");
    a.params.policy = TurnPolicy::conversation();
    let out = a.run(start(TASK), None, None).expect("loop");
    assert!(
        user_msgs(&out, "finish was rejected").is_empty(),
        "{out:#?}"
    );
    assert!(user_msgs(&out, "Pre-delivery check").is_empty());
    assert_eq!(
        out.iter()
            .filter(|m| m.tool_calls.iter().any(|t| t.function.name == "finish"))
            .count(),
        1
    );
}

#[test]
fn the_sandbox_flag_pins_the_child_policy() {
    // A child's sandbox and its policy go together: whatever a caller sets as the policy,
    // AGENT_CHILD_SANDBOX runs TurnPolicy::child, so a child can never be a conversation
    // whose text reply ends the turn before the dispatcher's exit contract is met.
    let d = tempfile::tempdir().expect("tmp");
    let mut p = RunParams::plain("small:2b", d.path());
    assert_eq!(p.turn_policy(), TurnPolicy::unattended());
    p.policy = TurnPolicy::conversation();
    assert_eq!(p.turn_policy(), TurnPolicy::conversation());
    p.child_mode = true;
    assert_eq!(p.turn_policy(), TurnPolicy::child());
}

#[test]
fn a_session_resumed_under_another_policy_says_so() {
    // Refactor step 9: the session's meta names the policy of its last turn. Resumed under
    // the same policy nothing is said; resumed under another one the log names both, and
    // the meta then records the new one. A session Mingbird wrote (no key) is not a change.
    let d = tempfile::tempdir().expect("tmp");
    let logs: Arc<Mutex<Vec<String>>> = Arc::default();
    let run_under = |policy: TurnPolicy, msgs: Vec<Message>| {
        let sink = Arc::clone(&logs);
        let mut a = agent_with(d.path(), &[], "", "Hello.").with_sink(move |e| {
            if let AgentEvent::Log(l) = e {
                sink.lock().expect("lock").push(l);
            }
        });
        a.params.policy = policy;
        a.run(msgs, Some("s9"), None).expect("loop")
    };
    let changed = |logs: &Arc<Mutex<Vec<String>>>| -> Vec<String> {
        let mut g = logs.lock().expect("lock");
        let found = g
            .iter()
            .filter(|l| l.contains("ran under"))
            .cloned()
            .collect();
        g.clear();
        found
    };
    let sdir = d.path().join("sessions");

    let mut out = run_under(TurnPolicy::conversation(), start("hi there"));
    assert!(
        changed(&logs).is_empty(),
        "a fresh session has no previous policy"
    );
    assert_eq!(
        rufous_kernel::agent::session::recorded_policy(&sdir, "s9").as_deref(),
        Some("conversation")
    );

    out.push(Message::user("and again"));
    let mut out = run_under(TurnPolicy::conversation(), out);
    assert!(changed(&logs).is_empty(), "same policy: nothing to say");

    out.push(Message::user("now from the editor"));
    let editor = TurnPolicy {
        name: "editor",
        ..TurnPolicy::conversation()
    };
    let out = run_under(editor, out);
    let said = changed(&logs);
    assert_eq!(said.len(), 1, "{said:#?}");
    assert_eq!(
        said[0],
        "[policy] session s9 ran under conversation; this turn runs under editor"
    );
    assert_eq!(
        rufous_kernel::agent::session::recorded_policy(&sdir, "s9").as_deref(),
        Some("editor"),
        "the meta follows the turn that ran"
    );
    assert!(out.len() > 3);

    // Mingbird's meta has no policy key: a session it wrote resumes without a change line.
    let meta = sdir.join("s9.meta.json");
    let mut m: serde_json::Map<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(&meta).expect("meta")).expect("json");
    m.remove("policy");
    std::fs::write(&meta, Value::Object(m).to_string()).expect("write");
    run_under(
        TurnPolicy::unattended(),
        start("a task from the batch harness"),
    );
    assert!(
        changed(&logs).is_empty(),
        "no recorded policy is not a change"
    );
}

#[test]
fn a_preset_lists_its_gates_one_by_one() {
    // The coding product's expected shape, by changing the preset alone: the
    // named-deliverable gate on, every other gate off. The task names fix_summary.md and it
    // is never written, so finish is bounced once by that gate; the hollow-finish gate
    // (nothing delivered), the test guard and the pre-delivery re-read stay silent.
    let task = "Read inventory.py, write bug_report.md, fix the bugs, then write fix_summary.md.";
    let setup = || {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::write(d.path().join("inventory.py"), "x = 1").expect("w");
        std::fs::write(d.path().join("bug_report.md"), "# bugs").expect("w");
        std::fs::write(
            d.path().join("test_inventory.py"),
            "def test_x():\n    assert False\n",
        )
        .expect("w");
        d
    };
    let d = setup();
    let mut a = agent_with(d.path(), &[], "", "Task complete.");
    a.params.policy = TurnPolicy {
        gates: Gates::NONE.with(Gate::NamedFiles),
        ..TurnPolicy::conversation()
    };
    let out = a.run(start(task), None, None).expect("loop");
    let named = user_msgs(&out, "the task names these files");
    assert_eq!(named.len(), 1, "{out:#?}");
    assert!(named[0].content.contains("fix_summary.md"));
    assert!(user_msgs(&out, "nothing has been delivered").is_empty());
    assert!(user_msgs(&out, "tests").is_empty(), "the test guard is off");
    assert!(user_msgs(&out, "Pre-delivery check").is_empty());
    assert_eq!(
        out.iter()
            .filter(|m| m.tool_calls.iter().any(|t| t.function.name == "finish"))
            .count(),
        2,
        "bounced once, then accepted"
    );

    // The same run under the plain coding preset: no gate at all.
    let d = setup();
    let mut a = agent_with(d.path(), &[], "", "Task complete.");
    a.params.policy = TurnPolicy::conversation();
    let out = a.run(start(task), None, None).expect("loop");
    assert!(user_msgs(&out, "the task names these files").is_empty());
    assert_eq!(
        out.iter()
            .filter(|m| m.tool_calls.iter().any(|t| t.function.name == "finish"))
            .count(),
        1
    );
}

#[test]
fn interactive_question_keeps_the_task_prompt_and_file_tools() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = vec![(
        "create_file",
        json!({"path": "notes.md", "content": "x"}),
        "[created notes.md]",
    )];
    let mut a = agent_for(d.path(), &calls);
    a.params.policy = TurnPolicy::conversation();
    let out = a
        .run(start("What is the next item to work on ?"), None, None)
        .expect("loop");
    assert_eq!(out[0].content, "sys", "no swap to the chat prompt");
    assert!(
        !out.iter()
            .any(|m| m.role == Role::Tool && m.content.contains("asking a question")),
        "{out:#?}"
    );
    assert!(out
        .iter()
        .any(|m| m.role == Role::Tool && m.content == "[created notes.md]"));
}

#[test]
fn interactive_two_empty_replies_end_the_turn() {
    let d = tempfile::tempdir().expect("tmp");
    let mut params = RunParams::plain("gemma4:12b", d.path());
    params.policy = TurnPolicy::conversation();
    let mut a = Agent::new(
        params,
        Responder {
            n: 0,
            f: |_| ChatResponse::text(""),
        },
        OkTools,
    );
    let out = a.run(start("Say something"), None, None).expect("loop");
    assert!(user_msgs(&out, "context has been reset").is_empty());
    assert!(user_msgs(&out, "output empty text").is_empty());
    assert_eq!(out.iter().filter(|m| m.role == Role::Assistant).count(), 2);
}

// ---- harness files out of the project (boubou-7s3) ----

#[test]
fn a_relocated_harness_dir_takes_the_plan_and_the_session_snapshot_follows() {
    use rufous_kernel::exec::Executor;
    use rufous_kernel::safety::SafetyPolicy;
    let d = tempfile::tempdir().expect("tmp");
    let h = tempfile::tempdir().expect("tmp");
    let sdir = tempfile::tempdir().expect("tmp");
    let mut params = RunParams::plain("gemma4:12b", d.path());
    params.policy = TurnPolicy::conversation();
    params.harness_dir = Some(h.path().to_path_buf());
    let mut tools = Executor::with_policy(d.path(), SafetyPolicy::default());
    tools.harness_dir = h.path().to_path_buf();
    let mut a = Agent::new(
        params,
        Responder {
            n: 0,
            f: |n| match n {
                1 => ChatResponse::call(
                    "",
                    "todo",
                    json!({"action": "create", "items": ["write a.txt"]}),
                ),
                2 => {
                    ChatResponse::call("", "create_file", json!({"path": "a.txt", "content": "hi"}))
                }
                3 => ChatResponse::call(
                    "",
                    "edit_file",
                    json!({"path": "a.txt", "old": "hi", "new": "ho"}),
                ),
                _ => ChatResponse::text("Done."),
            },
        },
        tools,
    );
    a.sessions_dir = sdir.path().to_path_buf();
    // As the ACP runner does: the checkpoint goes beside the session files too.
    a.checkpoint = Some(sdir.path().join("s1.state.json"));
    a.run(start("write a.txt"), Some("s1"), None).expect("loop");
    assert!(!d.path().join("todo.json").exists(), "plan in the project");
    assert!(
        !d.path().join("a.txt.bak").exists(),
        "backup in the project"
    );
    assert!(h.path().join("todo.json").exists());
    assert!(h.path().join("backups/a.txt.bak").exists());
    assert!(
        sdir.path().join("s1.todo.json").exists(),
        "session snapshot of the plan"
    );
    let left: Vec<String> = std::fs::read_dir(d.path())
        .expect("ls")
        .flatten()
        .map(|x| x.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "m08.py")
        .collect();
    assert_eq!(left, vec!["a.txt".to_string()], "{left:?}");
}

// ---- image the server cannot decode (tier7 CH-03 unattended, 2026-10-30; bd boubou-am3) ----

/// Answers like `Scripted`, except that a request carrying an image is refused the way Ollama
/// refuses a corrupt PNG. Counts the refusals.
struct RejectsImages {
    inner: Scripted,
    refused: usize,
    /// The 400 body; Ollama's wording unless a test sets an OpenAI-compatible server's.
    body: &'static str,
}

const OLLAMA_IMAGE_400: &str = r#"{"error":"decode image: png: invalid format: invalid checksum"}"#;

impl ChatBackend for RejectsImages {
    fn chat(
        &mut self,
        model: &str,
        messages: &[Message],
        tools: &[Value],
        on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        if messages.iter().any(|m| !m.images.is_empty()) {
            self.refused += 1;
            return Err(ChatError::Status {
                status: 400,
                body: self.body.to_string(),
            });
        }
        self.inner.chat(model, messages, tools, on)
    }
}

#[test]
fn an_image_the_server_cannot_decode_is_dropped_and_the_run_goes_on() {
    image_refused_then_dropped(OLLAMA_IMAGE_400);
}

/// LM Studio / OpenAI wording for a model without vision (bd boubou-2cr).
#[test]
fn an_image_an_openai_server_refuses_is_dropped_and_the_run_goes_on() {
    image_refused_then_dropped(
        r#"{"error":{"message":"Invalid content type. image_url is only supported by certain models.","type":"invalid_request_error"}}"#,
    );
    image_refused_then_dropped(
        r#"{"error":"Model does not support images. Please use a model that does."}"#,
    );
}

fn image_refused_then_dropped(body: &'static str) {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = vec![(
        "read_file",
        json!({"path": "shot.png"}),
        "[image attached: shot.png (0 KB)]",
    )];
    // read_file queues the image for the next request the way the real tool does.
    std::fs::write(
        d.path().join(".hb_pending_images.json"),
        r#"[{"path":"shot.png","b64":"QUJD"}]"#,
    )
    .expect("w");
    let backend = RejectsImages {
        inner: Scripted {
            calls: calls.clone(),
            i: 0,
            finish_content: "done".to_string(),
            finish_summary: "Looked at the folder.".to_string(),
        },
        refused: 0,
        body,
    };
    let tools = ScriptedTools {
        calls,
        repeat: Vec::new(),
        served: Vec::new(),
    };
    let _ = std::fs::write(d.path().join("m08.py"), "def f():\n    return 1\n");
    let params = RunParams::plain("gemma4:12b", d.path());
    let mut a = Agent::new(params, backend, tools);
    a.sessions_dir = d.path().join("sessions");
    let out = a
        .run(
            start("Look at shot.png and tell me what it shows."),
            None,
            None,
        )
        .expect("the run must not die on a 400 the harness caused");
    assert_eq!(
        a.backend.refused, 1,
        "refused once, then the retry carried no image"
    );
    assert!(out.iter().all(|m| m.images.is_empty()));
    let noted: Vec<&Message> = out
        .iter()
        .filter(|m| {
            m.content
                .contains("image could not be decoded by the model server")
        })
        .collect();
    assert_eq!(noted.len(), 1, "{out:#?}");
    // The image hangs on the last tool or user message before the request (a harness note
    // may follow the tool output), so the note is on whichever carried it.
    assert!(
        matches!(noted[0].role, Role::Tool | Role::User),
        "{:?}",
        noted[0].role
    );
    assert_eq!(out.last().map(|m| &m.role), Some(&Role::Assistant));
}

// Bead boubou-h5d: the ⚙ log line for a call whose result spans lines (the editor's
// "closest match" excerpt after a failed edit) is still one line, and carries its tag.

#[test]
fn the_tool_log_line_stays_one_line_with_its_tag_after_a_multi_line_result() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = vec![(
        "edit_file",
        json!({"path": "m08.py", "old": "zzz", "new": "y"}),
        "[edit failed: old text not found. Closest match in m08.py, lines 1–2:\ndef f():\n    return 1",
    )];
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = lines.clone();
    let mut a = agent_for(d.path(), &calls).with_sink(move |e| {
        if let AgentEvent::Log(l) = e {
            seen.lock().expect("lock").push(l.clone());
        }
    });
    a.run(start(TASK), None, None).expect("loop");
    let lines = lines.lock().expect("lock").clone();
    let gear: Vec<&String> = lines.iter().filter(|l| l.contains("⚙ edit_file")).collect();
    assert_eq!(gear.len(), 1, "{lines:?}");
    let line = gear[0];
    assert!(!line.contains('\n'), "{line}");
    assert!(line.ends_with(" ⟂error"), "{line}");
    assert!(line.contains("lines 1–2: ⏎ def f(): ⏎ "), "{line}");
    assert!(
        lines.iter().all(|l| !l.contains('\n') || !l.contains("⟂")),
        "a tag on a continuation line: {lines:?}"
    );
}

// Gate 3c (bead boubou-78s): under a coding-shaped policy, a text reply that ends the turn
// is held once when the instruction's file got no write; the next reply, after the write,
// ends the turn. A turn that asks for no write, or reports why it could not write, is
// not held.

fn coding_like() -> TurnPolicy {
    TurnPolicy {
        name: "coding-test",
        gates: Gates::NONE.with(Gate::NamedWrites),
        ..TurnPolicy::conversation()
    }
}

fn run_coding_like<F: FnMut(usize) -> ChatResponse>(
    d: &Path,
    task: &str,
    tools: &[Call],
    f: F,
) -> Vec<Message> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut params = RunParams::plain("gemma4:12b", d);
    params.policy = coding_like();
    let mut a = Agent::new(
        params,
        Lens { n: 0, f, seen },
        ScriptedTools {
            calls: tools.to_vec(),
            repeat: Vec::new(),
            served: Vec::new(),
        },
    );
    a.sessions_dir = d.join("sessions");
    a.run(start(task), None, None).expect("loop")
}

#[test]
fn a_text_reply_is_held_once_until_the_named_file_is_written() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("tasks.md"), "# Tasks\n- [ ] one\n").expect("fixture");
    let tools: Vec<Call> = vec![
        (
            "todo",
            json!({"action": "update", "index": 1, "done": true}),
            "[x] 1. one",
        ),
        (
            "append_file",
            json!({"path": "tasks.md", "content": "- [x] one\n"}),
            "[appended tasks.md]",
        ),
    ];
    let msgs = run_coding_like(
        d.path(),
        "Tick the first task in tasks.md as done. Do not start the second one.",
        &tools,
        |n| match n {
            1 => ChatResponse::call(
                "",
                "todo",
                json!({"action": "update", "index": 1, "done": true}),
            ),
            2 => ChatResponse::text("Ticked the first task in tasks.md."),
            3 => ChatResponse::call(
                "",
                "append_file",
                json!({"path": "tasks.md", "content": "- [x] one\n"}),
            ),
            _ => ChatResponse::text("Done: the first task is ticked in tasks.md."),
        },
    );
    let held = user_msgs(&msgs, "nothing was written to them this turn");
    assert_eq!(held.len(), 1, "{msgs:#?}");
    assert!(
        held[0].content.contains("- tasks.md"),
        "{}",
        held[0].content
    );
    assert!(held[0]
        .content
        .contains("todo plan is the harness's own list"));
    assert!(
        msgs.last().is_some_and(|m| m.content.starts_with("Done:")),
        "{msgs:#?}"
    );
}

#[test]
fn a_reply_that_asks_for_no_write_or_reports_the_failure_is_not_held() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("tasks.md"), "# Tasks\n- [ ] one\n").expect("fixture");
    // No write asked: the answer is the turn.
    let msgs = run_coding_like(
        d.path(),
        "Which task should we do first, and why? Just tell me; do not change any file.",
        &[],
        |_| ChatResponse::text("The first one: everything else needs it."),
    );
    assert!(
        user_msgs(&msgs, "nothing was written").is_empty(),
        "{msgs:#?}"
    );
    assert_eq!(msgs.len(), 3, "{msgs:#?}");
    // A write asked and declined: the report ends the turn.
    let msgs = run_coding_like(
        d.path(),
        "Write the single word hello to /tmp/boubou_convo_perm.txt.",
        &[],
        |_| {
            ChatResponse::text(
                "I was unable to write /tmp/boubou_convo_perm.txt: the user declined it.",
            )
        },
    );
    assert!(
        user_msgs(&msgs, "nothing was written").is_empty(),
        "{msgs:#?}"
    );
    // Held once at most: a model that answers the hold with the same text still ends the turn.
    let msgs = run_coding_like(
        d.path(),
        "Tick the first task in tasks.md as done.",
        &[],
        |_| ChatResponse::text("Ticked."),
    );
    assert_eq!(
        user_msgs(&msgs, "nothing was written").len(),
        1,
        "{msgs:#?}"
    );
}

// Bead boubou-alj arm A: the model is sent the ledger line, the history kept is not changed,
// and the ablation switch sends the history as it is.
#[test]
fn an_earlier_refusal_reply_is_sent_as_a_ledger_line_and_kept_as_said() {
    let decline = "[safety gate: the user declined this call: create_file on /tmp/perm.txt. Do not do it another way or write it somewhere else; say what you could not do. If the user asks for it again, make the same call again: they are asked each time and may allow it.]";
    let history = || {
        vec![
            Message::system("sys"),
            Message::user("Write the single word hello to /tmp/perm.txt."),
            Message::assistant_call(
                "create_file",
                json!({"path": "/tmp/perm.txt", "content": "hello"}),
            ),
            Message::tool_output(decline),
            Message::assistant("I cannot write to /tmp due to security restrictions."),
            Message::user("Try again, please: write the single word hello to /tmp/perm.txt."),
        ]
    };
    for ablated in [false, true] {
        let d = tempfile::tempdir().expect("tmp");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut params = RunParams::plain("gemma4:12b", d.path());
        if ablated {
            params.ablation = vec!["reattribute".to_string()];
        }
        let mut a = Agent::new(
            params,
            Lens {
                n: 0,
                f: |_| ChatResponse::text("Done."),
                seen: seen.clone(),
            },
            ScriptedTools {
                calls: Vec::new(),
                repeat: Vec::new(),
                served: Vec::new(),
            },
        );
        a.sessions_dir = d.path().join("sessions");
        let out = a.run(history(), None, None).expect("loop");
        let first = seen.lock().expect("lock")[0].1.clone();
        assert_eq!(
            first.contains("[ledger] turn 1: create_file /tmp/perm.txt was declined by the user"),
            !ablated
        );
        assert_eq!(first.contains("security restrictions"), ablated);
        assert!(out
            .iter()
            .any(|m| m.content == "I cannot write to /tmp due to security restrictions."));
        assert!(!out.iter().any(|m| m.content.starts_with("[ledger]")));
    }
}

/// Answers like `inner`, except that call number `fail_at` fails with `err`.
struct FailsOnce {
    inner: Scripted,
    fail_at: usize,
    err: ChatError,
    calls: usize,
}

impl ChatBackend for FailsOnce {
    fn chat(
        &mut self,
        model: &str,
        messages: &[Message],
        tools: &[Value],
        on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        self.calls += 1;
        if self.calls == self.fail_at {
            return Err(self.err.clone());
        }
        self.inner.chat(model, messages, tools, on)
    }
}

fn fails_once(
    d: &Path,
    err: ChatError,
) -> (Agent<FailsOnce, ScriptedTools>, Arc<Mutex<Vec<AgentEvent>>>) {
    let calls: Vec<Call> = vec![("list_dir", json!({"path": "."}), "a.txt")];
    let backend = FailsOnce {
        inner: Scripted {
            calls: calls.clone(),
            i: 0,
            finish_content: "done".to_string(),
            finish_summary: "Listed the folder.".to_string(),
        },
        fail_at: 2,
        err,
        calls: 0,
    };
    let tools = ScriptedTools {
        calls,
        repeat: Vec::new(),
        served: Vec::new(),
    };
    let mut params = RunParams::plain("slow:27b", d);
    params.sleep_scale = 0.0;
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let sink = Arc::clone(&events);
    let mut a =
        Agent::new(params, backend, tools).with_sink(move |e| sink.lock().expect("lock").push(e));
    a.sessions_dir = d.join("sessions");
    (a, events)
}

#[test]
fn a_reply_timeout_ends_the_turn_with_a_message_and_is_not_resent() {
    // Bead boubou-yqu: bonsai-2:27b was sent the same 5,898-token request five times, each
    // time writing for 4–6 minutes past the read timeout.
    let d = tempfile::tempdir().expect("tmp");
    let (mut a, events) = fails_once(d.path(), ChatError::Timeout { mid_reply: true });
    let out = a
        .run(start("List the folder."), None, None)
        .expect("a timeout ends the turn, it is not a failed run");
    assert_eq!(
        a.backend.calls, 2,
        "the timed-out request was not sent again"
    );
    assert!(
        out.iter()
            .any(|m| m.role == Role::Tool && m.content.contains("a.txt")),
        "the work before the timeout is kept: {out:#?}"
    );
    let ev = events.lock().expect("lock");
    assert!(ev
        .iter()
        .any(|e| matches!(e, AgentEvent::Error(t) if t.contains("slow:27b"))));
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::Text(t)
        if t.contains("⏱ Stopped: slow:27b") && t.contains("partway through its reply")
            && t.contains("AGENT_APITIMEOUT"))));
    assert!(!ev.iter().any(|e| matches!(e, AgentEvent::Finished { .. })));
}

#[test]
fn a_transport_error_that_is_not_a_timeout_is_still_retried() {
    let d = tempfile::tempdir().expect("tmp");
    let (mut a, events) = fails_once(
        d.path(),
        ChatError::Transport("error sending request: connection refused".into()),
    );
    a.run(start("List the folder."), None, None).expect("loop");
    assert!(
        a.backend.calls >= 3,
        "retried after the failure: {}",
        a.backend.calls
    );
    let ev = events.lock().expect("lock");
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::Finished { .. })));
}

#[test]
fn an_unreachable_engine_that_is_not_ours_ends_the_turn_at_once() {
    // --no-start-engine (boubou-f8d): the host owns the engine, so a refused connection is
    // not ridden out with minutes of back-off.
    let d = tempfile::tempdir().expect("tmp");
    let (mut a, _) = fails_once(
        d.path(),
        ChatError::Unreachable("Connection refused (os error 61)".into()),
    );
    a.params.engine_unmanaged = true;
    let err = a
        .run(start("List the folder."), None, None)
        .expect_err("the turn fails");
    assert_eq!(a.backend.calls, 2, "not retried");
    let text = err.to_string();
    assert!(
        text.contains("not reachable (Connection refused") && text.contains("--no-start-engine"),
        "{text}"
    );
}

#[test]
fn an_unreachable_engine_rufous_manages_is_retried() {
    let d = tempfile::tempdir().expect("tmp");
    let (mut a, events) = fails_once(
        d.path(),
        ChatError::Unreachable("Connection refused (os error 61)".into()),
    );
    a.run(start("List the folder."), None, None).expect("loop");
    assert!(a.backend.calls >= 3, "retried: {}", a.backend.calls);
    let ev = events.lock().expect("lock");
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::Finished { .. })));
}

/// Scripted tools whose `web_fetch` reports a contacted host (docs/egress.md).
struct Reaching(ScriptedTools);

impl ToolRunner for Reaching {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        self.0.run_tool(name, args)
    }
    fn take_egress(&mut self) -> Vec<rufous_kernel::egress::Egress> {
        rufous_kernel::egress::Egress::of("https://example.org/a", "web_fetch")
            .into_iter()
            .filter(|_| {
                self.0.served.last()
                    == Some(&json!(["web_fetch", {"url": "https://example.org/a"}]))
            })
            .collect()
    }
}

#[test]
fn a_call_s_egress_is_reported_before_its_result_and_logged() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = vec![(
        "web_fetch",
        json!({"url": "https://example.org/a"}),
        "Example page",
    )];
    let backend = Scripted {
        calls: calls.clone(),
        i: 0,
        finish_content: "done".to_string(),
        finish_summary: "Fetched the page.".to_string(),
    };
    let tools = Reaching(ScriptedTools {
        calls,
        repeat: Vec::new(),
        served: Vec::new(),
    });
    let mut params = RunParams::plain("m", d.path());
    params.sleep_scale = 0.0;
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let sink = Arc::clone(&events);
    let mut a =
        Agent::new(params, backend, tools).with_sink(move |e| sink.lock().expect("lock").push(e));
    a.sessions_dir = d.path().join("sessions");
    a.run(start("Fetch https://example.org/a"), None, None)
        .expect("loop");
    let ev = events.lock().expect("lock");
    let at = ev
        .iter()
        .position(|e| matches!(e, AgentEvent::Egress(l) if l[0].host == "example.org"))
        .expect("egress event");
    assert!(
        matches!(&ev[at + 1], AgentEvent::ToolResult { name, .. } if name == "web_fetch"),
        "{:?}",
        ev[at + 1]
    );
    assert!(ev.iter().any(
        |e| matches!(e, AgentEvent::Log(l) if l.ends_with("[egress] example.org (web_fetch)"))
    ));
}

#[test]
fn the_timeout_note_says_where_the_model_stalled() {
    use rufous_kernel::agent::run::reply_timeout_note;
    let before = reply_timeout_note("m", 240, false, 23000);
    assert!(before.contains("before its first token") && before.contains("~23000 tokens"));
    let mid = reply_timeout_note("m", 240, true, 23000);
    assert!(mid.contains("smaller pieces") && !mid.contains("~23000"));
}

// Plan hold (bead boubou-ear): Gate 5 at the text ending. A reply that ends the turn while
// the plan this turn made still has open items is held, at most twice, the second time only
// after progress; a plan the turn did not change, a failure report and the ablation never
// hold. The scripted model writes the plan file itself, standing in for the todo tool.

fn plan_file(d: &Path, items: &[(&str, bool)]) {
    let v: Vec<Value> = items
        .iter()
        .map(|(i, done)| json!({"item": i, "done": done}))
        .collect();
    std::fs::write(d.join("todo.json"), Value::from(v).to_string()).expect("plan");
}

fn run_plan_hold<F: FnMut(usize) -> ChatResponse>(
    d: &Path,
    ablation: &[&str],
    f: F,
) -> (Vec<Message>, Vec<String>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink_logs = logs.clone();
    let mut params = RunParams::plain("gemma4:12b", d);
    params.policy = TurnPolicy {
        name: "coding-test",
        gates: Gates::NONE.with(Gate::NamedWrites).with(Gate::PlanSync),
        ..TurnPolicy::conversation()
    };
    params.ablation = ablation.iter().map(|s| s.to_string()).collect();
    let tools: Vec<Call> = vec![
        (
            "todo",
            json!({"action": "create", "items": ["a", "b"]}),
            "[ ] 1. a\n[ ] 2. b",
        ),
        (
            "todo",
            json!({"action": "update", "index": 1}),
            "[x] 1. a\n[ ] 2. b",
        ),
        (
            "todo",
            json!({"action": "update", "index": 2}),
            "[x] 1. a\n[x] 2. b",
        ),
        (
            "create_file",
            json!({"path": "a.txt", "content": "a"}),
            "[created a.txt]",
        ),
    ];
    let mut a = Agent::new(
        params,
        Lens { n: 0, f, seen },
        ScriptedTools {
            calls: tools,
            repeat: Vec::new(),
            served: Vec::new(),
        },
    )
    .with_sink(move |ev| {
        if let AgentEvent::Log(l) = ev {
            sink_logs.lock().expect("lock").push(l);
        }
    });
    a.sessions_dir = d.join("sessions");
    let msgs = a
        .run(start("Implement a and b."), None, None)
        .expect("loop");
    let logs = logs.lock().expect("lock").clone();
    (msgs, logs)
}

fn holds(msgs: &[Message]) -> Vec<&Message> {
    user_msgs(msgs, "Your plan for this task still has")
}

#[test]
fn a_reply_with_this_turns_plan_open_is_held_until_the_plan_is_done() {
    let d = tempfile::tempdir().expect("tmp");
    let p = d.path().to_path_buf();
    let (msgs, logs) = run_plan_hold(d.path(), &[], |n| match n {
        1 => {
            plan_file(&p, &[("a", false), ("b", false)]);
            ChatResponse::call("", "todo", json!({"action": "create", "items": ["a", "b"]}))
        }
        2 => ChatResponse::text("I'll start with a."),
        3 => ChatResponse::call("", "create_file", json!({"path": "a.txt", "content": "a"})),
        4 => {
            plan_file(&p, &[("a", true), ("b", false)]);
            ChatResponse::call("", "todo", json!({"action": "update", "index": 1}))
        }
        5 => ChatResponse::text("a is done. Shall we move on to the next bead?"),
        6 => {
            plan_file(&p, &[("a", true), ("b", true)]);
            ChatResponse::call("", "todo", json!({"action": "update", "index": 2}))
        }
        _ => ChatResponse::text("Both are done."),
    });
    let h = holds(&msgs);
    assert_eq!(h.len(), 2, "{logs:#?}");
    assert!(
        h[0].content.contains("(0/2): 1. a; 2. b."),
        "{}",
        h[0].content
    );
    assert!(h[0]
        .content
        .contains("do not end the turn or start another task"));
    assert!(
        !h[0].content.contains("all=true"),
        "no sync-everything shortcut"
    );
    assert!(h[1].content.contains("(1/2): 2. b."), "{}", h[1].content);
    assert_eq!(
        msgs.last().map(|m| m.content.as_str()),
        Some("Both are done.")
    );
    assert_eq!(
        logs.iter().filter(|l| l.contains("⚠️ plan hold (")).count(),
        2,
        "{logs:#?}"
    );
}

#[test]
fn a_hold_with_no_progress_since_the_last_one_releases_the_turn() {
    let d = tempfile::tempdir().expect("tmp");
    let p = d.path().to_path_buf();
    let (msgs, logs) = run_plan_hold(d.path(), &[], |n| match n {
        1 => {
            plan_file(&p, &[("a", false), ("b", false)]);
            ChatResponse::call("", "todo", json!({"action": "create", "items": ["a", "b"]}))
        }
        2 => ChatResponse::text("Planned."),
        _ => ChatResponse::text("Planned, as I said."),
    });
    assert_eq!(holds(&msgs).len(), 1);
    assert!(
        logs.iter()
            .any(|l| l.contains("plan hold released: no progress since the last hold (0/2)")),
        "{logs:#?}"
    );
    assert_eq!(
        msgs.last().map(|m| m.content.as_str()),
        Some("Planned, as I said.")
    );
}

#[test]
fn a_plan_the_turn_did_not_change_a_failure_report_or_the_ablation_never_hold() {
    // A plan left open by an earlier turn: this turn answers a question.
    let d = tempfile::tempdir().expect("tmp");
    plan_file(d.path(), &[("a", true), ("b", false)]);
    let (msgs, _) = run_plan_hold(d.path(), &[], |_| ChatResponse::text("It is a todo app."));
    assert!(holds(&msgs).is_empty());

    // An honest failure report ends the turn with the plan as it stands.
    let d = tempfile::tempdir().expect("tmp");
    let p = d.path().to_path_buf();
    let (msgs, logs) = run_plan_hold(d.path(), &[], |n| match n {
        1 => {
            plan_file(&p, &[("a", false), ("b", false)]);
            ChatResponse::call("", "todo", json!({"action": "create", "items": ["a", "b"]}))
        }
        _ => ChatResponse::text("I could not build a: the compiler is missing."),
    });
    assert!(holds(&msgs).is_empty());
    assert!(
        logs.iter()
            .any(|l| l.contains("waived: the reply reports a failure")),
        "{logs:#?}"
    );

    // Ablated.
    let d = tempfile::tempdir().expect("tmp");
    let p = d.path().to_path_buf();
    let (msgs, _) = run_plan_hold(d.path(), &["plan_hold"], |n| match n {
        1 => {
            plan_file(&p, &[("a", false), ("b", false)]);
            ChatResponse::call("", "todo", json!({"action": "create", "items": ["a", "b"]}))
        }
        _ => ChatResponse::text("Planned."),
    });
    assert!(holds(&msgs).is_empty());
}

#[test]
fn a_todo_update_that_changes_nothing_does_not_reset_the_stale_plan_count() {
    // bead boubou-uzl: re-ticking a done item satisfied the stale-plan counter.
    let d = tempfile::tempdir().expect("tmp");
    let p = d.path().to_path_buf();
    let (msgs, _) = run_plan_hold(d.path(), &["plan_hold"], |n| match n {
        1 => {
            plan_file(&p, &[("a", true), ("b", false)]);
            ChatResponse::call("", "todo", json!({"action": "update", "index": 1}))
        }
        2 | 3 | 5 | 6 => ChatResponse::call(
            "",
            "create_file",
            json!({"path": "a.txt", "content": "a".repeat(n)}),
        ),
        // The same plan written again: a no-op update.
        4 => {
            plan_file(&p, &[("a", true), ("b", false)]);
            ChatResponse::call("", "todo", json!({"action": "update", "index": 1}))
        }
        _ => ChatResponse::text("Done."),
    });
    assert_eq!(
        user_msgs(&msgs, "Plan progress reminder").len(),
        1,
        "four producing calls with only a no-op update between them"
    );
}

#[test]
fn plan_sync_check_passes_holds_and_waives() {
    use rufous_kernel::agent::run::{plan_hold_note, plan_sync_check, PlanCheck};
    use rufous_kernel::guards::integrity::CommandRecord;
    use rufous_kernel::todo::TodoItem;
    let item = |s: &str, done: bool| TodoItem {
        item: s.to_string(),
        done,
    };
    let open = vec![item("a", true), item("b", false), item("c", false)];
    let done = vec![item("a", true)];
    assert_eq!(plan_sync_check(&[], "ok", &[]), PlanCheck::Pass);
    assert_eq!(plan_sync_check(&done, "ok", &[]), PlanCheck::Pass);
    assert_eq!(plan_sync_check(&open, "All done.", &[]), PlanCheck::Hold);
    let passed = [CommandRecord::from_result(
        "pytest -q",
        "[exit 0]\n3 passed",
    )];
    assert!(matches!(
        plan_sync_check(&open, "All done.", &passed),
        PlanCheck::Waived(_)
    ));
    // A build is not a check: no waiver.
    let built = [CommandRecord::from_result(
        "npm run build",
        "[exit 0]\nbuilt",
    )];
    assert_eq!(plan_sync_check(&open, "All done.", &built), PlanCheck::Hold);
    let note = plan_hold_note(&open);
    assert!(note.contains("2 open item(s) (1/3): 2. b; 3. c."), "{note}");
    assert!(note.contains("remove it with a one-line reason if it goes beyond the instruction"));
}

// Circling ladder (bead boubou-m5d): past 60 calls a hypothesis note, past 80 a checkpoint;
// after it, tool calls are refused, and after two refusals the harness's own report ends the
// turn. A text reply after the checkpoint ends the turn with no hold. Off for unattended
// runs and under the ablation.

struct FailingShell;

impl ToolRunner for FailingShell {
    fn run_tool(&mut self, name: &str, _args: &Value) -> String {
        if name == "run_bash" {
            "[exit 1]\nSTDERR:\nerror: build failed".to_string()
        } else if name == "create_file" {
            "[created a.txt]".to_string()
        } else {
            "[tool error: not scripted]".to_string()
        }
    }
}

fn run_circling<F: FnMut(usize) -> ChatResponse>(
    d: &Path,
    policy: TurnPolicy,
    ablation: &[&str],
    f: F,
) -> (Vec<Message>, Vec<String>, String) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let logs = Arc::new(Mutex::new(Vec::new()));
    let text = Arc::new(Mutex::new(String::new()));
    let (sink_logs, sink_text) = (logs.clone(), text.clone());
    let mut params = RunParams::plain("gemma4:12b", d);
    params.policy = policy;
    params.ablation = ["anti_loop", "corrector"]
        .iter()
        .chain(ablation)
        .map(|s| s.to_string())
        .collect();
    let mut a =
        Agent::new(params, Lens { n: 0, f, seen }, FailingShell).with_sink(move |ev| match ev {
            AgentEvent::Log(l) => sink_logs.lock().expect("lock").push(l),
            AgentEvent::Text(t) => sink_text.lock().expect("lock").push_str(&t),
            _ => {}
        });
    a.sessions_dir = d.join("sessions");
    let msgs = a
        .run(start("Make the app build."), None, None)
        .expect("loop");
    let logs = logs.lock().expect("lock").clone();
    let text = text.lock().expect("lock").clone();
    (msgs, logs, text)
}

fn varied_build(n: usize) -> ChatResponse {
    ChatResponse::call(
        "",
        "run_bash",
        json!({"command": format!("./gradlew clean build --attempt {n}")}),
    )
}

#[test]
fn a_turn_that_keeps_failing_is_asked_for_a_hypothesis_then_stopped_with_a_report() {
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, logs, text) = run_circling(d.path(), TurnPolicy::conversation(), &[], varied_build);
    let notes = |needle: &str| user_msgs(&msgs, needle).len();
    assert_eq!(notes("Before the next call, write in one or two lines"), 1);
    assert_eq!(notes("⚠️ Stop here ("), 1);
    assert!(
        logs.iter().any(|l| l
            .contains("[circling] stage 1: `./gradlew clean` failed with exit 1 8 times in a row")),
        "{logs:#?}"
    );
    assert!(
        logs.iter().any(|l| l.contains(
            "[circling] checkpoint: `./gradlew clean` failed with exit 1 12 times in a row"
        )),
        "{logs:#?}"
    );
    let refused = msgs
        .iter()
        .filter(|m| m.content.starts_with("[checkpoint: report to the user"))
        .count();
    assert_eq!(refused, 2);
    let last = msgs.last().expect("last");
    assert_eq!(last.role, Role::Assistant);
    assert!(
        last.content.starts_with("Stopped after 12 tool calls"),
        "{}",
        last.content
    );
    assert!(
        last.content.contains(
            "`./gradlew clean` exited 1 12 times in a row (last output: error: build failed)"
        ),
        "{}",
        last.content
    );
    assert!(
        text.contains("Stopped after 12 tool calls"),
        "the user sees the report"
    );
}

#[test]
fn a_text_reply_after_the_checkpoint_ends_the_turn_without_a_hold() {
    let d = tempfile::tempdir().expect("tmp");
    let p = d.path().to_path_buf();
    let policy = TurnPolicy {
        name: "coding-test",
        gates: Gates::NONE.with(Gate::NamedWrites).with(Gate::PlanSync),
        ..TurnPolicy::conversation()
    };
    let (msgs, logs, _) = run_circling(d.path(), policy, &[], |n| match n {
        1 => {
            plan_file(&p, &[("build", false)]);
            ChatResponse::call("", "todo", json!({"action": "create", "items": ["build"]}))
        }
        2..=13 => varied_build(n),
        _ => {
            ChatResponse::text("Tried 12 builds; the compiler cannot find module x. Unknown: why.")
        }
    });
    assert!(
        holds(&msgs).is_empty(),
        "the plan hold does not hold a checkpoint report"
    );
    assert!(
        logs.iter()
            .any(|l| l.contains("[circling] the report ends the turn")),
        "{logs:#?}"
    );
    assert_eq!(
        msgs.last().map(|m| m.content.as_str()),
        Some("Tried 12 builds; the compiler cannot find module x. Unknown: why.")
    );
}

#[test]
fn unattended_runs_and_the_ablation_never_circle_stop() {
    for (policy, ablation) in [
        (TurnPolicy::unattended(), vec![]),
        (TurnPolicy::conversation(), vec!["checkpoint"]),
    ] {
        let d = tempfile::tempdir().expect("tmp");
        let (msgs, logs, _) = run_circling(d.path(), policy, &ablation, |n| {
            if n <= 15 {
                varied_build(n)
            } else {
                ChatResponse::text("Gave up: the build keeps failing.")
            }
        });
        assert!(user_msgs(&msgs, "⚠️ Stop here (").is_empty());
        assert!(!logs.iter().any(|l| l.contains("[circling]")), "{logs:#?}");
    }
}

struct DenyingGate;

impl ToolRunner for DenyingGate {
    fn run_tool(&mut self, _name: &str, _args: &Value) -> String {
        "[scope gate: the instruction was: \"Make the app build.\"; this call writes code, which it did not ask for. Report what is done and stop; the user will say what comes next.]".to_string()
    }
}

#[test]
fn a_scope_gate_deny_goes_straight_to_the_checkpoint() {
    let d = tempfile::tempdir().expect("tmp");
    let mut params = RunParams::plain("gemma4:12b", d.path());
    params.policy = TurnPolicy::conversation();
    params.ablation = vec!["anti_loop".into(), "corrector".into()];
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink_logs = logs.clone();
    let backend = Lens {
        n: 0,
        f: |n| {
            if n == 1 {
                ChatResponse::call("", "create_file", json!({"path": "a.txt", "content": "x"}))
            } else {
                ChatResponse::text("The gate refused the write; nothing was changed.")
            }
        },
        seen: Arc::new(Mutex::new(Vec::new())),
    };
    let mut a = Agent::new(params, backend, DenyingGate).with_sink(move |ev| {
        if let AgentEvent::Log(l) = ev {
            sink_logs.lock().expect("lock").push(l);
        }
    });
    a.sessions_dir = d.path().join("sessions");
    let msgs = a
        .run(start("Make the app build."), None, None)
        .expect("loop");
    let logs = logs.lock().expect("lock").clone();
    assert!(
        logs.iter()
            .any(|l| l.contains("[circling] checkpoint: the instruction did not ask for that call")),
        "{logs:#?}"
    );
    assert!(logs
        .iter()
        .any(|l| l.contains("[circling] the report ends the turn")));
    assert_eq!(
        msgs.last().map(|m| m.content.as_str()),
        Some("The gate refused the write; nothing was changed.")
    );
}

// Gemma 4 native call leaked as text (bead boubou-5hg): salvaged and run, not the turn's end.
#[test]
fn a_gemma_native_call_leaked_as_text_runs_instead_of_ending_the_turn() {
    let d = tempfile::tempdir().expect("tmp");
    let (msgs, logs, _) = run_circling(d.path(), TurnPolicy::conversation(), &[], |n| match n {
        1 => ChatResponse::call("", "todo", json!({"action": "create", "items": ["build"]})),
        2 => ChatResponse::text(
            r#"<|channel>::<|channel>call:todo{action:<|"|>update<|"|>,done:true,index:1}<tool_call|>"#,
        ),
        _ => ChatResponse::text("Built; the plan is done."),
    });
    assert!(
        logs.iter()
            .any(|l| l.contains("⚡ salvaged tool call(s) from text (gemma native): todo")),
        "{logs:#?}"
    );
    assert_eq!(
        msgs.last().map(|m| m.content.as_str()),
        Some("Built; the plan is done.")
    );
}

// Claims hold (bead boubou-bcs): a conversation turn that wrote code and then says it works,
// with nothing run after the last write to show it, is held once; a clean run, a hedge or a
// second unbacked claim (released) ends it. At the finish of a conversation turn too.

struct ScriptShell(Vec<(&'static str, &'static str)>);

impl ToolRunner for ScriptShell {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        match name {
            "run_bash" => {
                let cmd = args["command"].as_str().unwrap_or("");
                self.0
                    .iter()
                    .find(|(k, _)| cmd.contains(k))
                    .map_or("[exit 0]\nSTDOUT:\n", |(_, out)| out)
                    .to_string()
            }
            "create_file" | "edit_file" => format!("[written {}]", args["path"]),
            _ => "[tool error: not scripted]".to_string(),
        }
    }
}

fn claims_policy() -> TurnPolicy {
    TurnPolicy {
        name: "coding-test",
        gates: Gates::NONE.with(Gate::Claims),
        ..TurnPolicy::conversation()
    }
}

fn run_claims<F: FnMut(usize) -> ChatResponse>(
    policy: TurnPolicy,
    ablation: &[&str],
    shell: Vec<(&'static str, &'static str)>,
    task: &str,
    f: F,
) -> (Vec<Message>, Vec<String>) {
    let d = tempfile::tempdir().expect("tmp");
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink_logs = logs.clone();
    let mut params = RunParams::plain("gemma4:12b", d.path());
    params.policy = policy;
    params.ablation = ["anti_loop", "corrector"]
        .iter()
        .chain(ablation)
        .map(|s| s.to_string())
        .collect();
    let backend = Lens {
        n: 0,
        f,
        seen: Arc::new(Mutex::new(Vec::new())),
    };
    let mut a = Agent::new(params, backend, ScriptShell(shell)).with_sink(move |ev| {
        if let AgentEvent::Log(l) = ev {
            sink_logs.lock().expect("lock").push(l);
        }
    });
    a.sessions_dir = d.path().join("sessions");
    let msgs = a.run(start(task), None, None).expect("loop");
    let logs = logs.lock().expect("lock").clone();
    (msgs, logs)
}

fn edit_app() -> ChatResponse {
    ChatResponse::call(
        "",
        "edit_file",
        json!({"path": "src/App.vue", "old": "a", "new": "b"}),
    )
}

const HELD: &str = "⚠️ Your reply was held: it says";

#[test]
fn an_unbacked_fix_claim_is_held_once_and_a_clean_run_then_backs_it() {
    let (msgs, logs) = run_claims(
        claims_policy(),
        &[],
        vec![("npm run build", "[exit 0]\nSTDOUT:\n✓ built in 1.2s\n")],
        "Fix the blank page.",
        |n| match n {
            1 => edit_app(),
            2 => ChatResponse::text("Fixed the blank page; it now loads."),
            3 => ChatResponse::call(
                "",
                "run_bash",
                json!({"command": "cd frontend && npm run build"}),
            ),
            _ => ChatResponse::text("Fixed the blank page; the build is clean."),
        },
    );
    let held = user_msgs(&msgs, HELD);
    assert_eq!(held.len(), 1);
    assert!(
        held[0].content.contains(
            "\"Fixed the\", but nothing ran after your last write (`edit_file src/App.vue`)"
        ),
        "{}",
        held[0].content
    );
    assert!(
        logs.iter()
            .any(|l| l.contains("[claims] hold: \"Fixed the\"")),
        "{logs:#?}"
    );
    assert!(logs
        .iter()
        .any(|l| l.contains("[claims] \"Fixed the\" backed by `npm run build`")));
    assert_eq!(
        msgs.last().map(|m| m.content.as_str()),
        Some("Fixed the blank page; the build is clean.")
    );
}

#[test]
fn a_run_that_prints_a_failure_does_not_back_and_a_second_claim_is_released() {
    let (msgs, logs) = run_claims(
        claims_policy(),
        &[],
        vec![("curl", "[exit 0]\nSTDOUT:\n{\"message\":\"Not Found\"}\n")],
        "The home page is a 404.",
        |n| match n {
            1 => edit_app(),
            2 => ChatResponse::call(
                "",
                "run_bash",
                json!({"command": "curl -s http://localhost:8080/ | head"}),
            ),
            3 => ChatResponse::text("Fixed the 404 error for static resources."),
            _ => ChatResponse::text("Fixed the 404 error for static resources, as I said."),
        },
    );
    let held = user_msgs(&msgs, HELD);
    assert_eq!(held.len(), 1);
    assert!(
        held[0]
            .content
            .contains("exited 0 but printed \"{\"message\":\"Not Found\"}\""),
        "{}",
        held[0].content
    );
    assert!(
        logs.iter()
            .any(|l| l.contains("[claims] released: \"Fixed the\" still unbacked")),
        "{logs:#?}"
    );
}

#[test]
fn a_reworded_reply_after_the_hold_ends_the_turn() {
    let (msgs, logs) = run_claims(
        claims_policy(),
        &[],
        vec![],
        "Add the toggle.",
        |n| match n {
            1 => edit_app(),
            2 => ChatResponse::text("I added the toggle and verified that it updates the store."),
            _ => {
                ChatResponse::text("I added the toggle to App.vue; it is written but not yet run.")
            }
        },
    );
    assert_eq!(user_msgs(&msgs, HELD).len(), 1);
    assert!(logs
        .iter()
        .any(|l| l.contains("[claims] hold: \"verified that\"")));
    assert!(!logs.iter().any(|l| l.contains("[claims] released")));
    assert_eq!(
        msgs.last().map(|m| m.content.as_str()),
        Some("I added the toggle to App.vue; it is written but not yet run.")
    );
}

#[test]
fn a_finish_that_claims_a_fix_is_held_the_same_way() {
    let (msgs, logs) = run_claims(
        claims_policy(),
        &[],
        vec![],
        "Fix the npm build.",
        |n| match n {
            1 => edit_app(),
            2 => ChatResponse::call(
                "",
                "finish",
                json!({"summary": "Fixed the npmBuild task; the build now works."}),
            ),
            _ => ChatResponse::call(
                "",
                "finish",
                json!({"summary": "Changed the npmBuild task to source nvm first; not yet run."}),
            ),
        },
    );
    assert_eq!(user_msgs(&msgs, HELD).len(), 1);
    assert!(msgs.iter().any(|m| m
        .content
        .starts_with("[finish held: the summary claims an outcome")));
    assert!(
        logs.iter()
            .any(|l| l.contains("[claims] hold: \"Fixed the\"")),
        "{logs:#?}"
    );
}

#[test]
fn no_write_a_question_the_ablation_or_an_unattended_run_never_holds() {
    let claim = |n: usize| match n {
        1 => edit_app(),
        _ => ChatResponse::text("Fixed the blank page; it now loads."),
    };
    // No write this turn: the claim is not about this turn's code.
    let (msgs, _) = run_claims(claims_policy(), &[], vec![], "Is the page fixed?", |_| {
        ChatResponse::text("Fixed the blank page earlier; it now loads.")
    });
    assert!(user_msgs(&msgs, HELD).is_empty());
    let (msgs, logs) = run_claims(
        claims_policy(),
        &["claims_hold"],
        vec![],
        "Fix the blank page.",
        claim,
    );
    assert!(user_msgs(&msgs, HELD).is_empty());
    assert!(!logs.iter().any(|l| l.contains("[claims]")));
    let unattended = TurnPolicy {
        gates: Gates::NONE.with(Gate::Claims),
        ..TurnPolicy::unattended()
    };
    let (msgs, logs) = run_claims(
        unattended,
        &[],
        vec![],
        "Fix the blank page.",
        |n| match n {
            1 => edit_app(),
            _ => ChatResponse::call(
                "",
                "finish",
                json!({"summary": "Fixed the blank page; it now loads."}),
            ),
        },
    );
    assert!(user_msgs(&msgs, HELD).is_empty());
    assert!(!logs.iter().any(|l| l.contains("[claims]")), "{logs:#?}");
}

// ---- host tools (docs/host-tools.md, bead boubou-c9a) ----

/// A runner whose session has host tools: `choreo.say_hello` inlined, or past the cap.
struct HostTools {
    inlined: bool,
}

impl ToolRunner for HostTools {
    fn run_tool(&mut self, name: &str, _args: &Value) -> String {
        match name {
            "choreo.say_hello" => "Hello, Ada!".to_string(),
            "finish" => "[TASK_COMPLETE] ok".to_string(),
            _ => "[ok]".to_string(),
        }
    }
    fn host_face(&self, _offline: bool) -> rufous_kernel::mcp::HostFace {
        let def = json!({"type": "function", "function": {
            "name": "choreo.say_hello",
            "description": "Say Hello: greets someone by name.",
            "parameters": {"type": "object", "properties": {"who": {"type": "string"}}},
        }});
        rufous_kernel::mcp::HostFace {
            names: vec!["choreo.say_hello".into()],
            titles: vec!["Say Hello".into()],
            defs: if self.inlined { vec![def] } else { Vec::new() },
            inlined: self.inlined,
        }
    }
}

struct Offered<F: FnMut(usize) -> ChatResponse> {
    n: usize,
    f: F,
    offered: Arc<Mutex<Vec<Vec<String>>>>,
}

impl<F: FnMut(usize) -> ChatResponse> ChatBackend for Offered<F> {
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

#[test]
fn a_question_turn_is_offered_the_host_tools_and_named_them() {
    // Session acp_1105_105519: "…then tell me exactly what it returned" went to the question
    // route, which offered no MCP tools, and the model said the automation did not exist.
    let d = tempfile::tempdir().expect("tmp");
    let offered = Arc::new(Mutex::new(Vec::new()));
    let mut a = Agent::new(
        RunParams::plain("qwen3.5:4b", d.path()),
        Offered {
            n: 0,
            offered: offered.clone(),
            f: |n| match n {
                1 => ChatResponse::call("", "choreo.say_hello", json!({"who": "Ada"})),
                _ => ChatResponse::text("It returned: Hello, Ada!"),
            },
        },
        HostTools { inlined: true },
    );
    let out = a
        .run(start("What did Say Hello return for Ada?"), None, None)
        .expect("loop");
    assert!(
        out[0].content.contains("chatting with the user"),
        "a question turn"
    );
    assert!(out[0].content.contains("HOST TOOLS") && out[0].content.contains("choreo.say_hello"));
    let first = offered.lock().expect("lock")[0].clone();
    assert!(first.iter().any(|n| n == "choreo.say_hello"), "{first:?}");
    assert!(out
        .iter()
        .any(|m| m.role == Role::Tool && m.content == "Hello, Ada!"));
}

#[test]
fn host_tools_past_the_cap_ride_on_mcp_call() {
    let d = tempfile::tempdir().expect("tmp");
    let offered = Arc::new(Mutex::new(Vec::new()));
    let mut a = Agent::new(
        RunParams::plain("qwen3.5:4b", d.path()),
        Offered {
            n: 0,
            offered: offered.clone(),
            f: |n| match n {
                1 => ChatResponse::call(
                    "",
                    "mcp_call",
                    json!({"server": "choreo", "tool": "say_hello", "args": {"who": "Ada"}}),
                ),
                _ => ChatResponse::text("Done."),
            },
        },
        HostTools { inlined: false },
    );
    let out = a
        .run(start("What did Say Hello return for Ada?"), None, None)
        .expect("loop");
    assert!(out[0].content.contains("call mcp_call"));
    let first = offered.lock().expect("lock")[0].clone();
    assert!(first.iter().any(|n| n == "mcp_call"), "{first:?}");
    assert!(!first.iter().any(|n| n == "choreo.say_hello"), "{first:?}");
    assert!(out
        .iter()
        .filter(|m| m.role == Role::Tool)
        .all(|m| !m.content.contains("is not available")));
}

fn run_host_turn(
    task: &str,
    ablation: &[&str],
    f: impl FnMut(usize) -> ChatResponse,
) -> Vec<Message> {
    let d = tempfile::tempdir().expect("tmp");
    let mut params = RunParams::plain("qwen3.5:4b", d.path());
    // A host serves a conversation (`--policy general`): text ends the turn.
    params.policy = TurnPolicy::conversation();
    params.ablation = ablation.iter().map(|s| s.to_string()).collect();
    let mut a = Agent::new(
        params,
        Offered {
            n: 0,
            offered: Arc::new(Mutex::new(Vec::new())),
            f,
        },
        HostTools { inlined: true },
    );
    a.run(start(task), None, None).expect("loop")
}

#[test]
fn a_named_host_tool_gets_a_note_before_the_first_call() {
    // Arm A (bead boubou-8c4): "Say Hello" in the instruction is choreo.say_hello, in text.
    let out = run_host_turn("What did Say Hello return for Ada?", &[], |n| match n {
        1 => ChatResponse::call("", "choreo.say_hello", json!({"who": "Ada"})),
        _ => ChatResponse::text("It returned: Hello, Ada!"),
    });
    let note = out
        .iter()
        .find(|m| m.role == Role::User && m.content.starts_with("Hint:"))
        .expect("mention note");
    assert!(note
        .content
        .contains("\"Say Hello\" is the app's automation choreo.say_hello"));
    // The note sits right after the instruction, before the model's first reply.
    let at = out.iter().position(|m| std::ptr::eq(m, note)).expect("pos");
    assert!(out[at - 1].content.contains("What did Say Hello"));
}

#[test]
fn a_reply_that_skips_a_named_host_tool_ends_the_turn() {
    // No hold behind the note (bead boubou-8c4): arm B ran Weekly Report after "Don't run
    // Weekly Report" 5/6 on qwen3.5:4b. "Don't run" is the user's to say.
    let out = run_host_turn(
        "Don't run Say Hello, just tell me what it does.",
        &[],
        |_| ChatResponse::text("It greets someone by name."),
    );
    assert_eq!(
        out.iter().filter(|m| m.role == Role::Assistant).count(),
        1,
        "{out:?}"
    );
}

#[test]
fn a_greeting_is_not_a_mention_of_a_two_word_host_tool() {
    let out = run_host_turn("Hello!", &[], |_| ChatResponse::text("Hi!"));
    assert!(!out.iter().any(|m| m.content.starts_with("Hint:")));
}

// ---- per-call usage (docs/call-usage.md) ----

/// Each model call is reported once, in order: the main calls on their context report, the
/// compaction summary and the wrap-up as `ModelCall`. Calls are told apart by the scripted
/// output count, which is the call's index.
#[test]
fn every_model_call_is_reported_once_with_its_purpose() {
    let d = tempfile::tempdir().expect("tmp");
    let mut params = RunParams::plain("gemma4:12b", d.path());
    params.num_ctx = 100_000;
    let mut a = Agent::new(
        params,
        Responder {
            n: 0,
            f: |n| {
                let mut r = match n {
                    1..=3 => ChatResponse::call(
                        "",
                        "create_file",
                        json!({"path": format!("f{n}.txt"), "content": "x"}),
                    ),
                    // The third main call reports 95 % of the window: L3 compaction, whose
                    // summary is call 4.
                    4 => ChatResponse::text("summary of the work so far"),
                    5 => ChatResponse::call("", "finish", json!({"summary": ""})),
                    _ => ChatResponse::text("Wrapped up: three files."),
                };
                r.prompt_eval_count = Some(if n == 3 { 95_000 } else { 1_000 });
                r.eval_count = Some(n as u64);
                r
            },
        },
        OkTools,
    );
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let sink = Arc::clone(&events);
    a = a.with_sink(move |e| sink.lock().expect("lock").push(e));
    a.run(start(TASK), None, None).expect("loop");

    let ev = events.lock().expect("lock");
    let calls: Vec<(&str, u64)> = ev
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ContextUsage { call: Some(c), .. }
            | AgentEvent::ModelCall { call: c, .. } => {
                Some((c.purpose, c.output_tokens.expect("count")))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            ("turn", 1),
            ("turn", 2),
            ("turn", 3),
            ("compaction", 4),
            ("turn", 5),
            ("wrap_up", 6),
        ]
    );
    assert!(ev.iter().all(|e| match e {
        AgentEvent::ContextUsage { call, .. } => call.as_ref().is_some_and(|c| c.purpose == "turn"),
        AgentEvent::ModelCall { call, .. } => call.purpose != "turn",
        _ => true,
    }));
    // The model id is the one the loop names, the scripted backend having no override.
    assert!(ev.iter().all(|e| match e {
        AgentEvent::ModelCall { call, .. } => call.model == "gemma4:12b",
        _ => true,
    }));
    // The wrap-up is reported before `Finished`, not lost after it.
    let finished = ev
        .iter()
        .position(|e| matches!(e, AgentEvent::Finished { .. }))
        .expect("finished");
    assert!(!ev[finished..]
        .iter()
        .any(|e| matches!(e, AgentEvent::ModelCall { .. })));
}
