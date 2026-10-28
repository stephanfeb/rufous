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
pub const HARNESS_NOTE_PREFIXES: [&str; 8] = [
    "⚠️",
    "Continue:",
    "[blocked:",
    "Your answer is already sufficient",
    "⏱",
    "Hint:",
    "Continue the previous conversation",
    DISPATCH_NOTE_MARK,
];

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
    "can you ",
    "do you ",
    "will you ",
    "would you ",
    "is there ",
    "what is ",
    "what are ",
    "what's",
    "what does",
    "how do i",
    "how does ",
    "how can i",
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
    "could you",
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
];

/// Hints match on word boundaries so `hi` does not hit `this` and `fix` does not hit
/// `prefix`; a hint with no alphanumeric character (none today) would match as a substring.
fn hint_hit(hint: &str, low: &str) -> bool {
    static CACHE: LazyLock<std::collections::HashMap<&'static str, Regex>> = LazyLock::new(|| {
        TASK_HINTS
            .iter()
            .chain(QA_HINTS.iter())
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

/// A message whose last non-blank character is a question mark (ASCII or full-width),
/// ignoring trailing quotes and brackets. "Are you capable of software development ?" is a
/// question even with the space before the mark.
pub fn ends_with_question(text: &str) -> bool {
    text.trim_end()
        .trim_end_matches(['"', '\'', ')', ']', '*', '_'])
        .trim_end()
        .ends_with(['?', '\u{ff1f}'])
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

/// `_is_qa`: is the latest real user message a question/greeting rather than a task?
/// Task words win, then question words, then "short and no action verb" is Q&A unless
/// the conversation already has progress (then it is a follow-up instruction).
pub fn is_qa(messages: &[Message], has_progress: bool) -> bool {
    for m in messages.iter().rev() {
        if m.role != Role::User {
            continue;
        }
        let t = m.content.trim();
        if t.is_empty() || is_harness_note(t) {
            continue;
        }
        let low = t.to_lowercase();
        if TASK_HINTS.iter().any(|h| hint_hit(h, &low)) {
            return false;
        }
        if QA_HINTS.iter().any(|h| hint_hit(h, &low)) || ends_with_question(t) {
            return true;
        }
        if text::clen(t) <= 40 {
            return !has_progress;
        }
        return false;
    }
    false
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
