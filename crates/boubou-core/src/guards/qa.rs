//! `_is_qa` / `_has_task_progress` / `_task_original_text`: telling a question or
//! greeting from a task, and finding the real user instruction among harness notes.

use std::sync::LazyLock;

use regex::Regex;

use crate::ollama::{Message, Role};
use crate::text;
use crate::tools::is_mcp_tool;

/// Marker prefix of the parallel-dispatch receipt message.
pub const DISPATCH_NOTE_MARK: &str = "[parallel-dispatch]";

/// Every user-role message the harness injects starts with one of these, so routing,
/// Q&A detection and task re-injection can skip them. Invariant: any new injected
/// message must start with a prefix from this list.
pub const HARNESS_NOTE_PREFIXES: [&str; 10] = [
    "⚠️",
    "Continue:",
    "[blocked:",
    "Your answer is already sufficient",
    "⏱",
    "Hint:",
    "Continue the previous conversation",
    DISPATCH_NOTE_MARK,
    FOLLOW_UP_DONE_NOTE,
    FOLLOW_UP_REPORT_NOTE,
];

/// Follow-up turn, model went quiet after acting: ask for the report, not for work.
/// Listed above so the decider, the session replay and the web history skip it; before
/// that (review 2026-10-29) it passed for the user's latest input.
pub const FOLLOW_UP_DONE_NOTE: &str =
    "If the user's instruction is done, say so in one line and stop. Otherwise make the next tool call.";
/// Follow-up turn, acted on and reported: the turn ends here.
pub const FOLLOW_UP_REPORT_NOTE: &str =
    "Your reply reports the follow-up; the turn ends here. Do not repeat the output.";

/// Marker of a compaction summary message (`history.rs`).
pub const CONTEXT_SUMMARY_MARK: &str = "[Earlier context summary]";

pub fn is_harness_note(text: &str) -> bool {
    HARNESS_NOTE_PREFIXES.iter().any(|p| text.starts_with(p)) || text.contains(CONTEXT_SUMMARY_MARK)
}

/// Action verbs and task-shaped phrases. Matched on word boundaries after lower-casing
/// (`hint_hit`), so `fix` does not hit `prefix`; a trailing space in a hint is trimmed
/// before matching and only documents that the word is a verb here.
const TASK_HINTS: &[&str] = &[
    "write ",
    "create ",
    "make ",
    "build ",
    "edit ",
    "modify",
    "change ",
    "delete ",
    "remove ",
    "run ",
    "execute",
    "implement",
    "fix ",
    "debug",
    "refactor",
    "generate",
    "count ",
    "translate",
    "summarise",
    "summarize",
    "compare",
    "test ",
    "install",
    "download",
    "deploy",
    "set up",
    "configure",
    "start ",
    "stop ",
    "tidy",
    "organise",
    "organize",
    "convert",
    "scrape",
    "crawl",
    "optimise",
    "optimize",
    "check ",
    "analyse",
    "analyze",
    "design ",
    "audit ",
    "review ",
    "clean ",
    "compute",
    "calculate",
    "export",
    "import ",
    "plot ",
    "profile ",
    "produce ",
    "research ",
    "rename",
    "save ",
    "search for",
    "find all",
    "list all",
    "step 1",
    "required steps",
    "plan at least",
    "todo(action",
    "the code",
    "a program",
    "a script",
    "help me do",
];

/// Question and small-talk cues, matched on word boundaries. A `?` counts only when it
/// ends the message (`ends_with_question`): benchmark-style tasks ("How many hours did it
/// take? Output just the number") end in a full stop and must keep their mutating tools.
const QA_HINTS: &[&str] = &[
    "are you ",
    "do you ",
    "is there ",
    "what is ",
    "what are ",
    "what's",
    "what does",
    "how do i",
    "how does ",
    "how can i",
    "how to ",
    "tell me ",
    "describe ",
    "explanation",
    "why is ",
    "why does ",
    "why do ",
    "explain",
    "difference between",
    "can you explain",
    "tell me about",
    "who is ",
    "who are you",
    "when was ",
    "where is ",
    "is it possible",
];

/// Greetings and sign-offs. They decide only a short message (`GREETING_MAX_CHARS`): a task
/// that mentions `hello.txt` or ends with "thanks" is still a task, which the plain
/// word-boundary match used to miss (three labelled cases).
const GREETING_HINTS: &[&str] = &[
    "hi",
    "hello",
    "hey",
    "thanks",
    "thank you",
    "goodbye",
    "bye",
    "good morning",
    "good night",
    "are you there",
    "who are you",
];
pub const GREETING_MAX_CHARS: usize = 40;

/// Request openers that say nothing about intent: "Could you add a retry?" asks for the
/// retry, "Can you explain the decider?" asks for an explanation. The opener is dropped and
/// the rest of the message is classified; with no cue in the rest, a question mark after an
/// opener is a request, not a question. (Mingbird read `can you` as a question, which sent
/// polite task requests to the Q&A route, where the writing tools are off.)
const POLITE_OPENERS: &[&str] = &[
    "can you please ",
    "could you please ",
    "would you please ",
    "would you mind ",
    "can you ",
    "could you ",
    "would you ",
    "will you ",
    "please ",
];

/// The message without a leading polite opener, and the opener if there was one.
fn strip_opener(low: &str) -> (Option<&'static str>, &str) {
    let s = low.trim_start_matches(|c: char| !c.is_alphanumeric());
    for o in POLITE_OPENERS {
        if let Some(rest) = s.strip_prefix(o) {
            return (Some(o), rest);
        }
    }
    (None, low)
}

/// Hints match on word boundaries so `hi` does not hit `this` and `fix` does not hit
/// `prefix`; a hint with no alphanumeric character (none today) would match as a substring.
fn hint_hit(hint: &str, low: &str) -> bool {
    static CACHE: LazyLock<std::collections::HashMap<&'static str, Regex>> = LazyLock::new(|| {
        TASK_HINTS
            .iter()
            .chain(QA_HINTS.iter())
            .chain(GREETING_HINTS.iter())
            .filter(|h| h.chars().any(char::is_alphanumeric))
            .map(|h| {
                let re = Regex::new(&format!(r"\b{}\b", regex::escape(h.trim()))).expect("regex");
                (*h, re)
            })
            .collect()
    });
    if !hint.chars().any(char::is_alphanumeric) {
        return low.contains(hint);
    }
    CACHE.get(hint).is_some_and(|re| re.is_match(low))
}

/// A message whose last non-blank character is a question mark, ignoring trailing quotes
/// and brackets. "Are you capable of software development ?" is a
/// question even with the space before the mark.
pub fn ends_with_question(text: &str) -> bool {
    text.trim_end()
        .trim_end_matches(['"', '\'', ')', ']', '*', '_'])
        .trim_end()
        .ends_with('?')
}

/// `_has_task_progress`: has this conversation already done real work (a mutating or
/// MCP tool call)? Read-only tools do not count: they appear in Q&A too.
pub fn has_task_progress(messages: &[Message]) -> bool {
    messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| m.tool_calls.iter())
        .any(|tc| {
            let n = tc.function.name.as_str();
            matches!(
                n,
                "create_file" | "edit_file" | "append_file" | "delete_file" | "run_bash"
            ) || is_mcp_tool(n)
        })
}

/// What the latest real user message is doing. `Question` changes the loop's path (the Q&A
/// prompt and tool set). `FollowUp` runs the task loop but ends it once the model has acted
/// and then reports in text (bead boubou-he2); `Task` runs until `finish`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Question,
    Task,
    FollowUp,
}

impl Intent {
    pub fn as_str(self) -> &'static str {
        match self {
            Intent::Question => "question",
            Intent::Task => "task",
            Intent::FollowUp => "follow_up",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "question" => Some(Intent::Question),
            "task" => Some(Intent::Task),
            "follow_up" | "followup" | "follow-up" => Some(Intent::FollowUp),
            _ => None,
        }
    }
}

/// The heuristic's intent verdict with the honesty the decider layer needs: which rule
/// fired, and 1.0 confidence on a rule hit against 0.5 on a fall-through default.
#[derive(Debug, Clone, PartialEq)]
pub struct HeuristicIntent {
    pub intent: Intent,
    pub confidence: f64,
    /// The rule that decided, for the log and the labelled set.
    pub rule: String,
    /// The message classified (empty when there was no real user message).
    pub text: String,
    pub has_progress: bool,
}

/// Longest cue-free message after progress that still reads as a follow-up (boubou-he2).
pub const FOLLOWUP_MAX_CHARS: usize = 80;

/// `_is_qa` as a classifier. Task words win, then question words or a trailing `?`, then
/// "short and no action verb" is a question unless the conversation already has progress
/// (then it is a follow-up instruction); anything longer with no cue defaults to a task.
pub fn intent_heuristic(messages: &[Message], has_progress: bool) -> HeuristicIntent {
    let verdict = |intent, confidence, rule: String, text: &str| HeuristicIntent {
        intent,
        confidence,
        rule,
        text: text.to_string(),
        has_progress,
    };
    for m in messages.iter().rev() {
        if m.role != Role::User {
            continue;
        }
        let t = m.content.trim();
        if t.is_empty() || is_harness_note(t) {
            continue;
        }
        let low = t.to_lowercase();
        let (opener, body) = strip_opener(&low);
        if let Some(h) = TASK_HINTS.iter().find(|h| hint_hit(h, body)) {
            return verdict(Intent::Task, 1.0, format!("task hint `{}`", h.trim()), t);
        }
        if let Some(h) = QA_HINTS.iter().find(|h| hint_hit(h, body)) {
            return verdict(
                Intent::Question,
                1.0,
                format!("question hint `{}`", h.trim()),
                t,
            );
        }
        if text::clen(t) <= GREETING_MAX_CHARS {
            if let Some(h) = GREETING_HINTS.iter().find(|h| hint_hit(h, &low)) {
                return verdict(Intent::Question, 1.0, format!("greeting `{}`", h.trim()), t);
            }
        }
        if ends_with_question(t) {
            return match opener {
                Some(o) => verdict(
                    Intent::Task,
                    0.7,
                    format!("polite request `{}` with `?`", o.trim()),
                    t,
                ),
                None => verdict(Intent::Question, 1.0, "trailing `?`".to_string(), t),
            };
        }
        // After real work, a cue-free message up to a sentence or two steers that work
        // ("I think we already did that one. It can be closed.", 51 chars, bead boubou-he2);
        // before any work only a short one reads as a greeting or question.
        if has_progress && text::clen(t) <= FOLLOWUP_MAX_CHARS {
            return verdict(
                Intent::FollowUp,
                0.5,
                "default: short message after progress".to_string(),
                t,
            );
        }
        if text::clen(t) <= 40 {
            return if has_progress {
                verdict(
                    Intent::FollowUp,
                    0.5,
                    "default: short message after progress".to_string(),
                    t,
                )
            } else {
                verdict(
                    Intent::Question,
                    0.5,
                    "default: short message, no progress".to_string(),
                    t,
                )
            };
        }
        return verdict(
            Intent::Task,
            0.5,
            "default: no cue in a long message".to_string(),
            t,
        );
    }
    verdict(
        Intent::Task,
        0.5,
        "default: no user message".to_string(),
        "",
    )
}

/// `_is_qa`: is the latest real user message a question/greeting rather than a task?
/// Thin wrapper over [`intent_heuristic`]; the loop goes through `decider::LayeredDecider`.
pub fn is_qa(messages: &[Message], has_progress: bool) -> bool {
    intent_heuristic(messages, has_progress).intent == Intent::Question
}

/// `_task_original_text`: the instruction this delivery answers = the most recent real
/// user input, skipping harness notes and compaction summaries. Long texts keep head and
/// tail halves with an elision marker.
pub fn task_original_text(messages: &[Message], max_chars: usize) -> String {
    for m in messages.iter().rev() {
        if m.role != Role::User {
            continue;
        }
        let t = m.content.trim();
        if t.is_empty() || is_harness_note(t) {
            continue;
        }
        if text::clen(t) <= max_chars {
            return t.to_string();
        }
        let half = max_chars / 2;
        return format!(
            "{}\n...(omitted)...\n{}",
            text::head(t, half),
            text::tail(t, half)
        );
    }
    String::new()
}

/// `_task_routing_text`: latest real user input for category routing (no truncation).
pub fn task_routing_text(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| {
            m.role == Role::User
                && !HARNESS_NOTE_PREFIXES
                    .iter()
                    .any(|p| m.content.starts_with(p))
        })
        .map(|m| m.content.clone())
        .unwrap_or_default()
}
