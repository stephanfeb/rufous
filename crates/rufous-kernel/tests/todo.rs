//! Port of `tests/test_todo.py`: the todo tool itself, panel text and progress.

use rufous_kernel::exec::Executor;
use rufous_kernel::safety::SafetyPolicy;
use rufous_kernel::todo::{format_todo_lines, load_todo, todo_file, todo_progress, TodoItem};
use serde_json::json;

fn ex() -> (tempfile::TempDir, Executor) {
    let d = tempfile::tempdir().expect("tmp");
    let e = Executor::with_policy(d.path(), SafetyPolicy::default());
    (d, e)
}

#[test]
fn create() {
    let (d, mut e) = ex();
    let out = e.run_tool(
        "todo",
        &json!({"action": "create", "items": ["step 1", "step 2"]}),
    );
    assert!(out.contains("[ ] 1. step 1") && out.contains("[ ] 2. step 2"));
    assert!(todo_file(d.path()).exists());
}

#[test]
fn update_marks_done_single_list_all() {
    let (_d, mut e) = ex();
    e.run_tool(
        "todo",
        &json!({"action": "create", "items": ["a", "b", "c"]}),
    );
    let out = e.run_tool("todo", &json!({"action": "update", "index": 1}));
    assert!(out.contains("[x] 1. a") && out.contains("[ ] 2. b"));
    let out = e.run_tool("todo", &json!({"action": "update", "index": [1, 3]}));
    assert!(out.contains("[x] 1. a") && out.contains("[ ] 2. b") && out.contains("[x] 3. c"));
    let out = e.run_tool("todo", &json!({"action": "update", "all": true}));
    assert!(!out.contains("[ ]") && out.matches("[x]").count() == 3);
    let out = e.run_tool(
        "todo",
        &json!({"action": "update", "index": 2, "done": false}),
    );
    assert!(out.contains("[ ] 2. b"));
}

#[test]
fn update_errors_are_actionable() {
    let (_d, mut e) = ex();
    let out = e.run_tool("todo", &json!({"action": "update", "index": 1}));
    assert!(out.contains("todo(action=create"));
    e.run_tool("todo", &json!({"action": "create", "items": ["a", "b"]}));
    let out = e.run_tool("todo", &json!({"action": "update", "index": 9}));
    assert!(out.contains("index") && out.contains("all=true"));
}

#[test]
fn list_and_persisted_progress() {
    let (d, mut e) = ex();
    e.run_tool("todo", &json!({"action": "create", "items": ["only"]}));
    assert!(e
        .run_tool("todo", &json!({"action": "list"}))
        .contains("[ ] 1. only"));
    e.run_tool("todo", &json!({"action": "create", "items": ["a", "b"]}));
    e.run_tool("todo", &json!({"action": "update", "index": 1}));
    assert_eq!(todo_progress(&load_todo(d.path())), (1, 2));
}

#[test]
fn lenient_item_shapes() {
    let (_d, mut e) = ex();
    let out = e.run_tool(
        "todo",
        &json!({"action": "create", "plan": "1. read\n2. write\n- verify"}),
    );
    assert!(
        out.contains("[ ] 1. read")
            && out.contains("[ ] 2. write")
            && out.contains("[ ] 3. verify")
    );
    let out = e.run_tool("todo", &json!({"action": "create", "items": []}));
    assert!(out.contains("no items found"));
}

#[test]
fn format_and_progress() {
    let t = vec![
        TodoItem {
            item: "a".into(),
            done: true,
        },
        TodoItem {
            item: "b".into(),
            done: false,
        },
    ];
    assert_eq!(format_todo_lines(&t), "[x] 1. a\n[ ] 2. b");
    let t3 = vec![
        TodoItem {
            item: "a".into(),
            done: true,
        },
        TodoItem {
            item: "b".into(),
            done: true,
        },
        TodoItem {
            item: "c".into(),
            done: false,
        },
    ];
    assert_eq!(todo_progress(&t3), (2, 3));
    assert_eq!(todo_progress(&[]), (0, 0));
    assert_eq!(format_todo_lines(&[TodoItem::default()]), "[ ] 1. ");
    // Mingbird's loader tolerates items with missing keys
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(todo_file(d.path()), "[{}]").expect("write");
    assert_eq!(load_todo(d.path()), vec![TodoItem::default()]);
}

#[test]
fn the_first_todo_call_of_a_session_creates_the_relocated_harness_dir() {
    // The ACP server points the plan at `<sessions>/<name>.files/`, which does not exist
    // until the harness writes there; the plan must be able to be that first write.
    let (_d, mut e) = ex();
    let h = tempfile::tempdir().expect("tmp");
    let hdir = h.path().join("acp_1030.files");
    e.harness_dir = hdir.clone();
    assert!(!hdir.exists());
    let out = e.run_tool(
        "todo",
        &json!({"action": "create", "items": ["write the block store", "wire bitswap"]}),
    );
    assert!(!out.contains("tool error"), "{out}");
    assert!(todo_file(&hdir).is_file(), "{out}");
    assert_eq!(load_todo(&hdir).len(), 2);
}
