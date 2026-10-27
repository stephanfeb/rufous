//! Port of `tests/test_safety_net.py` (ring 4: overwrite guard, trash, `~` teach-back) and
//! `tests/test_crawl_guard.py` (read_file byte budget), plus executor plumbing.

use boubou_core::exec::{CrawlConfig, Executor, TRASH_DIR};
use boubou_core::safety::SafetyPolicy;
use serde_json::json;

fn ex() -> (tempfile::TempDir, Executor) {
    let d = tempfile::tempdir().expect("tmp");
    let e = Executor::with_policy(d.path(), SafetyPolicy::default());
    (d, e)
}

fn create(e: &mut Executor, path: &str, content: &str) -> String {
    e.run_tool("create_file", &json!({"path": path, "content": content}))
}

fn read(e: &mut Executor, path: &str) -> String {
    e.run_tool("read_file", &json!({"path": path}))
}

fn mk(dir: &std::path::Path, name: &str, n_lines: usize, pad: &str) {
    let mut body = String::new();
    for i in 1..=n_lines {
        body.push_str(&format!("L{i:03} {pad}\n"));
    }
    std::fs::write(dir.join(name), body).expect("write");
}

// ---- 1. overwrite guard ----

#[test]
fn clobber_blocked_and_replace_escapes() {
    let (d, mut e) = ex();
    let big = format!("# 手册\n{}", "正文段落,内容充足。\n".repeat(60));
    assert!(create(&mut e, "doc.md", &big).contains("created"));
    let stub = "## 只有第二节\n一点尾巴";
    let r2 = create(&mut e, "doc.md", stub);
    assert!(r2.contains("覆盖防护") && r2.contains("replace=true"));
    assert_eq!(
        std::fs::read_to_string(d.path().join("doc.md")).expect("read"),
        big
    );
    let r3 = e.run_tool(
        "create_file",
        &json!({"path": "doc.md", "content": stub, "replace": true}),
    );
    assert!(r3.contains("created"));
    assert_eq!(
        std::fs::read_to_string(d.path().join("doc.md")).expect("read"),
        stub
    );
}

#[test]
fn grow_and_new_file_not_blocked() {
    let (_d, mut e) = ex();
    assert!(create(&mut e, "new.txt", "hello world").contains("created"));
    assert!(create(&mut e, "new.txt", &"hello world, 更长的续写".repeat(20)).contains("created"));
    create(&mut e, "tiny.txt", &"a".repeat(100));
    assert!(create(&mut e, "tiny.txt", &"b".repeat(80)).contains("created")); // under 300 B
}

#[test]
fn clobber_regression_wf10_shape() {
    let (d, mut e) = ex();
    let manual = format!(
        "# TimeTrack CLI User Manual\n\n## 1. Introduction\n\n{}",
        "line of manual text.\n".repeat(80)
    );
    create(&mut e, "user_manual.md", &manual);
    let r = create(
        &mut e,
        "user_manual.md",
        "## 2. Real Captured Terminal Outputs\n\n```text\nsome output\n```",
    );
    assert!(r.contains("覆盖防护"));
    assert!(std::fs::read_to_string(d.path().join("user_manual.md"))
        .expect("read")
        .contains("Introduction"));
}

// ---- 2. delete_file → trash ----

#[test]
fn delete_moves_to_trash() {
    let (d, mut e) = ex();
    std::fs::write(d.path().join("precious.txt"), "重要内容").expect("write");
    let r = e.run_tool("delete_file", &json!({"path": "precious.txt"}));
    assert!(r.contains("mingbird_trash"));
    assert!(!d.path().join("precious.txt").exists());
    let trashed: Vec<_> = std::fs::read_dir(d.path().join(TRASH_DIR))
        .expect("trash")
        .flatten()
        .collect();
    assert_eq!(trashed.len(), 1);
    let name = trashed[0].file_name().to_string_lossy().into_owned();
    assert!(name.starts_with("precious.txt"));
    assert!(std::fs::read_to_string(trashed[0].path())
        .expect("read")
        .contains("重要内容"));
    assert!(e
        .run_tool("delete_file", &json!({"path": "ghost.txt"}))
        .contains("not found"));
}

// ---- 5. `~` teach-back ----

#[test]
fn tilde_path_teachback() {
    let (d, mut e) = ex();
    let r = create(&mut e, "~/.timetrack/entries.json", "[]");
    assert!(r.contains("安全垫") && r.contains("相对路径"));
    assert!(!d.path().join("~").exists());
}

// ---- edit_file keeps a .bak ----

#[test]
fn edit_file_backs_up_and_reports_misses() {
    let (d, mut e) = ex();
    create(&mut e, "a.py", "x = 1\ny = 2\n");
    let r = e.run_tool(
        "edit_file",
        &json!({"path": "a.py", "old": "y = 2", "new": "y = 3"}),
    );
    assert!(r.starts_with("[edited a.py") && r.contains(".bak"));
    assert_eq!(
        std::fs::read_to_string(d.path().join("a.py.bak")).expect("bak"),
        "x = 1\ny = 2\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("a.py")).expect("new"),
        "x = 1\ny = 3\n"
    );
    let miss = e.run_tool(
        "edit_file",
        &json!({"path": "a.py", "old": "nope", "new": "z"}),
    );
    assert!(miss.starts_with("[edit failed: old text not found."));
}

// ---- crawl guard v2 ----

#[test]
fn within_budget_returns_requested_range() {
    let (d, mut e) = ex();
    mk(d.path(), "data.txt", 100, "");
    let r1 = e.run_tool(
        "read_file",
        &json!({"path": "data.txt", "start_line": 10, "end_line": 12}),
    );
    assert!(r1.starts_with("L010") && r1.contains("L012") && !r1.contains("L013"));
    assert!(!r1.contains("crawl-guard") && !r1.contains("[提示"));
    let r2 = e.run_tool(
        "read_file",
        &json!({"path": "data.txt", "start_line": 50, "end_line": 60}),
    );
    assert!(r2.starts_with("L050") && !r2.contains("crawl-guard"));
    mk(d.path(), "small.txt", 20, "");
    let r = read(&mut e, "small.txt");
    assert!(r.starts_with("L001") && r.contains("L020"));
}

#[test]
fn nudge_at_4th_and_10th_only() {
    let (d, mut e) = ex();
    mk(d.path(), "n.txt", 30, "");
    let results: Vec<String> = (0..11).map(|_| read(&mut e, "n.txt")).collect();
    let nudged: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| r.contains("[提示"))
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(nudged, vec![4, 10]);
    assert!(results[3].starts_with("[提示"));
    assert!(results[3].contains("L001") && results[3].contains("L030"));
    assert!(!results[3].contains("crawl-guard"));
}

#[test]
fn throttle_respects_requested_range() {
    let (d, mut e) = ex();
    e.crawl_cfg = CrawlConfig {
        budget_mult: 1,
        budget_floor: 1024,
        ..CrawlConfig::default()
    };
    mk(d.path(), "big.txt", 200, &"x".repeat(40));
    let r1 = read(&mut e, "big.txt");
    assert!(!r1.contains("crawl-guard"));
    let r2 = read(&mut e, "big.txt");
    assert!(r2.contains("读取不会被拒绝"));
    assert!(r2.contains("run_bash") && r2.contains("python"));
    let r3 = e.run_tool(
        "read_file",
        &json!({"path": "big.txt", "start_line": 150, "end_line": 200}),
    );
    assert!(r3.contains("限速中"));
    assert!(r3.lines().nth(1).expect("body").starts_with("L150"));
    assert!(!r3.split_once("限速中").expect("note").1.contains("L001"));
    let body3 = r3.split_once("]\n").expect("note").1;
    assert!(
        body3
            .split("\n[文件共")
            .next()
            .expect("body")
            .chars()
            .count()
            <= e.crawl_cfg.throttle
    );
}

#[test]
fn throttle_never_denies_outright() {
    let (d, mut e) = ex();
    e.crawl_cfg = CrawlConfig {
        budget_floor: 200,
        ..CrawlConfig::default()
    };
    mk(d.path(), "d.txt", 60, &"y".repeat(30));
    for _ in 0..8 {
        let r = read(&mut e, "d.txt");
        assert!(r.contains("L001") || r.contains("L0"));
    }
}

#[test]
fn small_file_many_reads_still_full() {
    let (d, mut e) = ex();
    std::fs::write(
        d.path().join("app.py"),
        "def run():\n    return 42\n".repeat(20),
    )
    .expect("write");
    for _ in 0..15 {
        let r = read(&mut e, "app.py");
        assert!(r.contains("def run():") && !r.contains("crawl-guard"));
    }
}

#[test]
fn state_isolation_between_executors() {
    let d = tempfile::tempdir().expect("tmp");
    mk(d.path(), "s.txt", 100, &"z".repeat(30));
    let cfg = CrawlConfig {
        budget_floor: 1024,
        ..CrawlConfig::default()
    };
    let mut e1 = Executor::with_policy(d.path(), SafetyPolicy::default());
    e1.crawl_cfg = cfg;
    for _ in 0..4 {
        read(&mut e1, "s.txt");
    }
    let mut e2 = Executor::with_policy(d.path(), SafetyPolicy::default());
    e2.crawl_cfg = cfg;
    assert!(!read(&mut e2, "s.txt").contains("crawl-guard"));
}

#[test]
fn batch_tools_shares_crawl_accounting() {
    let (d, mut e) = ex();
    mk(d.path(), "b.txt", 30, "");
    let out = e.run_tool(
        "batch_tools",
        &json!({"calls": [{"tool": "read_file", "args": {"path": "b.txt"}}]}),
    );
    assert_eq!(e.crawl_entry("b.txt").expect("counted").reads, 1);
    assert!(out.contains("L001"));
    assert!(e
        .run_tool(
            "batch_tools",
            &json!({"calls": [{"tool": "batch_tools", "args": {}}]})
        )
        .contains("禁止嵌套"));
}

#[test]
fn images_exempt_from_crawl_guard() {
    let (d, mut e) = ex();
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(std::iter::repeat_n(b'0', 64));
    std::fs::write(d.path().join("fig.png"), png).expect("write");
    for _ in 0..8 {
        let r = read(&mut e, "fig.png");
        assert!(
            r.contains("[image attached")
                || r.contains("[image queued")
                || r.contains("[image too large")
        );
    }
    assert!(e.crawl_entry("fig.png").is_none());
}

// ---- plumbing ----

#[test]
fn aliases_coercion_and_missing_args() {
    let (_d, mut e) = ex();
    assert!(e
        .run_tool("write_file", &json!({"path": "x.txt", "content": "hi"}))
        .contains("created x.txt"));
    assert!(e
        .run_tool("cat", &json!({"path": "x.txt"}))
        .starts_with("hi"));
    let r = e.run_tool("read_file", &json!({"path": "x.txt", "start_line": "abc"}));
    assert!(r.contains("需要 number 类型"));
    let r = e.run_tool("create_file", &json!({"content": "hi"}));
    assert!(r.contains("缺少参数 'path'") && r.contains("正确示例"));
    assert_eq!(e.run_tool("teleport", &json!({})), "[unknown tool]");
    assert!(e
        .run_tool("finish", &json!({"summary": "ok"}))
        .starts_with("[TASK_COMPLETE] ok"));
}

#[test]
fn run_bash_executes_and_formats() {
    let (_d, mut e) = ex();
    let out = e.run_tool("run_bash", &json!({"command": "echo hello"}));
    assert!(out.starts_with("[exit 0]\nSTDOUT:\nhello"));
    let out = e.run_tool("run_bash", &json!({"command": "exit 3"}));
    assert!(out.starts_with("[exit 3]"));
}

#[test]
fn run_bash_timeout_kills_and_reports() {
    let (_d, mut e) = ex();
    e.bash_timeout = std::time::Duration::from_millis(300);
    let started = std::time::Instant::now();
    let out = e.run_tool("run_bash", &json!({"command": "sleep 5"}));
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert!(out.contains("timed out"));
}

#[test]
fn enable_tools_receipts() {
    let (_d, mut e) = ex();
    let r = e.run_tool("enable_tools", &json!({"tools": ["web_fetch", "nonsense"]}));
    assert!(r.starts_with("[enabled tools: web_fetch."));
    assert!(r.contains("未知的工具名") && r.contains("nonsense"));
    let r = e.run_tool("enable_tools", &json!({"tools": ["web_fetch"]}));
    assert!(r.contains("无需再启用"));
}
