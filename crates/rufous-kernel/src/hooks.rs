//! Command hooks (bead boubou-w3i.2): a project or a user runs their own commands at the
//! loop's events, in the JSON shape and protocol the other harnesses share, so a
//! `hooks.json` written for one of them works here unchanged.
//!
//! Events and where they fire: `SessionStart` when the opening task prompt is built (its
//! context goes into the prompt, capped at [`CONTEXT_CHARS`]); `PreToolUse` and
//! `PostToolUse` in the executor's `run_tool`, around prepare and dispatch, so every entry
//! point sees them; `Stop` at the two points a turn ends, where a block becomes a user
//! message at most once per turn (a hook that always blocks cannot hold the loop).
//!
//! Protocol: the hook's command runs through the platform shell in the working directory
//! with one JSON object on stdin (`hook_event_name`, `cwd`, `session_id`, and per event
//! `tool_name`/`tool_input`, `tool_response`, `source`, `stop_hook_active`). Exit 0 with a
//! JSON object on stdout may carry `decision: "block"` + `reason`,
//! `hookSpecificOutput.permissionDecision: "deny"` + `permissionDecisionReason`, and
//! `hookSpecificOutput.additionalContext`; plain stdout is context for `SessionStart` and
//! ignored otherwise. Exit 2 blocks with stderr as the reason. Any other exit, or a timeout,
//! is logged and the loop proceeds. Hooks only narrow: an `allow` does not bypass the safety
//! rings, which keep the last word.
//!
//! Sources, all merged: `<agent_home>/hooks.json`, `<workdir>/.rufous/hooks.json` (and
//! `.boubou/hooks.json`, the name before rufous),
//! `<workdir>/.claude/settings.json`, `<workdir>/.claude/settings.local.json` (their `hooks`
//! key). `AGENT_HOOKS=0` turns every hook off.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};

/// Upper bound on the `SessionStart` context placed in the task prompt.
pub const CONTEXT_CHARS: usize = 6000;
/// Upper bound on the tool response handed to a `PostToolUse` hook.
pub const RESPONSE_CHARS: usize = 20_000;
/// A hook's wall-clock limit when its entry names none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    SessionStart,
    PreToolUse,
    PostToolUse,
    Stop,
}

impl Event {
    /// The name as the protocol writes it.
    pub fn name(self) -> &'static str {
        match self {
            Event::SessionStart => "SessionStart",
            Event::PreToolUse => "PreToolUse",
            Event::PostToolUse => "PostToolUse",
            Event::Stop => "Stop",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "sessionstart" => Event::SessionStart,
            "pretooluse" => Event::PreToolUse,
            "posttooluse" => Event::PostToolUse,
            "stop" => Event::Stop,
            _ => return None,
        })
    }
}

/// One command at one event.
#[derive(Debug, Clone)]
pub struct Hook {
    pub event: Event,
    /// Matched in full against the tool name (`PreToolUse`/`PostToolUse`) or the source
    /// (`SessionStart`); `None` matches everything.
    pub matcher: Option<Regex>,
    pub command: String,
    pub timeout: Duration,
    /// The file the entry came from, for the log.
    pub source: String,
}

impl Hook {
    fn matches(&self, subject: &str) -> bool {
        self.matcher.as_ref().is_none_or(|m| m.is_match(subject))
    }
}

/// What the hooks of one event said, together.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The first block, with its reason; the remaining hooks of the event did not run.
    pub blocked: Option<String>,
    /// `additionalContext` (and plain stdout at `SessionStart`), in hook order.
    pub context: Vec<String>,
    /// Lines for the harness log: errors, timeouts, blocks.
    pub notes: Vec<String>,
}

/// The configured hooks of a run.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    pub hooks: Vec<Hook>,
    /// Files that contributed entries, for the log.
    pub sources: Vec<String>,
    /// Event names found in the files that this version does not run.
    pub ignored_events: Vec<String>,
    pub session_id: String,
    cwd: PathBuf,
}

impl Hooks {
    /// No hooks at all (the executor's default until the loop hands it the run's).
    pub fn none() -> Self {
        Self::default()
    }

    /// The run's hooks from every source, or none under `AGENT_HOOKS=0`.
    pub fn load(workdir: &Path, agent_home: &Path) -> Self {
        if std::env::var("AGENT_HOOKS").is_ok_and(|v| matches!(v.trim(), "0" | "off" | "false")) {
            return Self::none();
        }
        Self::from_files(
            workdir,
            [
                agent_home.join("hooks.json"),
                workdir.join(".rufous").join("hooks.json"),
                workdir.join(".boubou").join("hooks.json"),
                workdir.join(".claude").join("settings.json"),
                workdir.join(".claude").join("settings.local.json"),
            ],
        )
    }

    /// The hooks in the given files (missing files skipped; a malformed one logged and
    /// skipped), running in `cwd`.
    pub fn from_files(cwd: &Path, files: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut out = Self {
            cwd: cwd.to_path_buf(),
            ..Self::default()
        };
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue;
            };
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("hooks: {} is not JSON: {e}", f.display());
                    continue;
                }
            };
            let label = f.display().to_string();
            let before = out.hooks.len();
            out.add_config(&v, &label);
            if out.hooks.len() > before {
                out.sources.push(label);
            }
        }
        out
    }

    /// Add the entries of one config object (`{"hooks": {Event: [...]}}`).
    pub fn add_config(&mut self, cfg: &Value, source: &str) {
        let Some(events) = cfg.get("hooks").and_then(Value::as_object) else {
            return;
        };
        for (name, groups) in events {
            let Some(event) = Event::parse(name) else {
                if !self.ignored_events.contains(name) {
                    self.ignored_events.push(name.clone());
                }
                continue;
            };
            let Some(groups) = groups.as_array() else {
                continue;
            };
            for g in groups {
                let matcher = match g.get("matcher").and_then(Value::as_str).map(str::trim) {
                    None | Some("") | Some("*") => None,
                    Some(m) => match Regex::new(&format!("^(?:{m})$")) {
                        Ok(r) => Some(r),
                        Err(e) => {
                            tracing::warn!("hooks: {source}: matcher {m:?} is not a regex: {e}");
                            continue;
                        }
                    },
                };
                // The shared shape nests the commands under `hooks`; a flat entry with a
                // `command` of its own is taken too.
                let entries: Vec<&Value> = match g.get("hooks").and_then(Value::as_array) {
                    Some(list) => list.iter().collect(),
                    None => vec![g],
                };
                for e in entries {
                    if e.get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t != "command")
                    {
                        continue;
                    }
                    let Some(command) = e.get("command").and_then(Value::as_str) else {
                        continue;
                    };
                    let timeout = e
                        .get("timeout")
                        .and_then(Value::as_f64)
                        .filter(|t| *t > 0.0)
                        .map_or(DEFAULT_TIMEOUT, Duration::from_secs_f64);
                    self.hooks.push(Hook {
                        event,
                        matcher: matcher.clone(),
                        command: command.to_string(),
                        timeout,
                        source: source.to_string(),
                    });
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// One line for the harness log: counts per event and the files they came from.
    pub fn describe(&self) -> String {
        let count = |ev: Event| self.hooks.iter().filter(|h| h.event == ev).count();
        let mut parts = Vec::new();
        for ev in [
            Event::SessionStart,
            Event::PreToolUse,
            Event::PostToolUse,
            Event::Stop,
        ] {
            let n = count(ev);
            if n > 0 {
                parts.push(format!("{}×{n}", ev.name()));
            }
        }
        let mut s = format!(
            "{} hook{}: {} (from {})",
            self.hooks.len(),
            if self.hooks.len() == 1 { "" } else { "s" },
            parts.join(", "),
            self.sources.join(", ")
        );
        if !self.ignored_events.is_empty() {
            s.push_str(&format!(
                "; not run by this version: {}",
                self.ignored_events.join(", ")
            ));
        }
        s
    }

    fn base_input(&self, event: Event) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("hook_event_name".into(), json!(event.name()));
        m.insert("cwd".into(), json!(self.cwd.display().to_string()));
        m.insert("session_id".into(), json!(self.session_id));
        m
    }

    /// `SessionStart`; `source` is `startup` for a fresh session.
    pub fn session_start(&self, source: &str) -> Outcome {
        let mut input = self.base_input(Event::SessionStart);
        input.insert("source".into(), json!(source));
        self.fire(Event::SessionStart, source, &Value::Object(input))
    }

    /// `PreToolUse`: `blocked` is the reason the call must not run.
    pub fn pre_tool(&self, tool: &str, args: &Value) -> Outcome {
        let mut input = self.base_input(Event::PreToolUse);
        input.insert("tool_name".into(), json!(tool));
        input.insert("tool_input".into(), args.clone());
        self.fire(Event::PreToolUse, tool, &Value::Object(input))
    }

    /// `PostToolUse`: `context` is appended to the tool result.
    pub fn post_tool(&self, tool: &str, args: &Value, response: &str) -> Outcome {
        let mut input = self.base_input(Event::PostToolUse);
        input.insert("tool_name".into(), json!(tool));
        input.insert("tool_input".into(), args.clone());
        input.insert(
            "tool_response".into(),
            json!(crate::text::head(response, RESPONSE_CHARS)),
        );
        self.fire(Event::PostToolUse, tool, &Value::Object(input))
    }

    /// `Stop`: `blocked` is the reason the turn must go on; `active` says a stop hook
    /// already blocked once in this turn.
    pub fn stop(&self, active: bool) -> Outcome {
        let mut input = self.base_input(Event::Stop);
        input.insert("stop_hook_active".into(), json!(active));
        self.fire(Event::Stop, "", &Value::Object(input))
    }

    fn fire(&self, event: Event, subject: &str, input: &Value) -> Outcome {
        let mut out = Outcome::default();
        let text = input.to_string();
        for h in self
            .hooks
            .iter()
            .filter(|h| h.event == event && h.matches(subject))
        {
            let run = run_hook(&h.command, &self.cwd, &text, h.timeout);
            match interpret(event, &run) {
                Verdict::Proceed(ctx) => {
                    if let Some(c) = ctx {
                        out.context.push(c);
                    }
                }
                Verdict::Block(reason) => {
                    out.notes.push(format!(
                        "{} hook `{}` blocked: {reason}",
                        event.name(),
                        crate::text::head(&h.command, 60)
                    ));
                    out.blocked = Some(reason);
                    break;
                }
                Verdict::Error(msg) => out.notes.push(format!(
                    "{} hook `{}` {msg}",
                    event.name(),
                    crate::text::head(&h.command, 60)
                )),
            }
        }
        out
    }
}

/// `SessionStart` contexts as one prompt block, capped at [`CONTEXT_CHARS`].
pub fn prompt_context(contexts: &[String]) -> Option<String> {
    let joined = contexts
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if joined.is_empty() {
        return None;
    }
    if crate::text::clen(&joined) > CONTEXT_CHARS {
        let cut = crate::text::head(&joined, CONTEXT_CHARS);
        return Some(format!(
            "{cut}\n[hook context truncated at {CONTEXT_CHARS} characters]"
        ));
    }
    Some(joined)
}

#[derive(Debug)]
pub struct HookRun {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Proceed(Option<String>),
    Block(String),
    Error(String),
}

fn str_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).map(str::trim)
}

/// The protocol's reading of one hook's exit code and output.
fn interpret(event: Event, run: &HookRun) -> Verdict {
    if run.timed_out {
        return Verdict::Error("timed out".to_string());
    }
    match run.code {
        0 => {
            let stdout = run.stdout.trim();
            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(stdout) {
                let s = str_of;
                if s(obj.get("decision")) == Some("block") {
                    return Verdict::Block(
                        s(obj.get("reason"))
                            .filter(|r| !r.is_empty())
                            .unwrap_or("blocked by a hook")
                            .to_string(),
                    );
                }
                let hso = obj.get("hookSpecificOutput");
                if s(hso.and_then(|h| h.get("permissionDecision"))) == Some("deny") {
                    return Verdict::Block(
                        s(hso.and_then(|h| h.get("permissionDecisionReason")))
                            .filter(|r| !r.is_empty())
                            .unwrap_or("denied by a hook")
                            .to_string(),
                    );
                }
                let ctx = s(hso.and_then(|h| h.get("additionalContext")))
                    .filter(|c| !c.is_empty())
                    .map(str::to_string);
                return Verdict::Proceed(ctx);
            }
            if event == Event::SessionStart && !stdout.is_empty() {
                return Verdict::Proceed(Some(stdout.to_string()));
            }
            Verdict::Proceed(None)
        }
        2 => {
            let reason = run.stderr.trim();
            Verdict::Block(if reason.is_empty() {
                "blocked by a hook (exit 2)".to_string()
            } else {
                reason.to_string()
            })
        }
        code => Verdict::Error(format!(
            "exited {code}: {}",
            crate::text::head(run.stderr.trim(), 200)
        )),
    }
}

/// Run one hook command through the platform shell with `input` on stdin.
pub fn run_hook(command: &str, cwd: &Path, input: &str, timeout: Duration) -> HookRun {
    use std::process::{Command, Stdio};
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    for var in crate::config::API_KEY_ENVS {
        c.env_remove(var);
    }
    c.current_dir(cwd)
        .env("RUFOUS_PROJECT_DIR", cwd)
        .env("BOUBOU_PROJECT_DIR", cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(e) => {
            return HookRun {
                code: 1,
                stdout: String::new(),
                stderr: format!("spawn failed: {e}"),
                timed_out: false,
            }
        }
    };
    let pid = child.id();
    let feed = child.stdin.take().map(|mut stdin| {
        let text = input.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        })
    });
    let reader = |pipe: Option<_>| {
        pipe.map(|mut p: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = p.read_to_end(&mut buf);
                crate::text::decode_console(&buf)
            })
        })
    };
    let out_t = reader(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let err_t = reader(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let mut timed_out = false;
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(1),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    timed_out = true;
                    crate::exec::kill_tree(pid);
                    let _ = child.wait();
                    break 1;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break 1,
        }
    };
    if let Some(f) = feed {
        let _ = f.join();
    }
    let stdout = out_t.and_then(|t| t.join().ok()).unwrap_or_default();
    let stderr = err_t.and_then(|t| t.join().ok()).unwrap_or_default();
    HookRun {
        code,
        stdout,
        stderr,
        timed_out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(code: i32, stdout: &str, stderr: &str) -> HookRun {
        HookRun {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            timed_out: false,
        }
    }

    #[test]
    fn the_protocol_is_read_as_the_other_harnesses_write_it() {
        assert_eq!(
            interpret(Event::PreToolUse, &run(0, "", "")),
            Verdict::Proceed(None)
        );
        assert_eq!(
            interpret(
                Event::PreToolUse,
                &run(0, r#"{"decision":"block","reason":"no"}"#, "")
            ),
            Verdict::Block("no".into())
        );
        assert_eq!(
            interpret(
                Event::PreToolUse,
                &run(
                    0,
                    r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"not that file"}}"#,
                    ""
                )
            ),
            Verdict::Block("not that file".into())
        );
        assert_eq!(
            interpret(
                Event::SessionStart,
                &run(
                    0,
                    r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"ctx"}}"#,
                    ""
                )
            ),
            Verdict::Proceed(Some("ctx".into()))
        );
        // An allow is not a bypass: it is an ordinary proceed.
        assert_eq!(
            interpret(
                Event::PreToolUse,
                &run(
                    0,
                    r#"{"hookSpecificOutput":{"permissionDecision":"allow"}}"#,
                    ""
                )
            ),
            Verdict::Proceed(None)
        );
        // Plain stdout is context at SessionStart only.
        assert_eq!(
            interpret(Event::SessionStart, &run(0, "remember the plan\n", "")),
            Verdict::Proceed(Some("remember the plan".into()))
        );
        assert_eq!(
            interpret(Event::PostToolUse, &run(0, "chatter", "")),
            Verdict::Proceed(None)
        );
        assert_eq!(
            interpret(Event::Stop, &run(2, "", "finish the checklist\n")),
            Verdict::Block("finish the checklist".into())
        );
        assert_eq!(
            interpret(Event::Stop, &run(2, "", "")),
            Verdict::Block("blocked by a hook (exit 2)".into())
        );
        assert!(
            matches!(interpret(Event::Stop, &run(1, "", "boom")), Verdict::Error(m) if m == "exited 1: boom")
        );
        let mut t = run(0, "", "");
        t.timed_out = true;
        assert_eq!(
            interpret(Event::Stop, &t),
            Verdict::Error("timed out".into())
        );
    }

    #[test]
    fn the_context_block_is_capped() {
        assert_eq!(prompt_context(&[]), None);
        assert_eq!(prompt_context(&[" ".into()]), None);
        assert_eq!(
            prompt_context(&["a".into(), "b".into()]).as_deref(),
            Some("a\n\nb")
        );
        let long = "x".repeat(CONTEXT_CHARS + 10);
        let c = prompt_context(&[long]).expect("some");
        assert!(c.ends_with(&format!(
            "[hook context truncated at {CONTEXT_CHARS} characters]"
        )));
    }
}
