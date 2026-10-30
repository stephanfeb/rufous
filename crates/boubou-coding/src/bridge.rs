//! The pure mapping from the loop's [`AgentEvent`] stream to ACP `session/update`
//! notifications. No I/O, no connection: [`TurnBridge::on_event`] returns the updates to
//! send, so the mapping is unit-tested on its own and the server stays a thin shell.
//!
//! Vocabulary (ACP v1):
//! - streamed text → `agent_message_chunk`, thinking → `agent_thought_chunk`;
//! - a tool call → `tool_call` (kind from the tool name, status in_progress, raw input,
//!   file locations), its result → `tool_call_update` (completed or failed, output as
//!   content, a diff for file edits);
//! - the plan (`todo.json`) → `plan` after every `todo` call and at the finish;
//! - context usage → `usage_update`; compaction and errors → `notice`;
//! - `finish` is not shown as a tool call: its summary becomes the closing message when
//!   the model did not already state the outcome in prose after its last tool call;
//! - a saved history → the same vocabulary replayed for `session/load`
//!   ([`replay_updates`]): user turns as `user_message_chunk`, harness notes dropped.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Diff, EmbeddedResourceResource, Notice, NoticeSeverity, Plan,
    PlanEntry, PlanEntryPriority, PlanEntryStatus, SessionUpdate, StopReason, ToolCall,
    ToolCallContent, ToolCallId, ToolCallLocation, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind, UsageUpdate,
};
use boubou_kernel::agent::AgentEvent;
use boubou_kernel::ollama::{Message, Role};
use boubou_kernel::todo::TodoItem;
use serde_json::Value;

/// One in-flight tool call: the loop's `ToolResult` carries name and args, not an id, so
/// results are matched to calls FIFO by (name, args).
#[derive(Debug, Clone)]
struct Pending {
    id: ToolCallId,
    name: String,
    args: Value,
}

/// Per-turn state of the mapping.
#[derive(Debug)]
pub struct TurnBridge {
    cwd: PathBuf,
    /// Where the loop keeps `todo.json` (the session's harness directory, or the cwd).
    plan_dir: PathBuf,
    next_call: u64,
    pending: VecDeque<Pending>,
    /// Whether the model has written prose since its last tool call (then the finish
    /// summary would repeat it).
    text_since_tool: bool,
    /// Whether a plan was ever sent (an emptied plan is then sent once more, empty).
    plan_shown: bool,
    max_iterations: bool,
}

impl TurnBridge {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        let cwd: PathBuf = cwd.into();
        Self {
            plan_dir: cwd.clone(),
            cwd,
            next_call: 0,
            pending: VecDeque::new(),
            text_since_tool: false,
            plan_shown: false,
            max_iterations: false,
        }
    }

    /// Read the plan from `dir` instead of the cwd (`session::harness_dir`).
    pub fn with_plan_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.plan_dir = dir.into();
        self
    }

    /// The id of the tool call currently running (the subject of a permission request).
    pub fn current_call(&self) -> Option<ToolCallId> {
        self.pending.back().map(|p| p.id.clone())
    }

    /// Why the turn stopped, from what the loop reported.
    pub fn stop_reason(&self, cancelled: bool) -> StopReason {
        if cancelled {
            StopReason::Cancelled
        } else if self.max_iterations {
            StopReason::MaxTurnRequests
        } else {
            StopReason::EndTurn
        }
    }

    /// Map one loop event to zero or more session updates.
    pub fn on_event(&mut self, ev: AgentEvent) -> Vec<SessionUpdate> {
        match ev {
            AgentEvent::Text(t) => {
                if t.is_empty() {
                    return vec![];
                }
                self.text_since_tool = true;
                vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                    t.into(),
                ))]
            }
            AgentEvent::Thinking(t) => {
                if t.is_empty() {
                    return vec![];
                }
                vec![SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                    t.into(),
                ))]
            }
            AgentEvent::ToolCall(tc) => {
                let name = tc.function.name;
                let args = tc.function.arguments;
                if name == "finish" {
                    return vec![];
                }
                self.text_since_tool = false;
                self.next_call += 1;
                let id = ToolCallId::new(format!("call_{}", self.next_call));
                let mut call = ToolCall::new(id.clone(), title_for(&name, &args))
                    .name(name.clone())
                    .kind(kind_for(&name))
                    .status(ToolCallStatus::InProgress)
                    .raw_input(args.clone());
                let locations = locations_for(&name, &args, &self.cwd);
                if !locations.is_empty() {
                    call = call.locations(locations);
                }
                self.pending.push_back(Pending { id, name, args });
                vec![SessionUpdate::ToolCall(call)]
            }
            AgentEvent::ToolResult {
                name,
                args,
                output,
                blocked,
            } => {
                if name == "finish" {
                    return vec![];
                }
                let Some(p) = self.take_pending(&name, &args) else {
                    return vec![];
                };
                let failed = blocked || boubou_kernel::agent::run::is_tool_error(&output);
                let mut content: Vec<ToolCallContent> = Vec::new();
                if let Some(diff) = diff_for(&p.name, &p.args, &self.cwd) {
                    if !failed {
                        content.push(ToolCallContent::Diff(diff));
                    }
                }
                if !output.trim().is_empty() {
                    content.push(ToolCallContent::from(output.clone()));
                }
                let fields = ToolCallUpdateFields::new()
                    .status(if failed {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    })
                    .content(content)
                    .raw_output(Value::String(output));
                let mut out = vec![SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    p.id, fields,
                ))];
                if p.name == "todo" {
                    out.extend(self.plan_update());
                }
                out
            }
            AgentEvent::Finished { summary } => {
                let mut out = self.plan_update();
                let s = summary.trim();
                if !self.text_since_tool && !s.is_empty() {
                    out.push(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                        s.to_string().into(),
                    )));
                }
                out
            }
            AgentEvent::MaxIterations => {
                self.max_iterations = true;
                vec![SessionUpdate::Notice(Notice::new(
                    NoticeSeverity::Warning,
                    "Turn limit reached",
                ))]
            }
            AgentEvent::ContextUsage { used, limit } => {
                vec![SessionUpdate::UsageUpdate(UsageUpdate::new(
                    used,
                    u64::from(limit),
                ))]
            }
            AgentEvent::Compacted {
                level,
                before,
                after,
            } => vec![SessionUpdate::Notice(
                Notice::new(NoticeSeverity::Info, "Context compacted")
                    .description(format!("level {level}: {before} → {after} messages")),
            )],
            AgentEvent::Error(e) => {
                let title = boubou_kernel::text::head(&e, 80).to_string();
                vec![SessionUpdate::Notice(
                    Notice::new(NoticeSeverity::Error, title).description(e),
                )]
            }
            // Harness lines and parallel-dispatch progress are diagnostics, not conversation;
            // the server logs them to stderr.
            AgentEvent::Log(_) | AgentEvent::Dispatch(_) => vec![],
        }
    }

    fn take_pending(&mut self, name: &str, args: &Value) -> Option<Pending> {
        let pos = self
            .pending
            .iter()
            .position(|p| p.name == name && &p.args == args)
            .or_else(|| (!self.pending.is_empty()).then_some(0))?;
        self.pending.remove(pos)
    }

    /// The current `todo.json` as an ACP plan: done items completed, the first open item in
    /// progress, the rest pending. Nothing when there never was a plan.
    fn plan_update(&mut self) -> Vec<SessionUpdate> {
        let items = boubou_kernel::todo::load_todo(&self.plan_dir);
        if items.is_empty() && !self.plan_shown {
            return vec![];
        }
        self.plan_shown = true;
        vec![SessionUpdate::Plan(plan_from(&items))]
    }
}

/// A saved Boubou history as the `session/update` stream an editor would have seen, for
/// `session/load`. System messages and harness notes (continue nudges, plan reminders,
/// compaction summaries) are the loop's business and are not shown; the user's own turns
/// become `user_message_chunk`; assistant prose, tool calls and their results map as in
/// [`TurnBridge::on_event`], with ids `hist_N`; a call with no result on record is marked
/// failed so the editor does not wait on it; `finish` shows its summary only where the model
/// wrote no prose in the same message; the plan file in `cwd`, if any, closes the replay.
pub fn replay_updates(messages: &[Message], cwd: &Path, plan_dir: &Path) -> Vec<SessionUpdate> {
    let mut out: Vec<SessionUpdate> = Vec::new();
    let mut open: VecDeque<Pending> = VecDeque::new();
    let mut n = 0u64;
    for m in messages {
        match m.role {
            Role::System => {}
            Role::User => {
                let text = m.content.trim();
                if text.is_empty()
                    || boubou_kernel::guards::qa::is_harness_note(text)
                    || text.starts_with("[tool")
                {
                    continue;
                }
                out.push(SessionUpdate::UserMessageChunk(ContentChunk::new(
                    text.to_string().into(),
                )));
            }
            Role::Assistant => {
                let prose = m.content.trim();
                if !prose.is_empty() {
                    out.push(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                        prose.to_string().into(),
                    )));
                }
                for tc in &m.tool_calls {
                    let name = tc.function.name.as_str();
                    let args = &tc.function.arguments;
                    if name == "finish" {
                        let summary = args
                            .get("summary")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .unwrap_or("");
                        if prose.is_empty() && !summary.is_empty() {
                            out.push(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                summary.to_string().into(),
                            )));
                        }
                        continue;
                    }
                    n += 1;
                    let id = ToolCallId::new(format!("hist_{n}"));
                    let mut call = ToolCall::new(id.clone(), title_for(name, args))
                        .name(name.to_string())
                        .kind(kind_for(name))
                        .status(ToolCallStatus::InProgress)
                        .raw_input(args.clone());
                    let locations = locations_for(name, args, cwd);
                    if !locations.is_empty() {
                        call = call.locations(locations);
                    }
                    open.push_back(Pending {
                        id,
                        name: name.to_string(),
                        args: args.clone(),
                    });
                    out.push(SessionUpdate::ToolCall(call));
                }
            }
            Role::Tool => {
                let Some(p) = open.pop_front() else {
                    continue;
                };
                let output = m.content.clone();
                let failed = boubou_kernel::agent::run::is_tool_error(&output);
                let mut content: Vec<ToolCallContent> = Vec::new();
                if !failed {
                    if let Some(diff) = diff_for(&p.name, &p.args, cwd) {
                        content.push(ToolCallContent::Diff(diff));
                    }
                }
                if !output.trim().is_empty() {
                    content.push(ToolCallContent::from(output.clone()));
                }
                let fields = ToolCallUpdateFields::new()
                    .status(if failed {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    })
                    .content(content)
                    .raw_output(Value::String(output));
                out.push(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    p.id, fields,
                )));
            }
        }
    }
    for p in open {
        out.push(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            p.id,
            ToolCallUpdateFields::new().status(ToolCallStatus::Failed),
        )));
    }
    let items = boubou_kernel::todo::load_todo(plan_dir);
    if !items.is_empty() {
        out.push(SessionUpdate::Plan(plan_from(&items)));
    }
    out
}

/// `todo.json` items → ACP plan entries.
pub fn plan_from(items: &[TodoItem]) -> Plan {
    let mut current_marked = false;
    let entries = items
        .iter()
        .map(|it| {
            let status = if it.done {
                PlanEntryStatus::Completed
            } else if !current_marked {
                current_marked = true;
                PlanEntryStatus::InProgress
            } else {
                PlanEntryStatus::Pending
            };
            PlanEntry::new(it.item.clone(), PlanEntryPriority::Medium, status)
        })
        .collect();
    Plan::new(entries)
}

/// ACP's tool category for a Boubou tool name.
pub fn kind_for(name: &str) -> ToolKind {
    match name {
        "read_file" | "list_dir" | "memory_recall" | "memory_list" => ToolKind::Read,
        "search_files" | "web_search" | "web_search_multi" | "memory_search" => ToolKind::Search,
        "create_file" | "edit_file" | "append_file" | "memory_save" => ToolKind::Edit,
        "delete_file" | "memory_forget" => ToolKind::Delete,
        "run_bash" => ToolKind::Execute,
        "web_fetch" => ToolKind::Fetch,
        "todo" => ToolKind::Think,
        _ => ToolKind::Other,
    }
}

/// A one-line human title for the tool call: the path, command or query, as the web UI
/// summarises a run block.
pub fn title_for(name: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim);
    let detail = match name {
        "run_bash" => s("command").or_else(|| s("cmd")),
        "web_search" | "web_search_multi" | "search_files" => s("query").or_else(|| s("pattern")),
        "web_fetch" => s("url"),
        "todo" => s("action"),
        "enable_tools" => args
            .get("tools")
            .and_then(Value::as_array)
            .map(|_| "")
            .or_else(|| s("tools")),
        _ => s("path").or_else(|| s("file")),
    };
    let label = name.replace('_', " ");
    match detail {
        Some(d) if !d.is_empty() => format!("{label}: {}", boubou_kernel::text::head(d, 100)),
        _ => label,
    }
}

/// File locations for follow-along: the `path` argument of a file tool, absolute.
pub fn locations_for(name: &str, args: &Value, cwd: &Path) -> Vec<ToolCallLocation> {
    if !matches!(
        name,
        "read_file" | "create_file" | "edit_file" | "append_file" | "delete_file"
    ) {
        return vec![];
    }
    args.get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .map(|p| vec![ToolCallLocation::new(absolute(cwd, p))])
        .unwrap_or_default()
}

/// The diff an editor can show for a file write: create (no old text), edit (old → new),
/// append (new text only).
pub fn diff_for(name: &str, args: &Value, cwd: &Path) -> Option<Diff> {
    let path = args.get("path").and_then(Value::as_str)?;
    let abs = absolute(cwd, path);
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    match name {
        "create_file" => Some(Diff::new(abs, s("content")?)),
        "edit_file" => Some(Diff::new(abs, s("new")?).old_text(s("old"))),
        "append_file" => Some(Diff::new(abs, s("content")?)),
        _ => None,
    }
}

fn absolute(cwd: &Path, p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() {
        pb
    } else {
        cwd.join(pb)
    }
}

/// The user's prompt as one text for the loop: text blocks as they are, embedded text
/// resources quoted with their URI, resource links as a line the model can act on.
pub fn prompt_text(blocks: &[ContentBlock]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in blocks {
        match b {
            ContentBlock::Text(t) => parts.push(t.text.clone()),
            ContentBlock::ResourceLink(r) => {
                parts.push(format!("Attached: {} ({})", r.name, uri_display(&r.uri)));
            }
            ContentBlock::Resource(r) => match &r.resource {
                EmbeddedResourceResource::TextResourceContents(t) => parts.push(format!(
                    "--- {} ---\n{}\n--- end ---",
                    uri_display(&t.uri),
                    t.text
                )),
                EmbeddedResourceResource::BlobResourceContents(b) => {
                    parts.push(format!(
                        "[binary resource omitted: {}]",
                        uri_display(&b.uri)
                    ));
                }
                _ => parts.push("[resource omitted]".to_string()),
            },
            ContentBlock::Image(_) => parts.push("[image omitted]".to_string()),
            ContentBlock::Audio(_) => parts.push("[audio omitted]".to_string()),
            // Non-exhaustive enum: a block this version does not know is skipped.
            _ => {}
        }
    }
    parts.join("\n\n")
}

fn uri_display(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boubou_kernel::ollama::{FunctionCall, ToolCall as LoopCall};
    use serde_json::json;

    fn call(name: &str, args: Value) -> AgentEvent {
        AgentEvent::ToolCall(LoopCall {
            function: FunctionCall {
                name: name.to_string(),
                arguments: args,
            },
        })
    }

    fn result(name: &str, args: Value, output: &str, blocked: bool) -> AgentEvent {
        AgentEvent::ToolResult {
            name: name.to_string(),
            args,
            output: output.to_string(),
            blocked,
        }
    }

    #[test]
    fn text_and_thinking_stream_as_chunks() {
        let mut b = TurnBridge::new("/w");
        let out = b.on_event(AgentEvent::Text("hello".into()));
        assert!(
            matches!(out.as_slice(), [SessionUpdate::AgentMessageChunk(c)] if matches!(&c.content, ContentBlock::Text(t) if t.text == "hello"))
        );
        let out = b.on_event(AgentEvent::Thinking("hmm".into()));
        assert!(matches!(
            out.as_slice(),
            [SessionUpdate::AgentThoughtChunk(_)]
        ));
        assert!(b.on_event(AgentEvent::Text(String::new())).is_empty());
    }

    #[test]
    fn tool_call_then_result_share_an_id_and_carry_kind_status_and_diff() {
        let mut b = TurnBridge::new("/w");
        let args = json!({"path": "a.txt", "content": "x"});
        let out = b.on_event(call("create_file", args.clone()));
        let SessionUpdate::ToolCall(tc) = &out[0] else {
            panic!("tool_call expected");
        };
        assert_eq!(tc.kind, ToolKind::Edit);
        assert_eq!(tc.status, ToolCallStatus::InProgress);
        assert_eq!(tc.name.as_deref(), Some("create_file"));
        assert_eq!(tc.title, "create file: a.txt");
        assert_eq!(tc.locations[0].path, PathBuf::from("/w/a.txt"));
        assert_eq!(b.current_call(), Some(tc.tool_call_id.clone()));

        let out = b.on_event(result("create_file", args, "[created a.txt]", false));
        let SessionUpdate::ToolCallUpdate(u) = &out[0] else {
            panic!("tool_call_update expected");
        };
        assert_eq!(u.tool_call_id, tc.tool_call_id);
        assert_eq!(u.fields.status, Some(ToolCallStatus::Completed));
        let content = u.fields.content.as_ref().expect("content");
        assert!(
            matches!(&content[0], ToolCallContent::Diff(d) if d.new_text == "x" && d.old_text.is_none())
        );
        assert!(matches!(&content[1], ToolCallContent::Content(_)));
        assert!(b.current_call().is_none());
    }

    #[test]
    fn blocked_or_erroring_results_are_failed() {
        let mut b = TurnBridge::new("/w");
        let args = json!({"command": "rm -rf /"});
        b.on_event(call("run_bash", args.clone()));
        let out = b.on_event(result(
            "run_bash",
            args,
            "[blocked by the safety gate]",
            true,
        ));
        let SessionUpdate::ToolCallUpdate(u) = &out[0] else {
            panic!()
        };
        assert_eq!(u.fields.status, Some(ToolCallStatus::Failed));

        let args = json!({"path": "nope.txt"});
        b.on_event(call("read_file", args.clone()));
        let out = b.on_event(result(
            "read_file",
            args,
            "[tool error: file not found]",
            false,
        ));
        let SessionUpdate::ToolCallUpdate(u) = &out[0] else {
            panic!()
        };
        assert_eq!(u.fields.status, Some(ToolCallStatus::Failed));
    }

    #[test]
    fn finish_is_not_a_tool_call_and_its_summary_closes_a_tool_ended_turn() {
        let mut b = TurnBridge::new("/w");
        assert!(b
            .on_event(call("finish", json!({"summary": "s"})))
            .is_empty());
        assert!(b
            .on_event(result(
                "finish",
                json!({"summary": "s"}),
                "[TASK_COMPLETE]",
                false
            ))
            .is_empty());
        // Prose answer, then finish: the summary would repeat it.
        b.on_event(AgentEvent::Text("The answer is 4.".into()));
        assert!(b
            .on_event(AgentEvent::Finished {
                summary: "Answered 4".into()
            })
            .is_empty());
        // Tool call, then finish: the summary is the only closing statement.
        let args = json!({"path": "r.md", "content": "x"});
        b.on_event(call("create_file", args.clone()));
        b.on_event(result("create_file", args, "[created r.md]", false));
        let out = b.on_event(AgentEvent::Finished {
            summary: "Wrote r.md".into(),
        });
        assert!(
            matches!(out.as_slice(), [SessionUpdate::AgentMessageChunk(c)] if matches!(&c.content, ContentBlock::Text(t) if t.text == "Wrote r.md"))
        );
    }

    #[test]
    fn plan_follows_todo_json() {
        let d = tempfile::tempdir().expect("tmp");
        let mut b = TurnBridge::new(d.path());
        // No plan file yet: a finish sends no plan.
        assert!(b
            .on_event(AgentEvent::Finished {
                summary: String::new()
            })
            .is_empty());
        boubou_kernel::todo::save_todo(
            d.path(),
            &[
                TodoItem {
                    item: "read".into(),
                    done: true,
                },
                TodoItem::new("write"),
                TodoItem::new("check"),
            ],
        )
        .expect("todo");
        let args = json!({"action": "create", "items": ["read", "write", "check"]});
        b.on_event(call("todo", args.clone()));
        let out = b.on_event(result("todo", args, "[todo created]", false));
        assert_eq!(out.len(), 2);
        let SessionUpdate::Plan(p) = &out[1] else {
            panic!("plan expected")
        };
        assert_eq!(p.entries.len(), 3);
        assert_eq!(p.entries[0].status, PlanEntryStatus::Completed);
        assert_eq!(p.entries[1].status, PlanEntryStatus::InProgress);
        assert_eq!(p.entries[2].status, PlanEntryStatus::Pending);
        // Once shown, an emptied plan is sent again (empty) so the client clears it.
        let _ = std::fs::remove_file(boubou_kernel::todo::todo_file(d.path()));
        let out = b.on_event(AgentEvent::Finished {
            summary: String::new(),
        });
        assert!(matches!(out.as_slice(), [SessionUpdate::Plan(p)] if p.entries.is_empty()));
    }

    #[test]
    fn usage_compaction_errors_and_limits_map_to_updates_and_notices() {
        let mut b = TurnBridge::new("/w");
        let out = b.on_event(AgentEvent::ContextUsage {
            used: 1200,
            limit: 4096,
        });
        assert!(
            matches!(out.as_slice(), [SessionUpdate::UsageUpdate(u)] if u.used == 1200 && u.size == 4096)
        );
        let out = b.on_event(AgentEvent::Compacted {
            level: 2,
            before: 40,
            after: 12,
        });
        assert!(
            matches!(out.as_slice(), [SessionUpdate::Notice(n)] if n.severity == NoticeSeverity::Info)
        );
        let out = b.on_event(AgentEvent::Error("boom".into()));
        assert!(
            matches!(out.as_slice(), [SessionUpdate::Notice(n)] if n.severity == NoticeSeverity::Error && n.title == "boom")
        );
        assert!(b.on_event(AgentEvent::Log("[decider] x".into())).is_empty());
        assert_eq!(b.stop_reason(false), StopReason::EndTurn);
        b.on_event(AgentEvent::MaxIterations);
        assert_eq!(b.stop_reason(false), StopReason::MaxTurnRequests);
        assert_eq!(b.stop_reason(true), StopReason::Cancelled);
    }

    #[test]
    fn kinds_titles_and_prompt_text() {
        assert_eq!(kind_for("run_bash"), ToolKind::Execute);
        assert_eq!(kind_for("web_fetch"), ToolKind::Fetch);
        assert_eq!(kind_for("delete_file"), ToolKind::Delete);
        assert_eq!(kind_for("server.tool"), ToolKind::Other);
        assert_eq!(
            title_for("run_bash", &json!({"command": "ls -la"})),
            "run bash: ls -la"
        );
        assert_eq!(
            title_for("web_search", &json!({"query": "libp2p dart"})),
            "web search: libp2p dart"
        );
        assert_eq!(
            title_for("enable_tools", &json!({"tools": ["web"]})),
            "enable tools"
        );
        assert!(locations_for("run_bash", &json!({"command": "ls"}), Path::new("/w")).is_empty());

        let blocks = vec![
            ContentBlock::from("Fix the bug"),
            ContentBlock::ResourceLink(agent_client_protocol::schema::v1::ResourceLink::new(
                "main.rs",
                "file:///w/src/main.rs",
            )),
        ];
        let t = prompt_text(&blocks);
        assert!(t.starts_with("Fix the bug\n\nAttached: main.rs (/w/src/main.rs)"));
    }

    fn history_call(name: &str, args: Value) -> LoopCall {
        LoopCall {
            function: FunctionCall {
                name: name.to_string(),
                arguments: args,
            },
        }
    }

    #[test]
    fn a_saved_history_replays_as_the_update_stream_minus_the_harness() {
        let d = tempfile::tempdir().expect("tmp");
        let cwd = d.path();
        let mut asst = Message::assistant("");
        asst.tool_calls = vec![
            history_call("create_file", json!({"path": "a.txt", "content": "hi"})),
            history_call("read_file", json!({"path": "b.txt"})),
        ];
        let mut fin = Message::assistant("");
        fin.tool_calls = vec![history_call("finish", json!({"summary": "wrote a.txt"}))];
        let history = vec![
            Message::system("task prompt"),
            Message::user("Set up the note"),
            asst,
            Message::tool("create_file", "[created a.txt]"),
            Message::tool("read_file", "[tool error: not found: b.txt]"),
            Message::user("⚠️ Plan progress reminder: sync the plan"),
            Message::user("Your answer is already sufficient; call finish now."),
            fin,
            Message::tool("finish", "[TASK_COMPLETE] done"),
            Message::user("And a second turn"),
            Message::assistant("Here is prose, no finish needed."),
        ];
        let out = replay_updates(&history, cwd, cwd);
        let kinds: Vec<&str> = out
            .iter()
            .map(|u| match u {
                SessionUpdate::UserMessageChunk(_) => "user",
                SessionUpdate::AgentMessageChunk(_) => "agent",
                SessionUpdate::ToolCall(_) => "call",
                SessionUpdate::ToolCallUpdate(_) => "update",
                SessionUpdate::Plan(_) => "plan",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["user", "call", "call", "update", "update", "agent", "user", "agent"],
            "{out:#?}"
        );
        let SessionUpdate::ToolCall(c0) = &out[1] else {
            panic!()
        };
        assert_eq!(c0.tool_call_id, ToolCallId::new("hist_1"));
        assert_eq!(c0.title, "create file: a.txt");
        assert_eq!(c0.kind, ToolKind::Edit);
        let SessionUpdate::ToolCallUpdate(u0) = &out[3] else {
            panic!()
        };
        assert_eq!(u0.tool_call_id, ToolCallId::new("hist_1"));
        assert_eq!(u0.fields.status, Some(ToolCallStatus::Completed));
        assert!(
            u0.fields
                .content
                .as_ref()
                .is_some_and(|c| matches!(c[0], ToolCallContent::Diff(_))),
            "a create shows its diff"
        );
        let SessionUpdate::ToolCallUpdate(u1) = &out[4] else {
            panic!()
        };
        assert_eq!(u1.tool_call_id, ToolCallId::new("hist_2"));
        assert_eq!(u1.fields.status, Some(ToolCallStatus::Failed));
        let texts: Vec<&str> = out
            .iter()
            .filter_map(|u| match u {
                SessionUpdate::AgentMessageChunk(c) | SessionUpdate::UserMessageChunk(c) => {
                    match &c.content {
                        ContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "Set up the note",
                "wrote a.txt",
                "And a second turn",
                "Here is prose, no finish needed."
            ]
        );
    }

    #[test]
    fn the_plan_is_read_from_the_plan_dir_not_the_cwd() {
        let cwd = tempfile::tempdir().expect("tmp");
        let plan = tempfile::tempdir().expect("tmp");
        std::fs::write(
            plan.path().join("todo.json"),
            r#"[{"item":"one","done":false}]"#,
        )
        .expect("todo");
        let mut b = TurnBridge::new(cwd.path()).with_plan_dir(plan.path());
        b.on_event(call("todo", json!({"action": "create", "items": ["one"]})));
        let out = b.on_event(result(
            "todo",
            json!({"action": "create", "items": ["one"]}),
            "[plan: 1 item]",
            false,
        ));
        assert!(
            out.iter().any(|u| matches!(u, SessionUpdate::Plan(_))),
            "{out:#?}"
        );
        let out = replay_updates(&[Message::user("go")], cwd.path(), plan.path());
        assert!(
            matches!(out.last(), Some(SessionUpdate::Plan(_))),
            "{out:#?}"
        );
    }

    #[test]
    fn a_call_without_a_result_is_marked_failed_and_the_plan_comes_last() {
        let d = tempfile::tempdir().expect("tmp");
        let cwd = d.path();
        std::fs::write(
            cwd.join("todo.json"),
            r#"[{"item":"first","done":true},{"item":"second","done":false}]"#,
        )
        .expect("todo");
        let mut asst = Message::assistant("Running it");
        asst.tool_calls = vec![history_call("run_bash", json!({"command": "ls"}))];
        let out = replay_updates(&[Message::user("go"), asst], cwd, cwd);
        assert_eq!(out.len(), 5, "{out:#?}");
        let SessionUpdate::ToolCallUpdate(u) = &out[3] else {
            panic!("{out:#?}")
        };
        assert_eq!(u.fields.status, Some(ToolCallStatus::Failed));
        assert!(u.fields.content.is_none());
        let SessionUpdate::Plan(p) = &out[4] else {
            panic!()
        };
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].status, PlanEntryStatus::Completed);
        assert_eq!(p.entries[1].status, PlanEntryStatus::InProgress);
    }
}
