//! Port of the `_scripted_loop` wiring tests from `tests/test_guards.py` and
//! `tests/test_output_cap.py`: the loop driven by a scripted backend and a scripted tool
//! runner, no network, no model.

use std::path::Path;
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

const TASK: &str = "修复 m08.py 里的 bug,并运行测试验证。";
const EDIT_OK: &str = "[edited m08.py (已备份),.bak 备份可用]";

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
        "[edit failed: old text not found. 文件开头预览: ...]"
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
    let hints = user_msgs(&msgs, "连续成功编辑");
    assert_eq!(hints.len(), 1);
    assert!(hints[0].content.contains("m08.py"));
    assert!(!a.face.is_disabled("edit_file"));
}

#[test]
fn failed_edits_not_counted() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = (0..5).map(|i| edit(i, false)).collect();
    let msgs = scripted(d.path(), &calls);
    assert!(user_msgs(&msgs, "连续成功编辑").is_empty());
}

#[test]
fn disabled_edit_hallucination_not_counted() {
    let d = tempfile::tempdir().expect("tmp");
    let calls: Vec<Call> = (0..5)
        .map(|i| {
            (
                "edit_file",
                json!({"path": "m08.py", "old": i.to_string(), "new": "x"}),
                "[tool edit_file 已被禁用(连续重复调用保护)。]",
            )
        })
        .collect();
    let mut a = agent_for(d.path(), &calls);
    a.face.disabled.insert("edit_file".into());
    let msgs = a.run(start(TASK), None, None).expect("loop");
    assert!(user_msgs(&msgs, "连续成功编辑").is_empty());
    // The refusal text is the loop's own (the scripted result is never consulted).
    assert!(msgs
        .iter()
        .any(|m| m.role == Role::Tool && m.content.contains("已被禁用")));
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
    let rereads = user_msgs(&msgs, "交付前自查");
    assert_eq!(rereads.len(), 1);
    assert!(rereads[0].content.contains(task));
    assert!(rereads[0].content.contains("重新 finish"));
    assert!(rereads[0].content.starts_with("⚠️"));
    assert_eq!(msgs.last().map(|m| &m.role), Some(&Role::Assistant));
}

#[test]
fn finish_reread_skipped_for_qa() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = agent_for(d.path(), &[])
        .run(start("你好"), None, None)
        .expect("loop");
    assert!(user_msgs(&msgs, "交付前自查").is_empty());
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
                json!({"summary": "已完成,见 report.md 和 real.txt"}),
            ),
            _ => ChatResponse::call("done", "finish", json!({"summary": "全部完成"})),
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
    let task = "写一份报告到 report.md,输出纯数字结论。";
    let mut a = Agent::new(
        RunParams::plain("gemma4:12b", d.path()),
        Liar { phase: 0 },
        CreateOnly,
    );
    let msgs = a.run(start(task), None, None).expect("loop");
    assert!(!user_msgs(&msgs, "声称的以下产物").is_empty());
    assert!(user_msgs(&msgs, "交付前自查").len() <= 1);
}

#[test]
fn fake_finish_rejection_carries_next_action() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = agent_with(d.path(), &[], "", "")
        .run(start(TASK), None, None)
        .expect("loop");
    let rejects = user_msgs(&msgs, "finish 被拒绝");
    assert!(!rejects.is_empty());
    let c = &rejects[0].content;
    assert!(c.contains("下一步三选一"));
    assert!(c.contains("create_file") && c.contains("answer.txt"));
    assert!(c.contains("继续用工具推进"));
    assert!(c.contains("分析/讲解类任务"));
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
    let msgs = agent_with(d.path(), &[], "变量逐条解释如下:…", "all done")
        .run(start(TASK), None, None)
        .expect("loop");
    assert!(user_msgs(&msgs, "finish 被拒绝").is_empty());
}

#[test]
fn fake_finish_allows_substantive_summary() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = agent_with(d.path(), &[], "", "结论:共 17 千小时。")
        .run(start(TASK), None, None)
        .expect("loop");
    assert!(user_msgs(&msgs, "finish 被拒绝").is_empty());
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
    let gates = user_msgs(&msgs, "项未标记完成");
    assert_eq!(gates.len(), 2);
    assert!(gates[0].content.contains("1/2"));
    assert_eq!(msgs.last().map(|m| &m.role), Some(&Role::Assistant));
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
    assert!(user_msgs(&msgs, "已被禁用").is_empty());
    // enable_tools really put web_search on the face: no "当前不可用" refusals.
    assert!(!msgs.iter().any(|m| m.content.contains("当前不可用")));
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
    assert!(!user_msgs(&msgs, "重复完全相同的网络查询").is_empty());
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
    assert!(!user_msgs(&msgs, "重复完全相同的网络查询").is_empty());
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
    assert!(!msgs.iter().any(|m| m.content.contains("当前不可用")));
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
        .filter(|m| m.role == Role::Tool && m.content.starts_with("[已拦截"))
        .collect();
    assert_eq!(blocked.len(), 1);
    assert!(!user_msgs(&msgs, "不要原样重试").is_empty());
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
    a.run(start("写一个大文件并验证。"), None, None)
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
    let advice = user_msgs(&out, "append_file 逐段追加");
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
    assert!(joined.contains("上下文已重置"));
    assert!(out.iter().any(|m| m.content.contains("写一个大文件并验证")));
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
    let err = a.run(start("写一个大文件并验证。"), None, None).err();
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
            f: |_| ChatResponse::text("你好!我是布布。"),
        },
        OkTools,
    );
    let out = a.run(start("你好"), None, None).expect("loop");
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
    assert!(!user_msgs(&out, "回答已经足够").is_empty());
    assert!(out.len() < 8);
}

#[test]
fn repeated_text_is_a_dead_loop() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_responder(d.path(), |n| {
        if n <= 3 {
            ChatResponse::text("我将开始实现这个功能,首先创建文件。")
        } else {
            ChatResponse::call("done", "finish", json!({"summary": "all done"}))
        }
    });
    assert!(!user_msgs(&out, "判定为死循环").is_empty());
    assert!(out
        .last()
        .is_some_and(|m| m.role == Role::Tool && m.content.contains("TASK_COMPLETE")));
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
