//! Port of `tests/test_safety_net.py` (ring 4: overwrite guard, trash, `~` teach-back) and
//! `tests/test_crawl_guard.py` (read_file byte budget), plus executor plumbing.

use rufous_kernel::exec::{rebase_workdir_prefix, rebased_path, CrawlConfig, Executor, TRASH_DIR};
use rufous_kernel::safety::SafetyPolicy;
use serde_json::json;

fn ex() -> (tempfile::TempDir, Executor) {
    let d = tempfile::tempdir().expect("tmp");
    let e = Executor::with_policy(d.path(), SafetyPolicy::default());
    (d, e)
}

#[test]
fn a_raised_stop_flag_terminates_a_running_command() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let (_d, mut e) = ex();
    let stop = Arc::new(AtomicBool::new(false));
    e.stop = Some(stop.clone());
    let raiser = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            stop.store(true, Ordering::SeqCst);
        })
    };
    let t0 = Instant::now();
    // run_bash runs `cmd /C` on Windows, where `;` is not a separator (bead boubou-tfr).
    let command = if cfg!(windows) {
        "echo started & sleep 30 & echo late"
    } else {
        "echo started; sleep 30; echo late"
    };
    let out = e.run_tool("run_bash", &json!({ "command": command }));
    raiser.join().expect("raiser");
    assert!(t0.elapsed() < Duration::from_secs(10), "{out}");
    assert!(out.contains("started"), "{out}");
    assert!(!out.contains("late"), "{out}");
    assert!(out.contains("stopped by the user"), "{out}");
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
    let big = format!(
        "# Manual\n{}",
        "A body paragraph with plenty of content.\n".repeat(60)
    );
    assert!(create(&mut e, "doc.md", &big).contains("created"));
    let stub = "## Only the second section\nA short tail";
    let r2 = create(&mut e, "doc.md", stub);
    assert!(r2.contains("overwrite guard") && r2.contains("replace=true"));
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
    assert!(create(
        &mut e,
        "new.txt",
        &"hello world, a longer continuation".repeat(20)
    )
    .contains("created"));
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
    assert!(r.contains("overwrite guard"));
    assert!(std::fs::read_to_string(d.path().join("user_manual.md"))
        .expect("read")
        .contains("Introduction"));
}

// ---- 2. delete_file → trash ----

#[test]
fn delete_moves_to_trash() {
    let (d, mut e) = ex();
    std::fs::write(d.path().join("precious.txt"), "important content").expect("write");
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
        .contains("important content"));
    assert!(e
        .run_tool("delete_file", &json!({"path": "ghost.txt"}))
        .contains("not found"));
}

// ---- 5. `~` teach-back ----

#[test]
fn tilde_path_teachback() {
    let (d, mut e) = ex();
    let r = create(&mut e, "~/.timetrack/entries.json", "[]");
    assert!(r.contains("safety net") && r.contains("relative path"));
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

// ---- edit_file's matcher (bead boubou-bk9.3) ----

fn edit(e: &mut Executor, path: &str, old: &str, new: &str) -> String {
    e.run_tool("edit_file", &json!({"path": path, "old": old, "new": new}))
}

#[test]
fn a_source_escape_is_edited_as_written_and_a_quoted_newline_is_unescaped() {
    // ED-06's shape: the file has the two characters `\n` inside a string literal, the model
    // quotes them as they are, and the edit must not turn them into a real newline.
    let (d, mut e) = ex();
    let src = "def f(cells):\n    return \",\".join(cells) + \"\\n\"\n";
    create(&mut e, "export.py", src);
    let r = edit(
        &mut e,
        "export.py",
        "return \",\".join(cells) + \"\\n\"",
        "return \"\\t\".join(cells) + \"\\n\"",
    );
    assert!(
        r.starts_with("[edited export.py") && !r.contains("matched"),
        "{r}"
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("export.py")).expect("read"),
        "def f(cells):\n    return \"\\t\".join(cells) + \"\\n\"\n"
    );
    // A real newline quoted as backslash-n: the unescape tier, applied to new as well.
    create(&mut e, "two.py", "a = 1\nb = 2\n");
    let r = edit(&mut e, "two.py", "a = 1\\nb = 2", "a = 1\\nb = 3");
    assert!(
        r.starts_with("[edited two.py") && r.contains("escape sequences unescaped"),
        "{r}"
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("two.py")).expect("read"),
        "a = 1\nb = 3\n"
    );
}

#[test]
fn a_second_occurrence_is_refused_with_the_count_until_all_is_passed() {
    let (d, mut e) = ex();
    create(
        &mut e,
        "store.py",
        "def a():\n    return fetchAll(c)\n\ndef b():\n    return fetchAll(c)\n",
    );
    let r = edit(&mut e, "store.py", "fetchAll", "fetch_all");
    assert!(
        r.starts_with("[edit failed: old text found 2 times in store.py;")
            && !r.contains("all=true")
            && r.contains("Lines 1–3 read:\ndef a():\n    return fetchAll(c)\n\n")
            && r.contains("Lines 4–5 read:\ndef b():\n    return fetchAll(c)\n")
            && r.contains("Quote one of these regions as old"),
        "{r}"
    );
    assert!(std::fs::read_to_string(d.path().join("store.py"))
        .expect("read")
        .contains("fetchAll"));
    let r = e.run_tool(
        "edit_file",
        &json!({"path": "store.py", "old": "fetchAll", "new": "fetch_all", "all": "true"}),
    );
    assert!(
        r.starts_with("[edited store.py") && r.contains("(2 occurrences replaced)"),
        "{r}"
    );
    assert!(!std::fs::read_to_string(d.path().join("store.py"))
        .expect("read")
        .contains("fetchAll"));
}

#[test]
fn all_is_accepted_but_not_advertised() {
    // A small model fills every boolean with true and took "or pass all=true" as the fix
    // (ED-06 coding r2, 2026-10-31): the argument is honoured when sent, never offered.
    let edit = rufous_kernel::tools::core_tools()
        .into_iter()
        .find(|t| t["function"]["name"] == "edit_file")
        .expect("edit_file spec");
    assert!(edit["function"]["parameters"]["properties"]
        .get("all")
        .is_none());
    assert!(!edit["function"]["description"]
        .as_str()
        .unwrap_or("")
        .contains("all"));
}

#[test]
fn an_edit_already_in_the_file_is_no_change_and_writes_nothing() {
    let (d, mut e) = ex();
    create(&mut e, "repo.py", "def fetch_all(conn):\n    pass\n");
    let r = edit(
        &mut e,
        "repo.py",
        "def fetchAll(conn):",
        "def fetch_all(conn):",
    );
    assert!(
        r.starts_with("[no change: this edit is already done. repo.py lines 1–1 now read:\ndef fetch_all(conn):\n"),
        "{r}"
    );
    assert!(!d.path().join("repo.py.bak").exists());
    assert_eq!(
        std::fs::read_to_string(d.path().join("repo.py")).expect("read"),
        "def fetch_all(conn):\n    pass\n"
    );
}

#[test]
fn a_miss_shows_the_closest_lines_with_their_numbers_instead_of_the_file_head() {
    let (_d, mut e) = ex();
    create(
        &mut e,
        "m.py",
        "import os\n\ndef f(x):\n    retrun x + 1\n\ndef g():\n    pass\n",
    );
    let r = edit(&mut e, "m.py", "    return x + 1", "    return x + 2");
    assert!(r.starts_with("[edit failed: old text not found. Closest match in m.py, lines 4–4:\n    retrun x + 1\n"), "{r}");
    assert!(!r.contains("Preview of the file start"));
}

#[test]
fn trailing_whitespace_unicode_and_typographic_differences_are_tiers_that_say_so() {
    let (d, mut e) = ex();
    create(&mut e, "t.py", "def f():\n    x = 1   \n    return x\n");
    let r = edit(
        &mut e,
        "t.py",
        "    x = 1\n    return x\n",
        "    return 1\n",
    );
    assert!(r.contains("matched ignoring trailing whitespace"), "{r}");
    assert_eq!(
        std::fs::read_to_string(d.path().join("t.py")).expect("read"),
        "def f():\n    return 1\n"
    );
    create(&mut e, "u.py", "\u{fb01}le = 1\n");
    assert!(edit(&mut e, "u.py", "file = 1", "file = 2").contains("after Unicode normalisation"));
    create(&mut e, "q.py", "s = \u{201c}hi\u{201d}\n");
    assert!(edit(&mut e, "q.py", "s = \"hi\"", "s = \"yo\"").contains("typographic quotes"));
    assert_eq!(
        std::fs::read_to_string(d.path().join("q.py")).expect("read"),
        "s = \"yo\"\n"
    );
}

#[test]
fn with_the_matcher_off_mingbirds_behaviour_is_back() {
    let (d, mut e) = ex();
    e.edit_match = false;
    create(&mut e, "export.py", "return \",\".join(cells) + \"\\n\"\n");
    let r = edit(
        &mut e,
        "export.py",
        "return \",\".join(cells) + \"\\n\"",
        "x",
    );
    assert!(
        r.starts_with("[edit failed: old text not found. Preview of the file start:"),
        "{r}"
    );
    create(&mut e, "two.py", "a\na\n");
    assert!(edit(&mut e, "two.py", "a", "b").starts_with("[edited two.py"));
    assert_eq!(
        std::fs::read_to_string(d.path().join("two.py")).expect("read"),
        "b\na\n"
    );
}

#[test]
fn string_encoded_lists_and_lone_objects_are_repaired_before_validation() {
    let (_d, mut e) = ex();
    let r = e.run_tool(
        "todo",
        &json!({"action": "create", "items": "[\"read it\", \"fix it\"]"}),
    );
    assert!(r.contains("1. read it") && r.contains("2. fix it"), "{r}");
    let r = e.run_tool("enable_tools", &json!({"tools": "[\"search_files\"]"}));
    assert!(!r.starts_with("[tool error"), "{r}");
    // A string that is not JSON is left for the validator (todo has its own newline-split
    // reading of items, so the case is enable_tools).
    let r = e.run_tool("enable_tools", &json!({"tools": "just text"}));
    assert!(r.starts_with("[tool error"), "{r}");
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
    assert!(!r1.contains("crawl-guard") && !r1.contains("[hint"));
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
        .filter(|(_, r)| r.contains("[hint"))
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(nudged, vec![4, 10]);
    assert!(results[3].starts_with("[hint"));
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
    assert!(r2.contains("Reads are not refused"));
    assert!(r2.contains("run_bash") && r2.contains("python"));
    let r3 = e.run_tool(
        "read_file",
        &json!({"path": "big.txt", "start_line": 150, "end_line": 200}),
    );
    assert!(r3.contains("rate-limited"));
    assert!(r3.lines().nth(1).expect("body").starts_with("L150"));
    assert!(!r3
        .split_once("rate-limited")
        .expect("note")
        .1
        .contains("L001"));
    let body3 = r3.split_once("]\n").expect("note").1;
    assert!(
        body3
            .split("\n[file has")
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
        .contains("nested batch_tools"));
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
fn other_harnesses_shell_names_map_to_run_bash_and_leaked_names_are_malformed() {
    use rufous_kernel::tools::{is_well_formed_tool_name, resolve_alias};
    for n in [
        "run_shell_command",
        "run_command",
        "execute_command",
        "run_terminal_cmd",
        "shell",
        "bash",
    ] {
        assert_eq!(resolve_alias(n), "run_bash", "{n}");
    }
    assert_eq!(resolve_alias("run_bash"), "run_bash");
    for ok in ["run_bash", "fs.read_file", "web-search", "Tool2"] {
        assert!(is_well_formed_tool_name(ok), "{ok}");
    }
    for bad in [
        "",
        "bd show initial-filesharing-app-setup/1-1-dependencies\n</parameter",
        "read file",
        "<tool_call>",
    ] {
        assert!(!is_well_formed_tool_name(bad), "{bad:?}");
    }
}

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
    assert!(r.contains("must be of type number"));
    let r = e.run_tool("create_file", &json!({"content": "hi"}));
    assert!(r.contains("is missing argument 'path'") && r.contains("Correct example"));
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
    assert!(r.contains("Unknown tool name") && r.contains("nonsense"));
    let r = e.run_tool("enable_tools", &json!({"tools": ["web_fetch"]}));
    assert!(r.contains("Nothing to enable"));
}

// ---- sandbox escape and gate ordering (review 2026-10-29) ----

#[cfg(unix)]
#[test]
fn create_file_through_a_symlink_and_dotdot_is_refused() {
    let (d, mut e) = ex();
    let outside = tempfile::tempdir().expect("tmp");
    let target = outside.path().join("target");
    std::fs::create_dir(&target).expect("mkdir");
    std::os::unix::fs::symlink(&target, d.path().join("link")).expect("symlink");
    // `link/../escaped.txt` is `<outside>/escaped.txt` to the kernel.
    let r = create(&mut e, "link/../escaped.txt", "x");
    assert!(r.contains("[safety gate"), "{r}");
    assert!(!outside.path().join("escaped.txt").exists());
}

#[test]
fn a_command_runs_as_written_and_the_gate_still_judges_its_unescaped_form() {
    let (_d, mut e) = ex();
    // The danger rules see the second command a literal backslash-n would hide.
    let r = e.run_tool("run_bash", &json!({"command": "echo hi\\nshutdown -h now"}));
    assert!(r.contains("dangerous operation"), "{r}");
    if cfg!(unix) {
        // What the shell gets is the model's text (bead boubou-bk9.10): a sed replacement
        // keeps its literal \n, a `python -c` program keeps its \" quotes. The old rewrite
        // turned both into real characters and broke four correct commands in a row in a
        // live editor session.
        let r = e.run_tool(
            "run_bash",
            &json!({"command": "printf '%s' \"s/a/b\\nc/\""}),
        );
        assert!(r.contains("s/a/b\\nc/"), "{r}");
        let r = e.run_tool(
            "run_bash",
            &json!({"command": "printf '%s' \"say \\\"hi\\\"\""}),
        );
        assert!(r.contains("say \"hi\""), "{r}");
    }
}

#[test]
fn read_file_on_a_directory_says_so() {
    let (d, mut e) = ex();
    std::fs::create_dir_all(d.path().join("pubspec.yaml")).expect("mkdir");
    let r = e.run_tool("read_file", &json!({"path": "pubspec.yaml"}));
    assert_eq!(r, "[not a file: pubspec.yaml is a directory; use list_dir]");
}

#[cfg(unix)]
#[test]
fn a_bash_rm_snapshots_the_files_it_names_globs_included() {
    let (d, mut e) = ex();
    let h = tempfile::tempdir().expect("tmp");
    e.harness_dir = h.path().to_path_buf();
    create(&mut e, "lib/a.dart", "a\n");
    create(&mut e, "lib/b.dart", "b\n");
    create(&mut e, "lib/keep.txt", "k\n");
    let r = e.run_tool("run_bash", &json!({"command": "rm lib/*.dart"}));
    assert!(
        r.contains("[backup of a.dart, b.dart kept by the harness; nothing to clean up]"),
        "{r}"
    );
    assert!(!d.path().join("lib/a.dart").exists());
    assert!(d.path().join("lib/keep.txt").exists());
    for (f, body) in [("a", "a\n"), ("b", "b\n")] {
        assert_eq!(
            std::fs::read_to_string(h.path().join(format!("backups/lib/{f}.dart.bak")))
                .expect("bak"),
            body
        );
    }
    let r = e.run_tool("run_bash", &json!({"command": "rm -f lib/nothing.dart"}));
    assert!(!r.contains("backup"), "{r}");
}

// ---- harness files out of the project (boubou-7s3) ----

#[test]
fn a_relocated_harness_dir_keeps_backups_trash_and_the_plan_out_of_the_workdir() {
    let (d, mut e) = ex();
    let h = tempfile::tempdir().expect("tmp");
    e.harness_dir = h.path().to_path_buf();
    create(&mut e, "lib/a.py", "x = 1\ny = 2\n");

    let r = e.run_tool(
        "edit_file",
        &json!({"path": "lib/a.py", "old": "y = 2", "new": "y = 3"}),
    );
    assert!(
        r.starts_with("[edited lib/a.py") && !r.contains(".bak"),
        "{r}"
    );
    assert!(!d.path().join("lib/a.py.bak").exists());
    assert_eq!(
        std::fs::read_to_string(h.path().join("backups/lib/a.py.bak")).expect("bak"),
        "x = 1\ny = 2\n"
    );

    let r = e.run_tool("run_bash", &json!({"command": "echo z > lib/a.py"}));
    assert!(
        r.contains("[backup of a.py kept by the harness; nothing to clean up]")
            && !r.contains(".bak"),
        "{r}"
    );
    assert!(!d.path().join("lib/a.py.bak").exists());
    assert_eq!(
        std::fs::read_to_string(h.path().join("backups/lib/a.py.bak")).expect("bak"),
        "x = 1\ny = 3\n"
    );

    let r = e.run_tool("delete_file", &json!({"path": "lib/a.py"}));
    assert!(r.contains("recoverable") && !r.contains("trash"), "{r}");
    assert!(!d.path().join(TRASH_DIR).exists());
    assert_eq!(
        std::fs::read_dir(h.path().join("trash"))
            .expect("trash")
            .count(),
        1
    );

    e.run_tool("todo", &json!({"action": "create", "items": ["one"]}));
    assert!(!d.path().join("todo.json").exists());
    assert!(h.path().join("todo.json").exists());

    let left: Vec<String> = std::fs::read_dir(d.path())
        .expect("ls")
        .flatten()
        .map(|x| x.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec!["lib".to_string()], "{left:?}");
}

#[test]
fn list_dir_hides_the_harness_files_in_the_workdir_layout() {
    let (d, mut e) = ex();
    create(&mut e, "a.py", "x = 1\n");
    e.run_tool(
        "edit_file",
        &json!({"path": "a.py", "old": "1", "new": "2"}),
    );
    create(&mut e, "gone.txt", "bye");
    e.run_tool("delete_file", &json!({"path": "gone.txt"}));
    for f in [
        ".agent_state.json",
        ".agent_state.meta.json",
        ".hb_pending_images.json",
    ] {
        std::fs::write(d.path().join(f), "{}").expect("write");
    }
    assert!(d.path().join("a.py.bak").exists() && d.path().join(TRASH_DIR).exists());
    assert_eq!(e.run_tool("list_dir", &json!({"path": "."})), "a.py");
}

// ---- the workdir-prefix rule (profile `rebase_workdir_prefix`, bead boubou-8e6) ----

/// An executor whose workdir is a directory literally named `workdir`, as the bench lays
/// cells out, with one fixture file at its root.
fn ex_named_workdir(rule_on: bool) -> (tempfile::TempDir, Executor) {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path().join("workdir");
    std::fs::create_dir(&wd).expect("mkdir");
    std::fs::write(wd.join("slug.py"), "def slugify(t):\n    return t\n").expect("fixture");
    let mut e = Executor::with_policy(&wd, SafetyPolicy::default());
    e.rebase_workdir_prefix = rule_on;
    (d, e)
}

#[test]
fn the_rule_rewrites_only_a_missing_first_segment_named_like_the_workdir() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path().join("proj");
    std::fs::create_dir(&wd).expect("mkdir");
    assert_eq!(
        rebase_workdir_prefix(&wd, "proj/a.py").as_deref(),
        Some("a.py")
    );
    assert_eq!(
        rebase_workdir_prefix(&wd, "proj/src/a.py").as_deref(),
        Some("src/a.py")
    );
    assert_eq!(rebase_workdir_prefix(&wd, "proj").as_deref(), Some("."));
    assert_eq!(
        rebase_workdir_prefix(&wd, "./proj/a.py").as_deref(),
        Some("a.py")
    );
    assert_eq!(rebase_workdir_prefix(&wd, "a.py"), None, "no prefix");
    assert_eq!(
        rebase_workdir_prefix(&wd, "other/a.py"),
        None,
        "another first segment"
    );
    assert_eq!(
        rebase_workdir_prefix(&wd, "project/a.py"),
        None,
        "a longer name is not the name"
    );
    let abs = wd.join("proj").join("a.py");
    assert_eq!(
        rebase_workdir_prefix(&wd, abs.to_str().expect("utf8")),
        None,
        "absolute"
    );
    // A real subdirectory of that name (a package named like its repository) is left alone.
    std::fs::create_dir(wd.join("proj")).expect("mkdir");
    assert_eq!(rebase_workdir_prefix(&wd, "proj/a.py"), None);
}

#[test]
fn with_the_rule_on_an_invented_workdir_prefix_reaches_the_real_file_and_says_so() {
    let (_d, mut e) = ex_named_workdir(true);
    let out = e.run_tool("read_file", &json!({"path": "workdir/slug.py"}));
    assert!(out.contains("def slugify"), "{out}");
    assert!(
        out.contains("(path rebased: workdir/slug.py → slug.py;"),
        "{out}"
    );
    assert!(out.contains("has no `workdir/` subdirectory"), "{out}");
    assert_eq!(rebased_path(&out), Some(("workdir/slug.py", "slug.py")));

    let out = e.run_tool("list_dir", &json!({"path": "workdir"}));
    assert!(out.starts_with("slug.py"), "{out}");
    assert_eq!(rebased_path(&out), Some(("workdir", ".")));

    let out = e.run_tool(
        "create_file",
        &json!({"path": "workdir/new.py", "content": "x = 1\n"}),
    );
    assert!(out.starts_with("[created new.py"), "{out}");
    assert!(e.workdir.join("new.py").exists());
    assert!(
        !e.workdir.join("workdir").exists(),
        "no subdirectory was created"
    );

    let out = e.run_tool(
        "edit_file",
        &json!({"path": "workdir/slug.py", "old": "return t", "new": "return t.lower()"}),
    );
    assert!(out.starts_with("[edited slug.py"), "{out}");
    assert!(std::fs::read_to_string(e.workdir.join("slug.py"))
        .expect("read")
        .contains("lower()"));

    // A plain path carries no clause.
    let out = e.run_tool("read_file", &json!({"path": "slug.py"}));
    assert_eq!(rebased_path(&out), None);
    assert!(!out.contains("path rebased"), "{out}");
}

#[test]
fn with_the_rule_off_the_invented_prefix_is_not_found_as_before() {
    let (_d, mut e) = ex_named_workdir(false);
    let out = e.run_tool("read_file", &json!({"path": "workdir/slug.py"}));
    assert_eq!(out, "[not found: workdir/slug.py]");
    let out = e.run_tool(
        "create_file",
        &json!({"path": "workdir/new.py", "content": "x = 1\n"}),
    );
    assert!(out.starts_with("[created workdir/new.py"), "{out}");
    assert!(e.workdir.join("workdir").join("new.py").exists());
}

#[test]
fn a_real_subdirectory_named_like_the_workdir_is_used_as_written() {
    let (_d, mut e) = ex_named_workdir(true);
    std::fs::create_dir(e.workdir.join("workdir")).expect("mkdir");
    std::fs::write(e.workdir.join("workdir").join("inner.txt"), "inner").expect("fixture");
    let out = e.run_tool("read_file", &json!({"path": "workdir/inner.txt"}));
    assert_eq!(out, "inner");
}

// ---- the FileStore seam (bead boubou-m64.2) ----

/// A host that owns the files: what it holds is what the tools see, whatever the disk says.
#[derive(Default)]
struct Held {
    files: std::collections::HashMap<std::path::PathBuf, String>,
    log: Vec<String>,
}

#[derive(Clone)]
struct Host(std::sync::Arc<std::sync::Mutex<Held>>);

impl rufous_kernel::exec::FileStore for Host {
    fn read(&mut self, path: &std::path::Path) -> std::io::Result<String> {
        let mut h = self.0.lock().expect("lock");
        h.log.push(format!("read {}", path.display()));
        h.files
            .get(path)
            .cloned()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
    }
    fn write(&mut self, path: &std::path::Path, content: &str) -> std::io::Result<()> {
        let mut h = self.0.lock().expect("lock");
        h.log.push(format!("write {}", path.display()));
        h.files.insert(path.to_path_buf(), content.to_string());
        Ok(())
    }
}

#[test]
fn the_text_file_tools_go_through_the_store_and_the_rest_stays_on_disk() {
    let (d, mut e) = ex();
    let host = Host(std::sync::Arc::new(std::sync::Mutex::new(Held::default())));
    let held = host.0.clone();
    e.store = Box::new(host);
    let notes = d.path().join("notes.txt");
    // The disk and the host disagree, as an unsaved editor buffer does.
    std::fs::write(&notes, "on disk\n").expect("fixture");
    held.lock()
        .expect("lock")
        .files
        .insert(notes.clone(), "in the buffer\n".to_string());

    // read_file serves the host's text.
    assert_eq!(read(&mut e, "notes.txt"), "in the buffer\n");
    // edit_file matches against it, writes through the host and backs up what it read.
    let r = edit(&mut e, "notes.txt", "in the buffer", "in the editor");
    assert!(r.starts_with("[edited notes.txt (backed up)"), "{r}");
    assert_eq!(
        held.lock().expect("lock").files[&notes],
        "in the editor\n",
        "the host holds the edit"
    );
    assert_eq!(
        std::fs::read_to_string(&notes).expect("read"),
        "on disk\n",
        "the disk was not written behind the host"
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("notes.txt.bak")).expect("bak"),
        "in the buffer\n",
        "the safety net copies the text the model saw"
    );
    // create_file and append_file (the trait's default append: read, then write).
    assert!(create(&mut e, "new.md", "first\n").starts_with("[created new.md"));
    let r = e.run_tool(
        "append_file",
        &json!({"path": "new.md", "content": "second\n"}),
    );
    assert_eq!(r, "[appended new.md]");
    assert_eq!(
        held.lock().expect("lock").files[&d.path().join("new.md")],
        "first\nsecond\n"
    );
    assert!(
        !d.path().join("new.md").exists(),
        "created through the host only"
    );
    // Escape repair happens on the text handed to the host, not by re-reading the disk.
    let r = create(&mut e, "esc.txt", r"a\nb\nc\nd");
    assert!(r.contains("escapes auto-repaired"), "{r}");
    assert_eq!(
        held.lock().expect("lock").files[&d.path().join("esc.txt")],
        "a\nb\nc\nd"
    );
    // A file the host does not hold is still "not found": existence is the disk's.
    assert_eq!(read(&mut e, "ghost.txt"), "[not found: ghost.txt]");
    // delete_file and list_dir are the disk's: the protocol has no method for them.
    assert!(e
        .run_tool("delete_file", &json!({"path": "notes.txt"}))
        .starts_with("[deleted notes.txt"));
    assert!(!notes.exists());
    let log = held.lock().expect("lock").log.clone();
    assert!(
        log.iter()
            .all(|l| l.starts_with("read ") || l.starts_with("write ")),
        "{log:?}"
    );
    assert!(
        !log.iter().any(|l| l.contains("ghost")),
        "no read for a file that does not exist: {log:?}"
    );
}

#[test]
fn the_disk_store_is_the_default_and_appends_in_place() {
    let (d, mut e) = ex();
    assert!(create(&mut e, "log.txt", "one\n").starts_with("[created log.txt"));
    assert_eq!(
        e.run_tool(
            "append_file",
            &json!({"path": "log.txt", "content": "two\n"})
        ),
        "[appended log.txt]"
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("log.txt")).expect("read"),
        "one\ntwo\n"
    );
    // Appending to a file that is not there creates it, as before.
    assert_eq!(
        e.run_tool("append_file", &json!({"path": "fresh.txt", "content": "x"})),
        "[appended fresh.txt]"
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("fresh.txt")).expect("read"),
        "x"
    );
}

// Bead boubou-l3e: qwen3.5:4b's `# Heron report` + `## Attendance` (3/3 longform cells of
// 2026-11-01) glued into one line because the append was raw. The tool now starts the
// appended text on a new line when the file does not end on one; nothing else changes.

#[test]
fn append_file_starts_on_a_new_line_when_the_file_does_not_end_on_one() {
    let (d, mut e) = ex();
    let text = |name: &str| std::fs::read_to_string(d.path().join(name)).expect("read");
    let append = |e: &mut rufous_kernel::exec::Executor, name: &str, content: &str| {
        e.run_tool("append_file", &json!({"path": name, "content": content}))
    };
    // The live case.
    assert!(create(&mut e, "report.md", "# Heron report").starts_with("[created"));
    assert_eq!(
        append(&mut e, "report.md", "## Attendance\n\nFour attended.\n"),
        "[appended report.md]"
    );
    assert_eq!(
        text("report.md"),
        "# Heron report\n## Attendance\n\nFour attended.\n"
    );
    // A file that ends on a newline gets nothing extra, and a content that brings its own
    // blank line keeps exactly that.
    append(&mut e, "report.md", "## Risks\n");
    append(&mut e, "report.md", "\n## Owners\n");
    assert_eq!(
        text("report.md"),
        "# Heron report\n## Attendance\n\nFour attended.\n## Risks\n\n## Owners\n"
    );
    // A content starting on its own newline after a partial line is not doubled.
    create(&mut e, "a.txt", "x");
    append(&mut e, "a.txt", "\ny");
    assert_eq!(text("a.txt"), "x\ny");
    // An empty file, a missing file and empty content are left as they are.
    create(&mut e, "empty.txt", "");
    append(&mut e, "empty.txt", "z");
    assert_eq!(text("empty.txt"), "z");
    append(&mut e, "fresh.txt", "q");
    assert_eq!(text("fresh.txt"), "q");
    append(&mut e, "a.txt", "");
    assert_eq!(text("a.txt"), "x\ny");
}

// Bead boubou-2pl: the snapshot's path pattern stopped at `~`, so an absolute path under an
// 8.3 short name (C:\Users\RUNNER~1\…, Windows CI's temp directory) matched no file and the
// shell overwrote it with no backup.
#[test]
fn a_shell_write_by_absolute_path_is_backed_up_when_the_workdir_path_has_a_tilde() {
    let t = tempfile::tempdir().expect("tmp");
    let wd = t.path().join("RUN~1").join("work");
    std::fs::create_dir_all(wd.join("lib")).expect("mkdir");
    std::fs::write(wd.join("lib").join("f.txt"), "old\n").expect("seed");
    let h = tempfile::tempdir().expect("harness");
    let mut e = Executor::with_policy(&wd, SafetyPolicy::default());
    e.harness_dir = h.path().to_path_buf();
    let target = wd.join("lib").join("f.txt");
    let r = e.run_tool(
        "run_bash",
        &json!({"command": format!("echo new >> {}", target.display())}),
    );
    assert!(
        r.contains("[backup of f.txt kept by the harness; nothing to clean up]"),
        "{r}"
    );
    assert_eq!(
        std::fs::read_to_string(h.path().join("backups").join("lib").join("f.txt.bak"))
            .expect("bak"),
        "old\n"
    );
}

// ---- a backgrounded process holding the output (bead boubou-d4z) ----

#[cfg(unix)]
fn running(pattern: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(unix)]
#[test]
fn a_backgrounded_process_holding_the_output_is_stopped_after_the_grace() {
    // Live Zed 2026-11-02: `./gradlew npmBuild run &` hung the turn for 11 minutes; the
    // shell had exited and the wait for its output had no limit.
    use std::time::{Duration, Instant};
    let (_d, mut e) = ex();
    let t0 = Instant::now();
    let out = e.run_tool("run_bash", &json!({"command": "echo up; sleep 37.4141 &"}));
    assert!(t0.elapsed() < Duration::from_secs(8), "{out}");
    assert!(out.contains("up"), "{out}");
    assert!(
        out.contains("a background process kept the command's output open"),
        "{out}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(!running("sleep 37.4141"), "the background process is gone");
}

#[cfg(unix)]
#[test]
fn a_backgrounded_process_with_redirected_output_is_left_alone() {
    use std::time::{Duration, Instant};
    let (_d, mut e) = ex();
    let t0 = Instant::now();
    let out = e.run_tool(
        "run_bash",
        &json!({"command": "sleep 3.2718 > /dev/null 2>&1 &"}),
    );
    assert!(t0.elapsed() < Duration::from_secs(2), "{out}");
    assert!(!out.contains("background process"), "{out}");
    assert!(running("sleep 3.2718"), "still running, as asked");
}

#[cfg(unix)]
#[test]
fn background_output_that_ends_within_the_grace_is_kept() {
    let (_d, mut e) = ex();
    let out = e.run_tool(
        "run_bash",
        &json!({"command": "echo hi; (sleep 1; echo late) &"}),
    );
    assert!(out.contains("hi") && out.contains("late"), "{out}");
    assert!(!out.contains("background process"), "{out}");
}

#[cfg(unix)]
#[test]
fn a_stop_during_the_grace_ends_the_call() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let (_d, mut e) = ex();
    let stop = Arc::new(AtomicBool::new(false));
    e.stop = Some(stop.clone());
    let raiser = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            stop.store(true, Ordering::SeqCst);
        })
    };
    let t0 = Instant::now();
    let out = e.run_tool("run_bash", &json!({"command": "sleep 38.5 &"}));
    raiser.join().expect("raiser");
    assert!(t0.elapsed() < Duration::from_millis(1500), "{out}");
    assert!(out.contains("stopped by the user"), "{out}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!running("sleep 38.5"));
}

// ---- search_files skips (bead boubou-r7i) ----

#[test]
fn search_files_skips_stores_binaries_and_backups_from_the_root() {
    let (d, mut e) = ex();
    let put = |rel: &str, body: &[u8]| {
        let p = d.path().join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, body).expect("write");
    };
    put("src/app.txt", b"needle in text\n");
    put(".git/index", b"needle in git\n");
    put(".beads/embeddeddolt/noms", b"needle in beads\n");
    put("node_modules/pkg/index.js", b"needle in deps\n");
    put("blob.bin", b"needle\0in binary\n");
    put("src/app.txt.bak", b"needle in backup\n");
    let r = e.run_tool("search_files", &json!({"path": ".", "pattern": "needle"}));
    assert!(r.contains("app.txt:1: needle in text"), "{r}");
    for absent in ["in git", "in beads", "in deps", "binary", "in backup"] {
        assert!(!r.contains(absent), "{absent}: {r}");
    }
    // Asked for by name, a store is still searched.
    let r = e.run_tool(
        "search_files",
        &json!({"path": ".git", "pattern": "needle"}),
    );
    assert!(r.contains("index:1: needle in git"), "{r}");
}

// Sandboxed helper (boubou-f8d): a path the OS refuses is said apart from a missing one, and
// nothing rufous started outlives it.

#[cfg(unix)]
#[test]
fn a_path_the_os_refuses_is_no_access_not_missing() {
    use std::os::unix::fs::PermissionsExt;
    let (d, mut e) = ex();
    let shut = d.path().join("shut");
    std::fs::create_dir(&shut).expect("dir");
    std::fs::write(shut.join("notes.md"), "secret").expect("file");
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read_dir(&shut).is_ok() {
        // Running as root: nothing is refused, so there is nothing to test.
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        return;
    }
    let calls = [
        ("read_file", json!({"path": "shut/notes.md"})),
        ("list_dir", json!({"path": "shut"})),
        ("search_files", json!({"path": "shut", "pattern": "secret"})),
        (
            "create_file",
            json!({"path": "shut/new.md", "content": "x"}),
        ),
        (
            "append_file",
            json!({"path": "shut/notes.md", "content": "x"}),
        ),
        (
            "edit_file",
            json!({"path": "shut/notes.md", "old": "secret", "new": "x"}),
        ),
    ];
    let outs: Vec<String> = calls.iter().map(|(n, a)| e.run_tool(n, a)).collect();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    for ((name, _), out) in calls.iter().zip(&outs) {
        assert!(
            out.starts_with("[no access: ") && out.ends_with("(not shared with this assistant)]"),
            "{name}: {out}"
        );
    }
    assert!(rufous_kernel::agent::run::is_tool_error(&outs[0]));
}

#[cfg(unix)]
#[test]
fn live_groups_are_killed_with_everything_they_started() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let mut child = std::process::Command::new("sh")
        .args(["-c", "sleep 30 & wait"])
        .process_group(0)
        .spawn()
        .expect("spawn");
    let live = rufous_kernel::exec::LiveGroup::register(child.id());
    rufous_kernel::exec::kill_live_groups();
    let st = child.wait().expect("wait");
    assert_eq!(st.signal(), Some(9), "{st:?}");
    drop(live);
}
