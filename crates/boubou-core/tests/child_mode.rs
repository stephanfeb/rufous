//! Child mode (`AGENT_CHILD_SANDBOX=1`) and the dispatch hook as the loop sees them: port of
//! the child-related cases of `tests/test_integration_0901.py` and
//! `tests/test_parallel_safety.py::TestChildModeInstall`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use boubou_core::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use boubou_core::agent::prompt::{chat_system, system_base, CHAT_SYSTEM, CHILD_SYSTEM};
use boubou_core::agent::{Agent, AgentEvent, RunParams, ToolRunner};
use boubou_core::exec::Executor;
use boubou_core::guards::qa::{is_qa, task_routing_text, DISPATCH_NOTE_MARK};
use boubou_core::ollama::{Message, Role};
use boubou_core::parallel::dispatch::{ChildProcess, SpawnRequest};
use boubou_core::parallel::{
    CapacityPlan, CapacityPlanner, DispatchHook, HookEvent, ParallelConfig, Spawner,
};
use boubou_core::safety::SafetyPolicy;
use boubou_core::todo::{save_todo, TodoItem};
use boubou_core::Error;
use serde_json::{json, Value};

type Call = (&'static str, Value, &'static str);

struct Scripted {
    calls: Vec<Call>,
    i: usize,
}

impl ChatBackend for Scripted {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        let k = self.i;
        self.i += 1;
        let mut r = if k < self.calls.len() {
            let (name, args, _) = &self.calls[k];
            ChatResponse::call("", name, args.clone())
        } else {
            ChatResponse::call("done", "finish", json!({"summary": "all done"}))
        };
        r.prompt_eval_count = Some(500);
        Ok(r)
    }
}

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

fn agent(workdir: &Path, calls: &[Call], child: bool) -> Agent<Scripted, ScriptedTools> {
    let mut params = RunParams::plain("small:2b", workdir);
    params.child_mode = child;
    let mut a = Agent::new(
        params,
        Scripted {
            calls: calls.to_vec(),
            i: 0,
        },
        ScriptedTools {
            calls: calls.to_vec(),
        },
    );
    a.sessions_dir = workdir.join("sessions");
    a
}

fn rejections(msgs: &[Message]) -> usize {
    msgs.iter()
        .filter(|m| {
            m.role == Role::User
                && m.content.contains("finish was rejected")
                && m.content.contains("not marked done")
        })
        .count()
}

const CREATE: Call = ("create_file", json_const(), "[created out.md]");

const fn json_const() -> Value {
    Value::Null
}

fn create_call() -> Call {
    (
        "create_file",
        json!({"path": "out.md", "content": "x"}),
        "[created out.md]",
    )
}

// ---------------- b. the child is exempt from the plan finish gate ----------------

#[test]
fn main_agent_finish_blocked_by_unsynced_plan_but_child_is_not() {
    let _ = CREATE;
    let d = tempfile::tempdir().expect("tmp");
    let plan = vec![
        TodoItem {
            item: "step 1".into(),
            done: true,
        },
        TodoItem::new("step 2"),
    ];
    save_todo(d.path(), &plan).expect("todo");
    let task = "Fix the bug in m08.py and run the tests to verify.";
    let msgs = agent(d.path(), &[create_call()], false)
        .run(
            vec![Message::system("sys"), Message::user(task)],
            None,
            None,
        )
        .expect("loop");
    assert!(
        rejections(&msgs) >= 1,
        "the parent's plan gate must still fire"
    );

    save_todo(d.path(), &plan).expect("todo");
    let msgs = agent(d.path(), &[create_call()], true)
        .run(
            vec![Message::system("CHILD-SYS"), Message::user(task)],
            None,
            None,
        )
        .expect("loop");
    assert_eq!(rejections(&msgs), 0);
}

// ---------------- c. a child is never Q&A ----------------

#[test]
fn child_qa_like_text_stays_task_mode() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = agent(d.path(), &[], true)
        .run(
            vec![Message::system("CHILD-SYS"), Message::user("What is this?")],
            None,
            None,
        )
        .expect("loop");
    assert_eq!(msgs[0].content, "CHILD-SYS");
    assert!(!msgs
        .iter()
        .any(|m| m.role == Role::System && m.content.starts_with(CHAT_SYSTEM)));
    // Control: the parent treats the same text as Q&A.
    assert!(is_qa(&[Message::user("What is this?")], false));
}

#[test]
fn child_prompt_is_minimal_and_parent_prompt_has_no_dispatch_text() {
    let d = tempfile::tempdir().expect("tmp");
    let mut a = agent(d.path(), &[], true);
    let m = a.task_messages("[simple] convert a.csv to a.md");
    assert!(m[0].content.starts_with(CHILD_SYSTEM));
    assert!(
        !m[0].content.contains("Platform:"),
        "no platform block for a child"
    );
    assert!(CHILD_SYSTEM.len() < system_base().len());
    let blob = format!("{}{}", system_base(), chat_system(None)).to_lowercase();
    for kw in ["sub-agent", "subagent", "parallel", "dispatch"] {
        assert!(!blob.contains(kw), "{kw} leaked into the parent prefill");
    }
}

// ---------------- d. the receipt pollutes neither routing nor Q&A ----------------

#[test]
fn routing_and_qa_skip_the_dispatch_note() {
    let note =
        format!("{DISPATCH_NOTE_MARK} The following items were completed by parallel sub-tasks; their output files are in the current working directory:");
    let msgs = vec![
        Message::user("convert data.csv to a markdown table"),
        Message::assistant(""),
        Message::user(note.clone()),
    ];
    assert_eq!(
        task_routing_text(&msgs),
        "convert data.csv to a markdown table"
    );
    let msgs = vec![
        Message::user("What does this error mean?"),
        Message::user(note.clone()),
    ];
    assert!(is_qa(&msgs, false));
    assert_eq!(task_routing_text(&[Message::user(note)]), "");
}

// ---------------- the hook ----------------

struct Stub(usize);
impl CapacityPlanner for Stub {
    fn plan_capacity(&self, _m: &str, _c: &ParallelConfig) -> CapacityPlan {
        CapacityPlan {
            max_parallel: self.0,
            ..Default::default()
        }
    }
}

struct FakeOk;
struct Done;
impl ChildProcess for Done {
    fn poll(&mut self) -> Option<i32> {
        Some(0)
    }
    fn kill(&mut self) -> bool {
        true
    }
    fn pid(&self) -> u32 {
        1
    }
}
impl Spawner for FakeOk {
    fn spawn(&self, req: &SpawnRequest<'_>) -> Result<Box<dyn ChildProcess>, String> {
        let name = req
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let art = format!("out_{name}.md");
        std::fs::write(req.cwd.join(&art), "# r\n").expect("write");
        std::fs::write(
            req.log_path,
            format!("===== TASK COMPLETE =====\ncreated {art}\n"),
        )
        .expect("log");
        std::fs::write(
            req.cwd.join(".agent_state.json"),
            format!("[{{\"role\":\"assistant\",\"content\":\"[TASK_COMPLETE] created {art}\"}}]"),
        )
        .expect("ckpt");
        Ok(Box::new(Done))
    }
}

fn enabled_cfg() -> ParallelConfig {
    ParallelConfig {
        enabled: true,
        child_model: "c".into(),
        ..Default::default()
    }
}

fn hook(cfg: ParallelConfig, env: &[(&str, &str)]) -> DispatchHook {
    let env: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    DispatchHook::new(cfg, "http://127.0.0.1:1")
        .with_env(env)
        .with_probe(Arc::new(Stub(1)))
        .with_spawner(Arc::new(FakeOk))
}

fn run_hook(h: &DispatchHook, msgs: Vec<Message>, wd: &Path) -> (Vec<Message>, Vec<HookEvent>) {
    let mut ev = Vec::new();
    let out = h.maybe_dispatch(msgs, wd, wd, &mut |e| ev.push(e));
    (out, ev)
}

/// A tool runner whose `todo(action=create)` really writes the plan, into `plan_dir`.
struct PlanWriter {
    plan_dir: std::path::PathBuf,
}

impl ToolRunner for PlanWriter {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        match name {
            "todo" => {
                let items: Vec<TodoItem> = args["items"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(TodoItem::new)
                            .collect()
                    })
                    .unwrap_or_default();
                let n = items.len();
                save_todo(&self.plan_dir, &items).expect("plan");
                format!("[todo created: {n} items]")
            }
            "finish" => "[TASK_COMPLETE] all done".to_string(),
            _ => "[tool error: not scripted]".to_string(),
        }
    }
}

#[test]
fn the_plan_the_model_writes_is_dispatched_once_from_the_harness_dir() {
    // The usual shape: no plan on disk when the loop starts, the model writes one with its
    // first call, the hook then reads it from the session's harness directory (not the
    // project) and farms the mechanical items out. A second todo(create) does not dispatch again.
    let d = tempfile::tempdir().expect("tmp");
    let hdir = d.path().join("session.files");
    let items: Vec<Value> = (1..=6)
        .map(|i| Value::from(format!("convert data{i}.csv to a markdown table")))
        .collect();
    let calls: Vec<Call> = vec![
        ("todo", json!({"action": "create", "items": items}), ""),
        ("todo", json!({"action": "create", "items": ["again"]}), ""),
    ];
    let mut params = RunParams::plain("small:2b", d.path());
    params.harness_dir = Some(hdir.clone());
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let mut a = Agent::new(
        params,
        Scripted { calls, i: 0 },
        PlanWriter {
            plan_dir: hdir.clone(),
        },
    )
    .with_parallel(hook(enabled_cfg(), &[]))
    .with_sink(move |ev| sink.lock().expect("events").push(ev));
    a.sessions_dir = d.path().join("sessions");
    let msgs = a
        .run(
            vec![
                Message::system("sys"),
                Message::user("Convert each data*.csv file in this directory to a markdown table."),
            ],
            None,
            None,
        )
        .expect("loop");
    let receipts: Vec<usize> = msgs
        .iter()
        .enumerate()
        .filter(|(_, m)| m.content.starts_with(DISPATCH_NOTE_MARK))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        receipts.len(),
        1,
        "one dispatch for two plan writes: {msgs:#?}"
    );
    let first_tool = msgs
        .iter()
        .position(|m| m.role == Role::Tool)
        .expect("the todo call ran");
    assert!(
        receipts[0] > first_tool,
        "the receipt follows the plan, it does not precede the model's first turn"
    );
    for i in 1..=6 {
        assert!(
            d.path().join(format!("out_task{i:02}.md")).is_file(),
            "child {i} ran in the project"
        );
    }
    assert!(
        !d.path().join("todo.json").exists(),
        "no plan in the project"
    );
    let ev = events.lock().expect("events");
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, AgentEvent::Log(l) if l.contains("[parallel] triggered")))
            .count(),
        1
    );
}

#[test]
fn dispatch_off_by_default_returns_messages_identical() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = vec![
        Message::system("sys"),
        Message::user("convert data.csv to a markdown table"),
    ];
    let (out, ev) = run_hook(
        &hook(ParallelConfig::default(), &[]),
        msgs.clone(),
        d.path(),
    );
    assert_eq!(out, msgs);
    assert!(ev.is_empty());
}

#[test]
fn env_hard_gate_child_mode_and_receipt_all_block() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = vec![Message::system("sys"), Message::user("task")];
    let (out, _) = run_hook(
        &hook(enabled_cfg(), &[("AGENT_PARALLEL", "0")]),
        msgs.clone(),
        d.path(),
    );
    assert_eq!(out, msgs);
    let (out, _) = run_hook(
        &hook(enabled_cfg(), &[]).with_child_mode(true),
        msgs.clone(),
        d.path(),
    );
    assert_eq!(out, msgs);
    let with_note = vec![
        Message::system("sys"),
        Message::user("task"),
        Message::user(format!(
            "{DISPATCH_NOTE_MARK} The following items were completed by parallel sub-tasks"
        )),
    ];
    let (out, ev) = run_hook(&hook(enabled_cfg(), &[]), with_note.clone(), d.path());
    assert_eq!(out, with_note); // idempotent on resume, no second dispatch
    assert!(ev.is_empty());
}

#[test]
fn the_agents_stop_flag_reaches_the_dispatch_hook_and_its_children() {
    // Wired either way round: the hook attached after the flag, or the flag after the hook.
    let d = tempfile::tempdir().expect("tmp");
    let flag = Arc::new(AtomicBool::new(false));
    let a = agent(d.path(), &[], false)
        .with_stop(flag.clone())
        .with_parallel(hook(enabled_cfg(), &[]));
    assert!(Arc::ptr_eq(
        &a.parallel.as_ref().expect("hook").abort,
        &flag
    ));
    let a = agent(d.path(), &[], false)
        .with_parallel(hook(enabled_cfg(), &[]))
        .with_stop(flag.clone());
    assert!(Arc::ptr_eq(
        &a.parallel.as_ref().expect("hook").abort,
        &flag
    ));

    // Raised, a dispatch that would otherwise run six children spawns none.
    let todo: Vec<Value> = (1..=6)
        .map(|i| json!({"item": format!("convert data{i}.csv to a markdown table"), "done": false}))
        .collect();
    std::fs::write(
        d.path().join("todo.json"),
        serde_json::to_string(&todo).expect("json"),
    )
    .expect("todo");
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let mut a = agent(d.path(), &[], false)
        .with_parallel(hook(enabled_cfg(), &[]))
        .with_stop(flag.clone())
        .with_sink(move |ev| sink.lock().expect("events").push(ev));
    flag.store(true, Ordering::SeqCst);
    let msgs = vec![
        Message::system("sys"),
        Message::user("Convert each data*.csv file in this directory to a markdown table."),
    ];
    a.run(msgs, None, None).expect("a stop is a clean exit");
    assert!(!d.path().join("out_task01.md").exists(), "a child ran");
    let ev = events.lock().expect("events");
    assert!(ev.iter().any(
        |e| matches!(e, AgentEvent::Dispatch(v) if v["phase"] == "done" && v["aborted"] == true)
    ));
    assert!(ev
        .iter()
        .any(|e| matches!(e, AgentEvent::Log(l) if l.contains("[stopped]"))));
}

#[test]
fn note_marker_prefix_added_by_harness_when_dispatch_happens() {
    let d = tempfile::tempdir().expect("tmp");
    let todo: Vec<Value> = (1..=6)
        .map(|i| json!({"item": format!("convert data{i}.csv to a markdown table"), "done": false}))
        .collect();
    std::fs::write(
        d.path().join("todo.json"),
        serde_json::to_string(&todo).expect("json"),
    )
    .expect("todo");
    let msgs = vec![Message::system("sys"), Message::user("task")];
    let (out, ev) = run_hook(&hook(enabled_cfg(), &[]), msgs, d.path());
    let last = out.last().expect("note");
    assert_eq!(last.role, Role::User);
    assert!(
        last.content.starts_with(DISPATCH_NOTE_MARK),
        "{}",
        last.content
    );
    assert!(last.content.contains("do not redo them"));
    assert!(ev
        .iter()
        .any(|e| matches!(e, HookEvent::Log(l) if l.contains("[parallel] triggered"))));
    assert!(ev
        .iter()
        .any(|e| matches!(e, HookEvent::Dispatch(v) if v["phase"] == "done")));
    for i in 1..=6 {
        assert!(d.path().join(format!("out_task{i:02}.md")).is_file());
    }
    let todo = boubou_core::todo::load_todo(d.path());
    assert!(todo.iter().all(|t| t.done));
}

#[test]
fn hook_logs_why_not_when_enabled_but_nothing_to_do() {
    let d = tempfile::tempdir().expect("tmp");
    let msgs = vec![Message::system("sys"), Message::user("task")];
    let (out, ev) = run_hook(&hook(enabled_cfg(), &[]), msgs.clone(), d.path());
    assert_eq!(out, msgs);
    assert!(ev.iter().any(
        |e| matches!(e, HookEvent::Log(l) if l.contains("not dispatching") && l.contains("too_few_pending"))
    ));
}

// ---------------- the executor's child gate and the loop's exit 53 ----------------

#[test]
fn install_narrows_tools_and_gate() {
    let d = tempfile::tempdir().expect("tmp");
    let mut ex = Executor::with_policy(d.path(), SafetyPolicy::default());
    let allowed = ex.install_child_mode(&ParallelConfig::default());
    assert_eq!(
        allowed,
        ["read_file", "list_dir", "create_file", "todo", "finish"]
    );
    // A hallucinated run_bash trips the fuse at once.
    let r = ex.run_tool("run_bash", &json!({"command": "dir"}));
    assert!(r.contains("child safety gate"), "{r}");
    assert!(ex.child_terminated().is_some());

    let mut ex = Executor::with_policy(d.path(), SafetyPolicy::default());
    ex.install_child_mode(&ParallelConfig::default());
    let r = ex.run_tool("read_file", &json!({"path": ".ssh/id_rsa"}));
    assert!(r.contains("default-deny"), "{r}");
    assert!(ex.child_terminated().is_none());
    let r = ex.run_tool("create_file", &json!({"path": "out.md", "content": "x"}));
    assert!(r.contains("created"), "{r}");
    assert!(d.path().join("audit.log").is_file());
    // Allowed tools without a path or command pass (the live run of 2026-10-28 caught this).
    let r = ex.run_tool("todo", &json!({"action": "create", "items": ["a", "b"]}));
    assert!(!r.contains("safety gate"), "{r}");
    let r = ex.run_tool("finish", &json!({"summary": "done"}));
    assert!(r.contains("TASK_COMPLETE"), "{r}");
    assert!(ex.child_terminated().is_none());
    // The confirmation channel is gone: an out-of-bounds read is a plain refusal.
    let r = ex.run_tool("read_file", &json!({"path": "/etc/hosts"}));
    assert!(r.contains("child safety gate"), "{r}");
}

#[test]
fn loop_ends_with_child_terminated_after_a_severe_refusal() {
    let d = tempfile::tempdir().expect("tmp");
    let mut params = RunParams::plain("small:2b", d.path());
    params.child_mode = true;
    let mut ex = Executor::with_policy(d.path(), SafetyPolicy::default());
    let allowed = ex.install_child_mode(&ParallelConfig::default());
    let backend = Scripted {
        calls: vec![(
            "create_file",
            json!({"path": "../escape.md", "content": "x"}),
            "",
        )],
        i: 0,
    };
    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = logs.clone();
    let mut a = Agent::new(params, backend, ex).with_sink(move |ev| {
        if let AgentEvent::Log(l) = ev {
            if let Ok(mut v) = sink.lock() {
                v.push(l);
            }
        }
    });
    a.face.narrow_to(&allowed);
    a.sessions_dir = d.path().join("sessions");
    let msgs = a.task_messages("[simple] convert a.csv to a.md");
    let err = a.run(msgs, Some("child1"), None).expect_err("fuse");
    assert!(matches!(err, Error::ChildTerminated(_)), "{err}");
    let logs = logs.lock().expect("logs");
    assert!(logs
        .iter()
        .any(|l| l.contains("[child-mode] sandbox installed")));
    assert!(logs.iter().any(|l| l.contains("circuit broken (exit 53)")));
    assert!(
        d.path().join(".agent_state.json").is_file(),
        "checkpoint saved before exit"
    );
    assert!(!d.path().join("../escape.md").exists());
}
