//! Command hooks (bead boubou-w3i.2): the shared `hooks.json` shape read from every
//! source, the protocol over stdin / exit codes / JSON stdout, and the four events at their
//! seams: `PreToolUse`/`PostToolUse` in the executor, `SessionStart` in the opening prompt,
//! `Stop` at the end of a turn. Shell hooks, so Unix only.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rufous_kernel::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use rufous_kernel::agent::{Agent, AgentEvent, RunParams, ToolRunner, TurnPolicy};
use rufous_kernel::exec::Executor;
use rufous_kernel::hooks::{Event, Hooks};
use rufous_kernel::ollama::{Message, Role};
use rufous_kernel::safety::SafetyPolicy;
use serde_json::{json, Value};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

/// `.claude/settings.json` in `dir`, as Claude Code writes it, with the given events.
fn claude_settings(dir: &Path, hooks: Value) -> PathBuf {
    let p = dir.join(".claude").join("settings.json");
    write(&p, &json!({ "hooks": hooks }).to_string());
    p
}

fn entry(matcher: &str, command: &str) -> Value {
    json!([{ "matcher": matcher, "hooks": [{ "type": "command", "command": command }] }])
}

#[test]
fn the_shared_shape_is_read_from_every_source_and_merged() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path().join("proj");
    let home = d.path().join("home");
    claude_settings(
        &wd,
        json!({
            "SessionStart": entry("", "bd prime --hook-json"),
            "PreToolUse": entry("edit_file|create_file", "check.sh"),
            "UserPromptSubmit": entry("", "bd codex-hook UserPromptSubmit"),
        }),
    );
    // rufous's own file: a flat entry, a bad matcher that is skipped, a non-command type.
    write(
        &wd.join(".rufous").join("hooks.json"),
        &json!({ "hooks": {
            "Stop": [{ "command": "checklist.sh" }],
            "PostToolUse": [
                { "matcher": "(", "hooks": [{ "type": "command", "command": "never.sh" }] },
                { "matcher": "*", "hooks": [{ "type": "prompt", "prompt": "not a command" }] }
            ]
        }})
        .to_string(),
    );
    write(
        &home.join("hooks.json"),
        &json!({ "hooks": { "PostToolUse": entry("run_bash", "lint.sh") } }).to_string(),
    );
    // A timeout of its own on one entry.
    write(
        &wd.join(".claude").join("settings.local.json"),
        &json!({ "hooks": { "PostToolUse": [{ "hooks": [{ "type": "command", "command": "slow.sh", "timeout": 5 }] }] } }).to_string(),
    );
    write(&wd.join("broken.json"), "{ not json");

    let h = Hooks::from_files(
        &wd,
        [
            home.join("hooks.json"),
            wd.join(".rufous").join("hooks.json"),
            wd.join(".claude").join("settings.json"),
            wd.join(".claude").join("settings.local.json"),
            wd.join("broken.json"),
            wd.join("missing.json"),
        ],
    );
    assert_eq!(h.hooks.len(), 5, "{:#?}", h.hooks);
    let by = |e: Event| h.hooks.iter().filter(|x| x.event == e).count();
    assert_eq!(by(Event::SessionStart), 1);
    assert_eq!(by(Event::PreToolUse), 1);
    assert_eq!(by(Event::PostToolUse), 2);
    assert_eq!(by(Event::Stop), 1);
    assert_eq!(h.sources.len(), 4);
    assert_eq!(h.ignored_events, vec!["UserPromptSubmit"]);
    let slow = h
        .hooks
        .iter()
        .find(|x| x.command == "slow.sh")
        .expect("slow");
    assert_eq!(slow.timeout, Duration::from_secs(5));
    assert!(slow.matcher.is_none());
    let pre = h
        .hooks
        .iter()
        .find(|x| x.event == Event::PreToolUse)
        .expect("pre");
    let m = pre.matcher.as_ref().expect("matcher");
    assert!(m.is_match("edit_file") && m.is_match("create_file") && !m.is_match("edit_files"));
    let line = h.describe();
    assert!(
        line.starts_with("5 hooks: SessionStart×1, PreToolUse×1, PostToolUse×2, Stop×1 (from "),
        "{line}"
    );
    assert!(
        line.ends_with("; not run by this version: UserPromptSubmit"),
        "{line}"
    );
    assert!(Hooks::none().is_empty());
}

#[test]
fn the_executor_runs_pre_and_post_tool_hooks_around_every_call() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    let settings = claude_settings(
        wd,
        json!({
            "PreToolUse": [
                { "matcher": "delete_file", "hooks": [{ "type": "command",
                    "command": "echo 'deletes are off in this project' >&2; exit 2" }] },
                { "matcher": "read_file", "hooks": [{ "type": "command",
                    "command": "cat > \"$RUFOUS_PROJECT_DIR/seen.json\"" }] }
            ],
            "PostToolUse": entry("create_file",
                "cat > /dev/null; echo '{\"hookSpecificOutput\":{\"hookEventName\":\"PostToolUse\",\"additionalContext\":\"remember to run the tests\"}}'"),
        }),
    );
    let mut e = Executor::with_policy(wd, SafetyPolicy::default());
    e.hooks = Hooks::from_files(wd, [settings]);

    // PostToolUse context rides on the result.
    let r = e.run_tool("create_file", &json!({"path": "a.txt", "content": "hello"}));
    assert!(r.starts_with("[created a.txt"), "{r}");
    assert!(r.ends_with("\n[hook: remember to run the tests]"), "{r}");

    // PreToolUse exit 2: the call does not run, the stderr is the reason, the ladder
    // sees a tool error.
    let r = e.run_tool("delete_file", &json!({"path": "a.txt"}));
    assert_eq!(
        r,
        "[tool error: blocked by a project hook: deletes are off in this project]"
    );
    assert!(wd.join("a.txt").exists(), "the delete was blocked");

    // The hook's input is the protocol's: event, cwd, tool name and arguments.
    let r = e.run_tool("read_file", &json!({"path": "a.txt"}));
    assert_eq!(r, "hello");
    let seen: Value =
        serde_json::from_str(&std::fs::read_to_string(wd.join("seen.json")).expect("seen"))
            .expect("json");
    assert_eq!(seen["hook_event_name"], "PreToolUse");
    assert_eq!(seen["tool_name"], "read_file");
    assert_eq!(seen["tool_input"]["path"], "a.txt");
    assert_eq!(seen["cwd"], wd.display().to_string());

    // A tool no matcher names runs untouched.
    let r = e.run_tool("list_dir", &json!({"path": "."}));
    assert!(r.contains("a.txt") && !r.contains("[hook"), "{r}");
}

/// Replies in order; text after the script runs out.
struct Script(Vec<ChatResponse>);
impl ChatBackend for Script {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        Ok(if self.0.is_empty() {
            ChatResponse::text("fallback")
        } else {
            self.0.remove(0)
        })
    }
}
struct OkTools;
impl ToolRunner for OkTools {
    fn run_tool(&mut self, name: &str, _args: &Value) -> String {
        if name == "finish" {
            "[TASK_COMPLETE] ok".into()
        } else {
            "[ok]".into()
        }
    }
}

fn agent(
    wd: &Path,
    script: Vec<ChatResponse>,
    logs: Arc<Mutex<Vec<String>>>,
) -> Agent<Script, OkTools> {
    let mut params = RunParams::plain("gemma4:e2b", wd);
    params.policy = TurnPolicy::conversation();
    Agent::new(params, Script(script), OkTools)
        .with_hooks(Hooks::from_files(
            wd,
            [wd.join(".claude").join("settings.json")],
        ))
        .with_sink(move |ev| {
            if let AgentEvent::Log(l) = ev {
                logs.lock().expect("lock").push(l);
            }
        })
}

#[test]
fn session_start_context_enters_the_task_prompt_once_and_failures_are_logged() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    claude_settings(
        wd,
        json!({ "SessionStart": [{ "hooks": [
            { "type": "command", "command": "echo '{\"hookSpecificOutput\":{\"hookEventName\":\"SessionStart\",\"additionalContext\":\"Project rule: run bd prime.\"}}'" },
            { "type": "command", "command": "echo 'Second line.'" },
            { "type": "command", "command": "echo nope >&2; exit 1" }
        ]}]}),
    );
    let logs = Arc::new(Mutex::new(Vec::new()));
    let mut a = agent(wd, vec![ChatResponse::text("Done.")], logs.clone());
    let m = a.task_messages("Tidy the notes");
    let sys = &m[0].content;
    let at = sys
        .find("SESSION CONTEXT (from the project's hooks; follow it):\nProject rule: run bd prime.\n\nSecond line.")
        .expect("context block");
    assert!(at < sys.find("Current working directory").expect("workdir line"));
    assert_eq!(m[1].content, "Tidy the notes", "the task turn is untouched");
    {
        let l = logs.lock().expect("lock");
        assert!(
            l.iter().any(|x| x.starts_with("[hooks] SessionStart: ")
                && x.ends_with(" chars of context into the task prompt")),
            "{l:?}"
        );
        assert!(
            l.iter()
                .any(|x| x.contains("SessionStart hook `echo nope >&2; exit 1` exited 1: nope")),
            "{l:?}"
        );
    }
    // The turn runs; the hooks do not fire again when the prompt is rebuilt.
    let out = a.run(m, Some("hooks_test"), None).expect("loop");
    assert_eq!(out.last().expect("reply").content, "Done.");
    let l = logs.lock().expect("lock");
    assert_eq!(
        l.iter()
            .filter(|x| x.starts_with("[hooks] SessionStart: "))
            .count(),
        1
    );
    assert!(
        l.iter()
            .any(|x| x.starts_with("[hooks] 3 hooks: SessionStart×3 (from ")),
        "{l:?}"
    );
}

#[test]
fn a_stop_hook_holds_a_text_turn_open_once() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    // Always blocks: the cap, not the hook, ends the turn.
    claude_settings(
        wd,
        json!({ "Stop": entry("", "cat > \"$RUFOUS_PROJECT_DIR/stop_$(date +%s%N).json\"; echo 'Tick the checklist first' >&2; exit 2") }),
    );
    let logs = Arc::new(Mutex::new(Vec::new()));
    let mut a = agent(
        wd,
        vec![
            ChatResponse::text("Done."),
            ChatResponse::text("Done, and the checklist is ticked."),
        ],
        logs.clone(),
    );
    let m = a.task_messages("Tidy the notes");
    let out = a.run(m, Some("stop_test"), None).expect("loop");
    let users: Vec<&str> = out
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(
        users,
        vec!["Tidy the notes", "[stop hook] Tick the checklist first"],
        "{out:#?}"
    );
    assert_eq!(
        out.last().expect("last").content,
        "Done, and the checklist is ticked."
    );
    let l = logs.lock().expect("lock");
    assert!(
        l.iter()
            .any(|x| x.contains("a Stop hook holds the turn open: Tick the checklist first")),
        "{l:?}"
    );
    assert!(
        l.iter()
            .any(|x| x.contains("Stop blocked again; ignored after one block per turn")),
        "{l:?}"
    );
    // The second call saw stop_hook_active: true.
    let mut inputs: Vec<Value> = std::fs::read_dir(wd)
        .expect("ls")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("stop_"))
        })
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).expect("read")).expect("json"))
        .collect();
    inputs.sort_by_key(|v| v["stop_hook_active"].as_bool());
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0]["stop_hook_active"], false);
    assert_eq!(inputs[1]["stop_hook_active"], true);
    assert_eq!(inputs[0]["session_id"], "stop_test");
    assert_eq!(inputs[0]["hook_event_name"], "Stop");
}

#[test]
fn a_stop_hook_defers_a_finish_once() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    claude_settings(
        wd,
        json!({ "Stop": entry("", "echo 'Write the summary file first' >&2; exit 2") }),
    );
    let logs = Arc::new(Mutex::new(Vec::new()));
    let finish = || ChatResponse::call("", "finish", json!({"summary": "done"}));
    let mut a = agent(wd, vec![finish(), finish()], logs.clone());
    let m = a.task_messages("Tidy the notes");
    let out = a.run(m, None, None).expect("loop");
    let texts: Vec<&str> = out.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        texts
            .iter()
            .filter(|t| **t == "[stop hook] Write the summary file first")
            .count(),
        1,
        "{texts:#?}"
    );
    assert!(
        texts.iter().any(
            |t| t.starts_with("[finish deferred: a project hook asked for more before the end")
        ),
        "{texts:#?}"
    );
    let l = logs.lock().expect("lock");
    assert_eq!(
        l.iter().filter(|x| x.contains("TASK COMPLETE")).count(),
        1,
        "{l:?}"
    );
}
