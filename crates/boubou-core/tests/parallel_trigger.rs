//! Port of `tests/test_parallel_trigger.py`: the todo provider, the classifier, the safety
//! precheck and the all-AND trigger.

use std::collections::HashMap;
use std::path::Path;

use boubou_core::parallel::{
    classify_task, safety_precheck, should_dispatch, CapacityPlan, CapacityPlanner, DispatchItem,
    JsonTodoProvider, ParallelConfig, TodoProvider,
};
use serde_json::{json, Value};

fn cfg() -> ParallelConfig {
    ParallelConfig::default()
}

fn on() -> ParallelConfig {
    let mut c = cfg();
    c.enabled = true;
    c.child_model = "small:2b".into();
    c
}

fn write_todo(dir: &Path, items: &[(&str, bool)]) {
    let v: Vec<Value> = items
        .iter()
        .map(|(t, d)| json!({"item": t, "done": d}))
        .collect();
    std::fs::write(
        dir.join("todo.json"),
        serde_json::to_string(&v).expect("json"),
    )
    .expect("write");
}

struct Stub {
    mp: usize,
    reasons: Vec<String>,
}

impl CapacityPlanner for Stub {
    fn plan_capacity(&self, _m: &str, _c: &ParallelConfig) -> CapacityPlan {
        CapacityPlan {
            max_parallel: self.mp,
            reasons: self.reasons.clone(),
            ..Default::default()
        }
    }
}

fn stub(mp: usize) -> Stub {
    Stub {
        mp,
        reasons: vec![],
    }
}

const SIMPLE: &str = "把 data.csv 转成 markdown 表格";

fn six(dir: &Path) -> JsonTodoProvider {
    write_todo(dir, &[(SIMPLE, false); 6]);
    JsonTodoProvider::new(dir)
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---------------- provider ----------------

#[test]
fn provider_reads_current_format_and_tolerates_variants() {
    let d = tempfile::tempdir().expect("tmp");
    write_todo(d.path(), &[("a", false), ("b", true)]);
    let p = JsonTodoProvider::new(d.path());
    assert_eq!(p.all_items().expect("items").len(), 2);
    let pending: Vec<String> = p
        .pending_items()
        .expect("items")
        .into_iter()
        .map(|i| i.text)
        .collect();
    assert_eq!(pending, ["a"]);

    std::fs::write(
        d.path().join("todo.json"),
        r#"{"items": [{"text": "x", "done": false}, "bare string", {"title": "t", "complexity": "simple"}]}"#,
    )
    .expect("write");
    let items = JsonTodoProvider::new(d.path()).all_items().expect("items");
    let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts, ["x", "bare string", "t"]);
    assert_eq!(items[2].complexity, "simple");
    assert_eq!(items[2].id, "3");
}

#[test]
fn mark_done_idempotent_and_no_reorder() {
    let d = tempfile::tempdir().expect("tmp");
    write_todo(d.path(), &[("a", false), ("b", false), ("c", false)]);
    let mut p = JsonTodoProvider::new(d.path());
    assert!(p.mark_done("2", "done-by-subagent"));
    assert!(p.mark_done("2", ""));
    let items = p.all_items().expect("items");
    let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts, ["a", "b", "c"]);
    let done: Vec<bool> = items.iter().map(|i| i.done).collect();
    assert_eq!(done, [false, true, false]);
    assert!(!p.mark_done("9", ""));
    // The loop's own loader still reads the file.
    let loaded = boubou_core::todo::load_todo(d.path());
    assert_eq!(loaded.len(), 3);
    assert!(loaded[1].done);
}

#[test]
fn missing_file_is_empty_not_error() {
    let d = tempfile::tempdir().expect("tmp");
    assert!(JsonTodoProvider::new(d.path())
        .all_items()
        .expect("items")
        .is_empty());
}

// ---------------- classifier ----------------

#[test]
fn classifier_positive_and_vetoes() {
    assert!(classify_task("把 data.csv 里的列名翻译成中文并另存", &cfg()).dispatchable);
    for t in [
        "删除 tmp.log 并统计行数",
        "覆盖 config.json 里的字段",
        "把 result.md 上传到服务器",
        "重构 project.py 的所有模块结构",
        "pip install pandas 后生成 out.csv",
        "运行 test_report.py 验证输出",
        "为 report.md 设计一个更好的结构",
        "[simple] 整理所有文件",
        "[complex] 把 a.csv 转成 b.md",
        "转换 main.py,这一步很关键,别搞错",
        "",
    ] {
        let r = classify_task(t, &cfg());
        assert!(!r.dispatchable, "{t}");
        assert!(!r.reason.is_empty(), "{t}");
    }
}

#[test]
fn multi_file_not_dispatchable_but_annotation_escapes_mechanical_rule() {
    let r = classify_task("把 a.csv 和 b.csv 都转成 markdown", &cfg());
    assert!(!r.dispatchable);
    assert!(r.reason.contains("files="), "{}", r.reason);
    let r = classify_task("[simple] 为 out.md 补一个标题行", &cfg());
    assert!(r.dispatchable);
    assert_eq!(r.file_tokens, ["out.md"]);
}

#[test]
fn safety_precheck_second_gate() {
    let e = safety_precheck("清理 wsl --unregister Ubuntu 的残留", &cfg()).expect_err("blocked");
    assert!(e.contains("severe_command"), "{e}");
    let e = safety_precheck("把结果写到 .env 里", &cfg()).expect_err("blocked");
    assert!(e.contains("sensitive_in_task_text"), "{e}");
    assert!(safety_precheck(SIMPLE, &cfg()).is_ok());
}

// ---------------- the trigger (all AND) ----------------

#[test]
fn disabled_by_default() {
    let d = tempfile::tempdir().expect("tmp");
    let plan = should_dispatch(&stub(2), &six(d.path()), &cfg(), &env(&[]), None);
    assert!(!plan.should);
    assert!(plan.reasons[0].contains("disabled"));
}

#[test]
fn hard_kill_switch_depth_and_model_gates() {
    let d = tempfile::tempdir().expect("tmp");
    let p = six(d.path());
    let plan = should_dispatch(&stub(2), &p, &on(), &env(&[("AGENT_PARALLEL", "0")]), None);
    assert!(!plan.should && plan.reasons[0].contains("hard_kill_switch"));
    let plan = should_dispatch(&stub(2), &p, &on(), &env(&[("MINGBIRD_DEPTH", "1")]), None);
    assert!(!plan.should && plan.reasons[0].contains("depth_limit"));
    let mut c = cfg();
    c.enabled = true;
    let plan = should_dispatch(&stub(2), &p, &c, &env(&[]), None);
    assert!(!plan.should && plan.reasons[0].contains("no_child_model"));
}

#[test]
fn too_few_pending_or_dispatchable() {
    let d = tempfile::tempdir().expect("tmp");
    write_todo(d.path(), &[(SIMPLE, false); 3]);
    let plan = should_dispatch(
        &stub(2),
        &JsonTodoProvider::new(d.path()),
        &on(),
        &env(&[]),
        None,
    );
    assert!(!plan.should && plan.reasons[0].contains("too_few_pending"));

    let mut items = vec![(SIMPLE, false)];
    let others: Vec<String> = (0..5).map(|i| format!("重构模块{i}")).collect();
    items.extend(others.iter().map(|s| (s.as_str(), false)));
    write_todo(d.path(), &items);
    let plan = should_dispatch(
        &stub(2),
        &JsonTodoProvider::new(d.path()),
        &on(),
        &env(&[]),
        None,
    );
    assert!(!plan.should && plan.reasons[0].contains("too_few_dispatchable"));
}

#[test]
fn no_capacity_blocks_and_happy_path_passes() {
    let d = tempfile::tempdir().expect("tmp");
    let p = six(d.path());
    let probe = Stub {
        mp: 0,
        reasons: vec!["no headroom".into()],
    };
    let plan = should_dispatch(&probe, &p, &on(), &env(&[]), None);
    assert!(!plan.should && plan.reasons[0].contains("no_capacity"));

    let plan = should_dispatch(&stub(2), &p, &on(), &env(&[]), None);
    assert!(plan.should);
    assert_eq!(plan.n_items(), 6);
    assert!(plan.items.iter().all(|i| !i.done));
    assert!(plan.reasons[0].starts_with("trigger_ok("));
}

#[test]
fn done_items_not_dispatched_and_skipped_carry_reason() {
    let d = tempfile::tempdir().expect("tmp");
    write_todo(d.path(), &[(SIMPLE, true); 6]);
    assert!(
        !should_dispatch(
            &stub(2),
            &JsonTodoProvider::new(d.path()),
            &on(),
            &env(&[]),
            None
        )
        .should
    );

    let mut c = on();
    c.min_dispatchable = 1;
    c.min_pending_items = 3;
    write_todo(
        d.path(),
        &[
            (SIMPLE, false),
            ("删除 old.csv", false),
            ("重构 core.py", false),
        ],
    );
    let plan = should_dispatch(
        &stub(2),
        &JsonTodoProvider::new(d.path()),
        &c,
        &env(&[]),
        None,
    );
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.skipped.len(), 2);
    assert!(plan.skipped.iter().all(|(_, r)| !r.is_empty()));
}

#[test]
fn provider_error_is_caught() {
    struct Boom;
    impl TodoProvider for Boom {
        fn all_items(&self) -> Result<Vec<DispatchItem>, String> {
            Err("disk full".into())
        }
        fn mark_done(&mut self, _: &str, _: &str) -> bool {
            false
        }
    }
    let plan = should_dispatch(&stub(2), &Boom, &on(), &env(&[]), None);
    assert!(!plan.should && plan.reasons[0].contains("todo_provider_error"));
}
