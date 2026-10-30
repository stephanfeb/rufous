//! Port of `tests/test_parallel_dispatch.py`: dispatcher and integrator with a scripted
//! spawner — no model, no real child processes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use boubou_kernel::parallel::dispatch::{ChildProcess, SpawnRequest};
use boubou_kernel::parallel::{
    artifact_files, progress_line, summary_for_parent, CapacityPlan, CapacityPlanner, ChildStatus,
    ChildTask, DispatchItem, DispatchResult, JsonTodoProvider, ParallelConfig, ParallelDispatcher,
    ResultIntegrator, Spawner, TodoProvider,
};
use serde_json::{json, Value};

fn cfg() -> ParallelConfig {
    ParallelConfig {
        hard_cap: 2,
        ..Default::default()
    }
}

struct StubProbe(usize);

impl CapacityPlanner for StubProbe {
    fn plan_capacity(&self, _m: &str, _c: &ParallelConfig) -> CapacityPlan {
        CapacityPlan {
            max_parallel: self.0,
            ..Default::default()
        }
    }
}

/// A finished (or never-finishing) fake child.
struct FakeProc {
    rc: Option<i32>,
    killed: Arc<Mutex<bool>>,
}

impl ChildProcess for FakeProc {
    fn poll(&mut self) -> Option<i32> {
        self.rc
    }
    fn kill(&mut self) -> bool {
        if let Ok(mut k) = self.killed.lock() {
            *k = true;
        }
        self.rc = Some(-9);
        true
    }
    fn pid(&self) -> u32 {
        4242
    }
}

/// What a successful child leaves behind: artefact + agent.log + checkpoint. The summary
/// must name the artefact (the judge cross-checks claims against files).
fn fake_child_ok(dir: &Path, artifact: &str, summary: Option<&str>) {
    let summary = summary
        .map(String::from)
        .unwrap_or_else(|| format!("created {artifact}"));
    std::fs::write(dir.join(artifact), "# report\n\nhello\n").expect("write");
    std::fs::write(
        dir.join("agent.log"),
        format!("[0] ⚙ create_file ...\n===== TASK COMPLETE =====\n{summary}\n"),
    )
    .expect("write");
    std::fs::write(
        dir.join(".agent_state.json"),
        serde_json::to_string(
            &json!([{"role": "assistant", "content": format!("[TASK_COMPLETE] {summary}")}]),
        )
        .expect("json"),
    )
    .expect("write");
}

fn items(n: usize) -> Vec<DispatchItem> {
    (1..=n)
        .map(|i| {
            DispatchItem::new(
                i.to_string(),
                format!("convert data{i}.csv to a markdown table"),
            )
        })
        .collect()
}

type Behaviour = dyn Fn(&SpawnRequest<'_>) -> Result<Box<dyn ChildProcess>, String> + Send + Sync;

/// Records every spawn and delegates the behaviour to a closure.
struct Scripted {
    cmds: Mutex<Vec<Vec<String>>>,
    envs: Mutex<Vec<HashMap<String, String>>>,
    behaviour: Box<Behaviour>,
}

impl Scripted {
    fn new(
        b: impl Fn(&SpawnRequest<'_>) -> Result<Box<dyn ChildProcess>, String> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            cmds: Mutex::new(Vec::new()),
            envs: Mutex::new(Vec::new()),
            behaviour: Box::new(b),
        })
    }
    fn n(&self) -> usize {
        self.cmds.lock().map(|c| c.len()).unwrap_or(0)
    }
}

impl Spawner for Scripted {
    fn spawn(&self, req: &SpawnRequest<'_>) -> Result<Box<dyn ChildProcess>, String> {
        if let Ok(mut c) = self.cmds.lock() {
            c.push(req.cmd.to_vec());
        }
        if let Ok(mut e) = self.envs.lock() {
            e.push(req.env.clone());
        }
        (self.behaviour)(req)
    }
}

fn done(rc: i32) -> Box<dyn ChildProcess> {
    Box::new(FakeProc {
        rc: Some(rc),
        killed: Arc::new(Mutex::new(false)),
    })
}

fn success_spawner() -> Arc<Scripted> {
    Scripted::new(|req| {
        let name = req
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // log_path is the child's agent.log, which fake_child_ok has already filled.
        assert_eq!(req.log_path, req.cwd.join("agent.log"));
        fake_child_ok(req.cwd, &format!("out_{name}.md"), None);
        Ok(done(0))
    })
}

fn dispatcher(c: ParallelConfig, sp: Arc<Scripted>) -> ParallelDispatcher {
    ParallelDispatcher::new(c)
        .with_spawner(sp)
        .with_env(HashMap::new())
        .with_agent_exe("/opt/boubou/boubou")
}

fn fast_clock(d: ParallelDispatcher) -> (ParallelDispatcher, Arc<Mutex<f64>>) {
    let now = Arc::new(Mutex::new(1000.0));
    let c = now.clone();
    let s = now.clone();
    let d = d.with_clock(
        Arc::new(move || c.lock().map(|t| *t).unwrap_or(0.0)),
        Arc::new(move |dt| {
            if let Ok(mut t) = s.lock() {
                *t += dt;
            }
        }),
    );
    (d, now)
}

// ---------------- happy path ----------------

#[test]
fn children_env_and_command() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = success_spawner();
    let disp = dispatcher(cfg(), sp.clone());
    disp.run(
        items(2),
        d.path(),
        &StubProbe(2),
        Some("small:2b"),
        Some("r1"),
        0,
    );
    let envs = sp.envs.lock().expect("envs");
    let env = &envs[0];
    assert_eq!(env["AGENT_CHILD_SANDBOX"], "1");
    assert_eq!(env["AGENT_PARALLEL"], "0");
    assert_eq!(env["MINGBIRD_DEPTH"], "1");
    assert!(!env.contains_key("HUMMINGBIRD_DEPTH"));
    assert!(!env.contains_key("AGENT_STREAM"));
    let cmds = sp.cmds.lock().expect("cmds");
    let dirs: std::collections::BTreeSet<String> = cmds
        .iter()
        .map(|c| {
            Path::new(&c[4])
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        dirs,
        ["task01".to_string(), "task02".to_string()]
            .into_iter()
            .collect()
    );
    for c in cmds.iter() {
        assert_eq!(c[0], "/opt/boubou/boubou");
        assert_eq!(c[1], "run");
        assert_eq!(c[2], "small:2b");
        assert!(c[3].ends_with("task.md"));
        let i = c.iter().position(|x| x == "--session").expect("--session");
        assert!(c[i + 1].starts_with("disp_r1_"));
        assert_eq!(c.last().map(String::as_str), Some("--new"));
    }
}

#[test]
fn a_child_runs_at_the_context_window_the_planner_budgeted() {
    let mut parent = HashMap::new();
    parent.insert("AGENT_CTX".to_string(), "131072".to_string());
    let c = ParallelConfig {
        child_ctx: 8192,
        ..cfg()
    };
    let env = ParallelDispatcher::new(c).with_env(parent).child_env(0);
    assert_eq!(env["AGENT_CTX"], "8192");
    // The default budget too, with no parent value to override.
    let env = ParallelDispatcher::new(cfg()).child_env(0);
    assert_eq!(
        env["AGENT_CTX"],
        ParallelConfig::default().child_ctx.to_string()
    );
}

#[test]
fn child_env_strips_parent_time_budget() {
    let mut parent = HashMap::new();
    parent.insert("AGENT_TIME_BUDGET_SEC".to_string(), "5400".to_string());
    parent.insert("HUMMINGBIRD_DEPTH".to_string(), "0".to_string());
    parent.insert("PATH".to_string(), "/usr/bin".to_string());
    let d = ParallelDispatcher::new(cfg()).with_env(parent);
    let env = d.child_env(0);
    assert!(!env.contains_key("AGENT_TIME_BUDGET_SEC"));
    assert_eq!(env["AGENT_CHILD_SANDBOX"], "1");
    assert_eq!(env["AGENT_PARALLEL"], "0");
    assert_eq!(env["PATH"], "/usr/bin");
}

#[test]
fn three_items_all_ok_and_collected() {
    let d = tempfile::tempdir().expect("tmp");
    let todo: Vec<Value> = (0..3)
        .map(|_| json!({"item": "convert data.csv to a markdown table", "done": false}))
        .collect();
    std::fs::write(
        d.path().join("todo.json"),
        serde_json::to_string(&todo).expect("json"),
    )
    .expect("write");
    let mut provider = JsonTodoProvider::new(d.path());
    let disp = dispatcher(cfg(), success_spawner());
    let mut res = disp.run(items(3), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!((res.total, res.ok, res.failed), (3, 3, 0));
    let c = cfg();
    let (integrated, conflicts, _notes) = ResultIntegrator::new(&c)
        .with_provider(&mut provider)
        .integrate(&mut res, d.path());
    assert_eq!(integrated.len(), 3);
    assert!(conflicts.is_empty());
    for name in &integrated {
        assert!(d.path().join(name).is_file(), "{name}");
    }
    assert!(provider.all_items().expect("items").iter().all(|i| i.done));
}

#[test]
fn progress_events_emitted_and_protocol_line() {
    let d = tempfile::tempdir().expect("tmp");
    let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let disp = dispatcher(cfg(), success_spawner()).with_progress(Arc::new(move |ev| {
        if let Ok(mut v) = sink.lock() {
            v.push(ev);
        }
    }));
    disp.run(items(2), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    let ev = events.lock().expect("events");
    assert_eq!(ev[0]["phase"], "start");
    assert_eq!(ev[ev.len() - 1]["phase"], "done");
    assert_eq!(ev[ev.len() - 1]["ok"], 2);
    let line = progress_line(&json!({"phase": "start", "total": 2}));
    assert!(line.starts_with("@@DISPATCH@@") && line.contains("\"total\":2"));
}

// ---------------- failure / retry / fallback ----------------

#[test]
fn retry_then_success() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = Scripted::new(|req| {
        let name = req
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.ends_with("_retry1") {
            fake_child_ok(req.cwd, "out.md", None);
        }
        Ok(done(0)) // first attempt: no artifact, no TASK COMPLETE
    });
    let mut c = cfg();
    c.max_retries = 1;
    let res =
        dispatcher(c, sp.clone()).run(items(1), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!(res.ok, 1);
    assert_eq!(sp.n(), 2);
    assert!(d.path().join("_dispatch/r1/task01_retry1").is_dir());
}

#[test]
fn retry_exhausted_falls_back() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = Scripted::new(|_| Ok(done(0)));
    let mut c = cfg();
    c.max_retries = 1;
    let res = dispatcher(c, sp).run(items(2), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!(res.ok, 0);
    assert_eq!(res.fallback.len(), 2);
    assert!(res
        .fallback
        .iter()
        .all(|(_, why)| why.contains("no_task_complete")));
}

#[test]
fn missing_or_no_artifacts_rejected() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = Scripted::new(|req| {
        fake_child_ok(req.cwd, "out.md", Some("created missing.md and out.md"));
        Ok(done(0))
    });
    let mut c = cfg();
    c.max_retries = 0;
    let res =
        dispatcher(c.clone(), sp).run(items(1), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!(res.ok, 0);
    assert!(res.fallback[0].1.contains("missing_artifacts"));

    let d2 = tempfile::tempdir().expect("tmp");
    let sp = Scripted::new(|req| {
        fake_child_ok(req.cwd, "out.md", Some("done"));
        std::fs::remove_file(req.cwd.join("out.md")).expect("rm");
        Ok(done(0))
    });
    let res = dispatcher(c, sp).run(items(1), d2.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!(res.ok, 0);
    assert!(res.fallback[0].1.contains("no_artifacts"));
}

#[test]
fn timeout_kills_child() {
    let d = tempfile::tempdir().expect("tmp");
    let killed = Arc::new(Mutex::new(false));
    let k = killed.clone();
    let sp = Scripted::new(move |_| {
        Ok(Box::new(FakeProc {
            rc: None,
            killed: k.clone(),
        }) as Box<dyn ChildProcess>)
    });
    let mut c = cfg();
    c.per_task_timeout_s = 5.0;
    c.poll_interval_s = 1.0;
    c.max_retries = 0;
    let (disp, _now) = fast_clock(dispatcher(c, sp));
    let res = disp.run(items(1), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!(res.ok, 0);
    assert!(
        res.fallback[0].1.starts_with("hard_timeout"),
        "{}",
        res.fallback[0].1
    );
    assert!(*killed.lock().expect("killed"));
}

#[test]
fn the_dispatcher_aborts_on_the_callers_flag() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = success_spawner();
    let flag = Arc::new(AtomicBool::new(false));
    let disp = dispatcher(cfg(), sp.clone()).with_abort(flag.clone());
    assert!(!disp.aborted());
    flag.store(true, Ordering::SeqCst);
    assert!(disp.aborted());
    let res = disp.run(items(2), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert!(res.aborted);
    assert_eq!(sp.n(), 0);
}

#[test]
fn abort_before_run_spawns_nothing() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = success_spawner();
    let disp = dispatcher(cfg(), sp.clone());
    disp.abort();
    let res = disp.run(items(3), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert!(res.aborted);
    assert_eq!(sp.n(), 0);
    assert_eq!(res.fallback.len(), 3);
}

#[test]
fn capacity_lost_before_spawn_falls_back() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = success_spawner();
    let res = dispatcher(cfg(), sp.clone()).run(
        items(2),
        d.path(),
        &StubProbe(0),
        Some("m"),
        Some("r1"),
        0,
    );
    assert_eq!(sp.n(), 0);
    assert_eq!(res.fallback.len(), 2);
    assert!(res.fallback[0].1.contains("capacity_lost_before_spawn"));
}

#[test]
fn spawn_error_does_not_crash_dispatcher() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = Scripted::new(|_| Err("no such interpreter".to_string()));
    let mut c = cfg();
    c.max_retries = 0;
    let res = dispatcher(c, sp).run(items(1), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!(res.ok, 0);
    assert!(res.fallback[0].1.contains("spawn_error"));
}

#[test]
fn all_failed_falls_back_to_parent() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = Scripted::new(|req| {
        std::fs::write(req.log_path, "x".repeat(5000)).expect("log"); // not a load failure
        Ok(done(1))
    });
    let mut c = cfg();
    c.max_retries = 0;
    let res = dispatcher(c, sp).run(items(2), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!(res.ok, 0);
    assert_eq!(res.fallback.len(), 2);
    assert!(res
        .fallback
        .iter()
        .all(|(_, w)| w.starts_with("child_exit_1")));
    let note = summary_for_parent(&res, &[]);
    assert!(note.contains("do them yourself as originally planned"));
}

// ---------------- safety integration ----------------

#[test]
fn severe_violation_no_retry_and_fallback() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = Scripted::new(|req| {
        std::fs::write(
            req.cwd.join("audit.log"),
            format!("{}\n", json!({"verdict": "severe", "kind": "write", "target": "C:\\Windows\\x", "severity": "severe", "reason": "sensitive_path(\\windows)"})),
        )
        .expect("audit");
        Ok(done(53))
    });
    let mut c = cfg();
    c.max_retries = 2;
    let res =
        dispatcher(c, sp.clone()).run(items(1), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!(res.severe_violations, 1);
    assert_eq!(res.ok, 0);
    assert_eq!(sp.n(), 1); // severe is never retried
    assert!(res.fallback[0].1.contains("severe"));
    assert_eq!(res.children[0].exit_code, Some(53));
}

#[test]
fn audit_severe_found_while_running_kills() {
    let d = tempfile::tempdir().expect("tmp");
    let sp = Scripted::new(|req| {
        std::fs::write(
            req.cwd.join("audit.log"),
            format!("{}\n", json!({"verdict": "severe", "kind": "write", "target": "\\\\wsl$\\Ubuntu", "severity": "severe", "reason": "sensitive_path(\\\\wsl$)"})),
        )
        .expect("audit");
        Ok(Box::new(FakeProc {
            rc: None,
            killed: Arc::new(Mutex::new(false)),
        }) as Box<dyn ChildProcess>)
    });
    let mut c = cfg();
    c.per_task_timeout_s = 600.0;
    c.poll_interval_s = 0.1;
    c.audit_poll_interval_s = 0.2;
    c.max_retries = 0;
    let (disp, _) = fast_clock(dispatcher(c, sp));
    let res = disp.run(items(1), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert_eq!(res.severe_violations, 1);
    assert_eq!(res.children[0].status, ChildStatus::Severe);
}

#[test]
fn load_failure_aborts_whole_batch() {
    let d = tempfile::tempdir().expect("tmp");
    let spawns = Arc::new(AtomicUsize::new(0));
    let s = spawns.clone();
    let sp = Scripted::new(move |req| {
        s.fetch_add(1, Ordering::SeqCst);
        std::fs::write(
            req.log_path,
            "Error: model 'nope' not found, try pulling it first\n",
        )
        .expect("log");
        Ok(done(1))
    });
    let mut c = cfg();
    c.max_retries = 0;
    c.hard_cap = 1;
    let res = dispatcher(c, sp).run(items(3), d.path(), &StubProbe(2), Some("m"), Some("r1"), 0);
    assert!(
        res.fallback[0].1.contains("load_failure"),
        "{}",
        res.fallback[0].1
    );
    assert_eq!(spawns.load(Ordering::SeqCst), 1); // OOM / load failure → whole batch aborted
    assert_eq!(res.fallback.len(), 3);
}

// ---------------- integrator ----------------

fn ok_child(item_id: &str, child_dir: PathBuf, files: &[&str]) -> ChildTask {
    let mut ct = ChildTask::new(DispatchItem::new(item_id, "t"), 0);
    ct.status = ChildStatus::Ok;
    ct.child_dir = child_dir;
    ct.files = files.iter().map(|s| s.to_string()).collect();
    ct
}

#[test]
fn conflict_never_overwrites_by_default() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(
        d.path().join("todo.json"),
        r#"[{"item": "t", "done": false}]"#,
    )
    .expect("todo");
    std::fs::write(d.path().join("out.md"), "# original user file").expect("write");
    let child_dir = d.path().join("_dispatch/r1/task01");
    std::fs::create_dir_all(&child_dir).expect("mkdir");
    std::fs::write(child_dir.join("out.md"), "# child").expect("write");
    let mut res = DispatchResult {
        total: 1,
        ok: 1,
        children: vec![ok_child("1", child_dir, &["out.md"])],
        ..Default::default()
    };
    let c = cfg();
    let mut provider = JsonTodoProvider::new(d.path());
    let (integrated, conflicts, _) = ResultIntegrator::new(&c)
        .with_provider(&mut provider)
        .integrate(&mut res, d.path());
    assert!(integrated.is_empty());
    assert_eq!(conflicts, ["out.md"]);
    assert_eq!(
        std::fs::read_to_string(d.path().join("out.md")).expect("read"),
        "# original user file"
    );
    assert_eq!(res.children[0].status, ChildStatus::Failed);
    assert!(!provider.all_items().expect("items")[0].done);
}

#[test]
fn overwrite_allowed_when_configured() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("out.md"), "# original").expect("write");
    let child_dir = d.path().join("c");
    std::fs::create_dir_all(&child_dir).expect("mkdir");
    std::fs::write(child_dir.join("out.md"), "# child").expect("write");
    let mut res = DispatchResult {
        total: 1,
        ok: 1,
        children: vec![ok_child("1", child_dir, &["out.md"])],
        ..Default::default()
    };
    let mut c = cfg();
    c.allow_overwrite_existing = true;
    let (integrated, conflicts, _) = ResultIntegrator::new(&c).integrate(&mut res, d.path());
    assert_eq!(integrated, ["out.md"]);
    assert!(conflicts.is_empty());
    assert_eq!(
        std::fs::read_to_string(d.path().join("out.md")).expect("read"),
        "# child"
    );
}

#[test]
fn harness_files_not_copied_back() {
    let d = tempfile::tempdir().expect("tmp");
    let c = d.path().join("c");
    std::fs::create_dir_all(c.join("sub")).expect("mkdir");
    for f in [
        ".agent_state.json",
        "todo.json",
        "task.md",
        "agent.log",
        "audit.log",
        ".agent_state.meta.json",
    ] {
        std::fs::write(c.join(f), "x").expect("write");
    }
    std::fs::write(c.join("real.md"), "content").expect("write");
    std::fs::write(c.join("sub/nested.csv"), "a,b").expect("write");
    std::fs::write(c.join("empty.txt"), "").expect("write");
    assert_eq!(artifact_files(&c), ["real.md", "sub/nested.csv"]);
}

#[test]
fn verify_cmd_must_pass_blacklist() {
    let c = cfg();
    let integ = ResultIntegrator::new(&c);
    assert!(integ.verify_cmd_allowed("python -m pytest -q"));
    assert!(!integ.verify_cmd_allowed("wsl --unregister Ubuntu"));
    assert!(!integ.verify_cmd_allowed("rm -rf x"));
}

// ---------------- the message to the parent ----------------

#[test]
fn summary_lists_done_and_fallback() {
    let it_bad = DispatchItem::new("2", "convert b.csv to md");
    let mut ok = ChildTask::new(DispatchItem::new("1", "convert a.csv to md"), 0);
    ok.status = ChildStatus::Ok;
    ok.files = vec!["a.md".into()];
    let mut bad = ChildTask::new(it_bad.clone(), 1);
    bad.status = ChildStatus::Failed;
    bad.reason = "missing_artifacts(b.md)".into();
    let res = DispatchResult {
        total: 2,
        ok: 1,
        failed: 1,
        children: vec![ok, bad],
        fallback: vec![(it_bad, "missing_artifacts(b.md)".into())],
        ..Default::default()
    };
    let note = summary_for_parent(&res, &["a.md".to_string()]);
    assert!(note.contains("a.md") && note.contains("convert b.csv to md"));
    assert!(note.contains("do not redo them") && note.contains("do them yourself"));
    assert_eq!(summary_for_parent(&DispatchResult::default(), &[]), "");
}
