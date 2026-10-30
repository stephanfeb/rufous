//! Idle-streak guard (bead boubou-9c5): a bound on consecutive tool calls that change
//! nothing. Observed twice on 2026-10-29 in an editor session with a 9B model: a question
//! turn made 31 read-only calls before answering, and a follow-up turn whose request was
//! done after three calls went on for forty more reads, listings and `find`s around two
//! empty directories. The repeat guards never fire on that shape: every call is different.
//!
//! An idle call is one that reads: `read_file`, `list_dir`, `search_files`, the web tools,
//! `todo`, any other non-producing tool, and `run_bash` with a read-only command
//! ([`crate::guards::integrity::is_read_only_command`]). Writing, editing or deleting a
//! file, running any other command, or `finish` ends the streak.
//!
//! Question route: a nudge at [`QA_IDLE_NUDGE`], then from [`QA_IDLE_CAP`] every further
//! non-finish call is refused and the turn ends the way a text answer does. Task route: a
//! nudge at [`TASK_IDLE_NUDGE`] and again at [`TASK_IDLE_NUDGE_2`], nothing refused, so a
//! long read of a codebase before the first edit is still possible.

use serde_json::{Map, Value};

use super::integrity::is_read_only_command;

/// Question route: nudge after this many consecutive idle calls.
pub const QA_IDLE_NUDGE: usize = 8;
/// Question route: refuse further non-finish calls from this many.
pub const QA_IDLE_CAP: usize = 12;
/// Task route: first nudge.
pub const TASK_IDLE_NUDGE: usize = 12;
/// Task route: second (last) nudge.
pub const TASK_IDLE_NUDGE_2: usize = 24;

/// Tools whose successful call counts as progress whatever the arguments.
const PRODUCING: [&str; 5] = [
    "create_file",
    "edit_file",
    "append_file",
    "delete_file",
    "finish",
];

/// Whether a built-in tool call left the world as it was. MCP tools are judged by the
/// caller (`face::is_mcp_producing`).
pub fn is_idle_call(name: &str, args: &Map<String, Value>) -> bool {
    if PRODUCING.contains(&name) {
        return false;
    }
    if name == "run_bash" {
        let command = args.get("command").and_then(Value::as_str).unwrap_or("");
        return is_read_only_command(command);
    }
    true
}

/// The question-route nudge.
pub fn qa_nudge(k: usize) -> String {
    format!("⚠️ You have made {k} read-only calls on this question without answering. Answer now with what you have. If the answer needs something you could not find or run, say what it is and which command the user should run.")
}

/// The refusal text once the question-route cap is reached.
pub fn qa_cap_result(name: &str, k: usize) -> String {
    format!("[blocked: {k} read-only calls on a question; the answer is due. {name} will not run. Reply in text with what you have.]")
}

/// The task-route nudge.
pub fn task_nudge(k: usize) -> String {
    format!("⚠️ The last {k} calls only read (files, listings, searches, read-only commands); nothing was written, edited or run that changes anything. If the user's instruction is already satisfied, call finish now with a one-line summary. Otherwise do the first real step now: write or edit the file, or run the command. If something blocks you, call finish and say what it is.")
}

/// Two web searches in a row that found nothing relevant (the `web_search` no-result text).
pub const NO_RESULTS_MARK: &str = "[web_search: no relevant results";

pub const NO_RESULTS_NOTE: &str = "⚠️ Two web searches in a row found nothing relevant. The web is not where this answer is: use the files and commands in the workspace, or answer with what you have.";
