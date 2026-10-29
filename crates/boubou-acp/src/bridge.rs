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
//!   the model did not already state the outcome in prose after its last tool call.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Diff, EmbeddedResourceResource, Notice, NoticeSeverity, Plan,
    PlanEntry, PlanEntryPriority, PlanEntryStatus, SessionUpdate, StopReason, ToolCall,
    ToolCallContent, ToolCallId, ToolCallLocation, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind, UsageUpdate,
};
use boubou_core::agent::AgentEvent;
use boubou_core::todo::TodoItem;
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
        Self {
            cwd: cwd.into(),
            next_call: 0,
            pending: VecDeque::new(),
            text_since_tool: false,
            plan_shown: false,
            max_iterations: false,
        }
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
                let failed = blocked || boubou_core::agent::run::is_tool_error(&output);
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
                let title = boubou_core::text::head(&e, 80).to_string();
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
        let items = boubou_core::todo::load_todo(&self.cwd);
        if items.is_empty() && !self.plan_shown {
            return vec![];
        }
        self.plan_shown = true;
        vec![SessionUpdate::Plan(plan_from(&items))]
    }
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
        Some(d) if !d.is_empty() => format!("{label}: {}", boubou_core::text::head(d, 100)),
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
    use boubou_core::ollama::{FunctionCall, ToolCall as LoopCall};
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
        boubou_core::todo::save_todo(
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
        let _ = std::fs::remove_file(boubou_core::todo::todo_file(d.path()));
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
}
