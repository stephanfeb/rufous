//! Isolated corrector: one fresh-context call to the same model when a tool call has failed
//! twice with identical arguments, before the anti-loop ladder disables the tool.
//!
//! Boubou addition (bead boubou-65d.7), after "Three Roles, One Model": a model that keeps
//! re-issuing a failing call is copying its own history; the same model, shown only the task,
//! the call and the error, either produces a different call or says the step is blocked. The
//! loop probe (bench/probes.py, gemma4:e2b n=10) showed every trial re-running `python fetch.py`
//! into a 503, 4/10 then inventing dataset.csv and 5/10 ending by the dead-loop guard. The
//! corrector's job is the exit path: a corrected call that runs, or an explicit, honest give-up
//! that the finish gates then let through.
//!
//! Rules: at most one consultation per failing signature and three per run; the corrector never
//! sees the conversation; a give-up blocks the identical call for the rest of the run.

use serde_json::Value;

use super::chat::{ChatBackend, ChatResponse, StreamPiece};
use crate::guards::leak;
use crate::ollama::Message;
use crate::text;

/// What the corrector decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// A different call to try now.
    Retry {
        name: String,
        args: Value,
        reason: String,
    },
    /// The step cannot be completed from here; stop retrying and report it.
    GiveUp { reason: String },
    /// No usable answer (empty, the same call again, or free text with no decision).
    Unclear,
}

/// Failures of one signature before the corrector is consulted.
pub const CONSULT_AT: u32 = 2;
/// Consultations per run.
pub const MAX_PER_RUN: u32 = 3;

const SYSTEM: &str = "You are the corrector for an autonomous coding agent. One of its tool calls has failed repeatedly with identical arguments. You see only the task, the failing call and the error, not the agent's conversation. Decide one of two things:\n1. A DIFFERENT call could succeed (wrong path, wrong flag, wrong interpreter, missing quoting): make that call now with the tool provided. Never repeat the failing call unchanged.\n2. No call from here can fix it (the error comes from the environment: a network or service error such as 503 or connection refused, a missing input the agent cannot obtain, denied permissions): reply in plain text, one line, starting with GIVE UP: and the reason.\nNever invent, simulate or fabricate the missing input or result; an honest failure report is the correct outcome in that case.";

/// The two-message context the corrector gets: no history, only the facts.
pub fn prompt(task: &str, name: &str, args: &Value, result: &str, fails: u32) -> Vec<Message> {
    let user = format!(
        "TASK (head):\n{}\n\nFAILING CALL ({fails} identical attempts): {name} {}\n\nLAST RESULT:\n{}\n\nEither make a different tool call that could succeed, or reply `GIVE UP: <reason>`.",
        text::head(task, 1200),
        text::head(&args.to_string(), 600),
        text::head(result, 800)
    );
    vec![Message::system(SYSTEM), Message::user(user)]
}

/// Tools that only look: proposing one of these is not a fix for a failing call.
const LOOKING_TOOLS: [&str; 10] = [
    "list_dir",
    "read_file",
    "search_files",
    "web_search",
    "web_search_multi",
    "web_fetch",
    "memory_recall",
    "todo",
    "skills",
    "enable_tools",
];

/// A proposed call is a retry only if it can change the outcome: the same tool with other
/// arguments, or a producing tool. A looking-only proposal (probes_corrector t05: `list_dir`
/// for a 503 from fetch.py, after which the model ran fetch.py five more times) means the
/// corrector found no fix, and that is a give-up.
fn retry_or_give_up(name: String, args: Value, reason: String, failing: (&str, &Value)) -> Verdict {
    if name == failing.0 && &args == failing.1 {
        return Verdict::Unclear;
    }
    if name != failing.0 && LOOKING_TOOLS.contains(&name.as_str()) {
        return Verdict::GiveUp {
            reason: format!(
                "no corrected call was found; the corrector only proposed {name}, which cannot change the outcome of {}",
                failing.0
            ),
        };
    }
    Verdict::Retry { name, args, reason }
}

/// Read the corrector's reply. `failing` is the (name, args) it must not repeat.
pub fn parse_verdict(resp: &ChatResponse, failing: (&str, &Value)) -> Verdict {
    let content = resp.message.content.trim();
    let reason_of = |s: &str| -> String {
        let s = s.trim();
        let s = s
            .strip_prefix("GIVE UP:")
            .or_else(|| s.strip_prefix("GIVE UP"))
            .unwrap_or(s)
            .trim_start_matches([':', ' ', '-']);
        text::head(s.lines().next().unwrap_or("").trim(), 200).to_string()
    };
    // A native tool call wins.
    if let Some(tc) = resp.message.tool_calls.first() {
        let f = &tc.function;
        return retry_or_give_up(
            f.name.clone(),
            f.arguments.clone(),
            reason_of(content),
            failing,
        );
    }
    let low = content.to_lowercase();
    if low.starts_with("give up") || low.contains("give up:") {
        let line = content
            .lines()
            .find(|l| l.to_lowercase().contains("give up"))
            .unwrap_or(content);
        return Verdict::GiveUp {
            reason: reason_of(line),
        };
    }
    // A call written as text (small models leak the call format).
    if let Some(calls) = leak::try_parse_tool_calls(content) {
        if let Some((cname, cargs)) = calls.first() {
            return retry_or_give_up(
                cname.clone(),
                Value::Object(cargs.clone()),
                String::new(),
                failing,
            );
        }
    }
    // Phrases that say the failure is outside the agent's reach. The bare word
    // `environment` is not one: "check the environment variable" is a retry hint, and a
    // give-up blocks the call for the rest of the run.
    const BLOCKED: &[&str] = &[
        "cannot be fixed",
        "can't be fixed",
        "not fixable",
        "unreachable",
        "service unavailable",
        "no command will",
        "out of the agent's control",
        "environment problem",
        "environment error",
        "environment issue",
        "environmental",
    ];
    if !low.is_empty() && BLOCKED.iter().any(|w| low.contains(w)) {
        return Verdict::GiveUp {
            reason: reason_of(content),
        };
    }
    Verdict::Unclear
}

/// The repeated failure the corrector is asked about.
#[derive(Debug, Clone, Copy)]
pub struct FailedCall<'a> {
    pub name: &'a str,
    pub args: &'a Value,
    pub result: &'a str,
    pub fails: u32,
}

/// One isolated call. Streaming tokens are discarded: the verdict is for the loop, not the
/// user. A transport error is an `Unclear` verdict, never a loop error.
pub fn consult<B: ChatBackend + ?Sized>(
    backend: &mut B,
    model: &str,
    task: &str,
    failed: FailedCall<'_>,
    tools: &[Value],
) -> Verdict {
    let msgs = prompt(task, failed.name, failed.args, failed.result, failed.fails);
    match backend.chat(model, &msgs, tools, &mut |_p: StreamPiece<'_>| {}) {
        Ok(r) => parse_verdict(&r, (failed.name, failed.args)),
        Err(_) => Verdict::Unclear,
    }
}

/// The note the loop injects after a give-up: the honest exit, spelled out.
pub fn give_up_note(
    name: &str,
    args: &Value,
    fails: u32,
    reason: &str,
    missing: &[String],
) -> String {
    let files = if missing.is_empty() {
        String::new()
    } else {
        format!(
            " The task names {} and they do not exist: say so by name in the deliverable and in the summary (for example \"{}: could not be produced, {}\").",
            missing.join(", "),
            missing[0],
            text::head(reason, 80)
        )
    };
    format!(
        "⚠️ Corrector verdict: `{name} {}` has failed {fails} times with the same error and cannot be fixed from here ({reason}). Do NOT run it again (identical calls are now blocked) and do NOT create the missing input yourself: an invented file is worse than none. Honest exit: (1) write the deliverable the task asks for, stating plainly which step failed and the error; (2) call finish with a summary that says the same.{files} Steps that failed stay unticked in the plan; that is fine.",
        text::head(&args.to_string(), 120)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn failing() -> (String, Value) {
        (
            "run_bash".to_string(),
            json!({"command": "python fetch.py"}),
        )
    }

    #[test]
    fn prompt_has_no_history_and_carries_the_facts() {
        let (n, a) = failing();
        let m = prompt(
            "Run fetch.py then write summary.md",
            &n,
            &a,
            "[exit 1]\nConnectionError: 503",
            2,
        );
        assert_eq!(m.len(), 2);
        assert!(m[0].content.contains("GIVE UP:"));
        assert!(m[1].content.contains("Run fetch.py"));
        assert!(m[1].content.contains("python fetch.py"));
        assert!(m[1].content.contains("503"));
        assert!(m[1].content.contains("2 identical attempts"));
    }

    #[test]
    fn native_call_is_a_retry_unless_identical() {
        let (n, a) = failing();
        let r = ChatResponse::call("", "run_bash", json!({"command": "python3 fetch.py"}));
        match parse_verdict(&r, (&n, &a)) {
            Verdict::Retry { name, args, .. } => {
                assert_eq!(name, "run_bash");
                assert_eq!(args["command"], "python3 fetch.py");
            }
            v => panic!("{v:?}"),
        }
        let same = ChatResponse::call("", "run_bash", json!({"command": "python fetch.py"}));
        assert_eq!(parse_verdict(&same, (&n, &a)), Verdict::Unclear);
        // probes_corrector t05: list_dir is not a fix for a failing command.
        let look = ChatResponse::call("", "list_dir", json!({"path": "."}));
        match parse_verdict(&look, (&n, &a)) {
            Verdict::GiveUp { reason } => {
                assert!(reason.contains("only proposed list_dir"), "{reason}")
            }
            v => panic!("{v:?}"),
        }
        // a producing tool is a legitimate change of approach
        let write = ChatResponse::call("", "create_file", json!({"path": "x", "content": "y"}));
        assert!(matches!(
            parse_verdict(&write, (&n, &a)),
            Verdict::Retry { .. }
        ));
    }

    #[test]
    fn give_up_text_is_parsed_with_its_reason() {
        let (n, a) = failing();
        let r = ChatResponse::text(
            "GIVE UP: the mirror returns HTTP 503; no command change fixes a remote outage.",
        );
        assert_eq!(
            parse_verdict(&r, (&n, &a)),
            Verdict::GiveUp {
                reason: "the mirror returns HTTP 503; no command change fixes a remote outage."
                    .into()
            }
        );
        let r = ChatResponse::text(
            "The data mirror is unreachable (503). This is an environment problem.",
        );
        assert!(matches!(
            parse_verdict(&r, (&n, &a)),
            Verdict::GiveUp { .. }
        ));
        assert_eq!(
            parse_verdict(&ChatResponse::text(""), (&n, &a)),
            Verdict::Unclear
        );
        assert_eq!(
            parse_verdict(&ChatResponse::text("Let me think about this."), (&n, &a)),
            Verdict::Unclear
        );
        // Mentioning the environment while suggesting a fix is not giving up.
        assert_eq!(
            parse_verdict(
                &ChatResponse::text(
                    "The environment variable PYTHONPATH does not include src; run it as python3 -m src.fetch instead."
                ),
                (&n, &a)
            ),
            Verdict::Unclear
        );
    }

    #[test]
    fn leaked_text_call_is_a_retry() {
        let (n, a) = failing();
        let r = ChatResponse::text(
            r#"{"name": "run_bash", "arguments": {"command": "python3 fetch.py"}}"#,
        );
        assert!(matches!(parse_verdict(&r, (&n, &a)), Verdict::Retry { .. }));
    }

    #[test]
    fn give_up_note_names_the_missing_files_and_forbids_fabrication() {
        let (n, a) = failing();
        let note = give_up_note(&n, &a, 2, "HTTP 503", &["dataset.csv".to_string()]);
        assert!(note.starts_with("⚠️"));
        assert!(note.contains("dataset.csv: could not be produced"));
        assert!(note.contains("do NOT create the missing input"));
        assert!(note.contains("stay unticked"));
    }
}
