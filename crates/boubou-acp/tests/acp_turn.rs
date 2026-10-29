//! End to end over an in-process channel: an ACP client (as an editor would be) talks to
//! `BoubouAcp` driving a scripted model and scripted tools. No network, no Ollama.
//!
//! Covers the bead's acceptance list minus the real editor: initialize, session/new, a
//! prompt turn with streamed tool calls and statuses, a permission round trip through the
//! safety gate's confirmer, the finish summary as the closing message, a second prompt on
//! the same session (history kept), and session/cancel ending a turn.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, InitializeRequest, NewSessionRequest, PermissionOptionId,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, StopReason, ToolCallStatus,
    ToolKind,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Channel, Client, ConnectTo, Responder};
use boubou_acp::runner::{AgentFactory, TurnRunner, TurnSetup};
use boubou_acp::BoubouAcp;
use boubou_core::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use boubou_core::agent::{Agent, RunParams, ToolRunner};
use boubou_core::ollama::Message;
use boubou_core::safety::{ConfirmRequest, Confirmer, Verdict};
use serde_json::{json, Value};

type Script = Vec<(&'static str, Value)>;

/// Replays a script of tool calls, then finishes. `delay` slows each model call so a
/// cancel sent by the client can land mid-turn.
struct Scripted {
    calls: Script,
    i: usize,
    delay: Duration,
    forever: bool,
}

impl ChatBackend for Scripted {
    fn chat(
        &mut self,
        _model: &str,
        _messages: &[Message],
        _tools: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        std::thread::sleep(self.delay);
        let k = self.i;
        self.i += 1;
        let mut r = if self.forever {
            ChatResponse::call("", "read_file", json!({"path": format!("f{k}.txt")}))
        } else if k < self.calls.len() {
            let (name, args) = &self.calls[k];
            ChatResponse::call("", name, args.clone())
        } else {
            ChatResponse::call("done", "finish", json!({"summary": "all done"}))
        };
        r.prompt_eval_count = Some(500);
        Ok(r)
    }
}

/// Scripted tools; `delete_file` goes through the confirmer like the real gate would.
struct Tools {
    confirmer: Box<dyn Confirmer>,
    cwd: PathBuf,
}

impl ToolRunner for Tools {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        match name {
            "create_file" => format!("[created {path}]"),
            "read_file" => "contents".to_string(),
            "delete_file" => {
                let req = ConfirmRequest {
                    tool: "delete_file",
                    target: self.cwd.join(path).display().to_string(),
                    action: "delete",
                    workdir: &self.cwd,
                };
                match self.confirmer.confirm(&req) {
                    Verdict::Deny => "[tool error: delete_file denied by the user]".to_string(),
                    _ => format!("[deleted {path}]"),
                }
            }
            "finish" => "[TASK_COMPLETE] done".to_string(),
            _ => "[tool error: not scripted]".to_string(),
        }
    }
}

struct ScriptedFactory {
    script: Script,
    delay: Duration,
    forever: bool,
    turns: Arc<AtomicUsize>,
    /// The message count the loop was started with, per turn (history check).
    opened_with: Arc<Mutex<Vec<usize>>>,
}

struct Counting {
    inner: Agent<Scripted, Tools>,
    opened_with: Arc<Mutex<Vec<usize>>>,
}

impl TurnRunner for Counting {
    fn task_messages(&mut self, task: &str) -> Vec<Message> {
        self.inner.task_messages(task)
    }
    fn run(
        &mut self,
        messages: Vec<Message>,
        session: Option<&str>,
    ) -> boubou_core::Result<Vec<Message>> {
        self.opened_with.lock().expect("lock").push(messages.len());
        self.inner.run(messages, session, None)
    }
}

impl AgentFactory for ScriptedFactory {
    fn new_turn(&self, setup: TurnSetup) -> boubou_core::Result<Box<dyn TurnRunner>> {
        self.turns.fetch_add(1, Ordering::SeqCst);
        let params = RunParams::plain(setup.model, &setup.cwd);
        let backend = Scripted {
            calls: self.script.clone(),
            i: 0,
            delay: self.delay,
            forever: self.forever,
        };
        let tools = Tools {
            confirmer: setup.confirmer,
            cwd: setup.cwd.clone(),
        };
        let mut a = Agent::new(params, backend, tools)
            .with_sink(setup.sink)
            .with_stop(setup.stop);
        a.sessions_dir = setup.cwd.join("sessions");
        Ok(Box::new(Counting {
            inner: a,
            opened_with: self.opened_with.clone(),
        }))
    }
}

#[derive(Default)]
struct Seen {
    updates: Vec<SessionUpdate>,
    permissions: Vec<RequestPermissionRequest>,
}

fn text_of(u: &SessionUpdate) -> Option<&str> {
    match u {
        SessionUpdate::AgentMessageChunk(c) => match &c.content {
            ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        },
        _ => None,
    }
}

#[tokio::test]
async fn a_prompt_turn_streams_tool_calls_asks_permission_and_closes_with_the_summary() {
    let d = tempfile::tempdir().expect("tmp");
    let cwd = d.path().to_path_buf();
    let opened_with = Arc::new(Mutex::new(Vec::new()));
    let factory = ScriptedFactory {
        script: vec![
            ("create_file", json!({"path": "a.txt", "content": "hello"})),
            ("delete_file", json!({"path": "a.txt"})),
        ],
        delay: Duration::ZERO,
        forever: false,
        turns: Arc::new(AtomicUsize::new(0)),
        opened_with: opened_with.clone(),
    };
    let agent = BoubouAcp::with_factory(cwd.join("home"), "scripted:1b".into(), Arc::new(factory));
    let (agent_ch, client_ch) = Channel::duplex();
    let seen = Arc::new(Mutex::new(Seen::default()));

    let client_seen = seen.clone();
    let perm_seen = seen.clone();
    let client = Client
        .builder()
        .name("test-editor")
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                client_seen.lock().expect("lock").updates.push(n.update);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest,
                        responder: Responder<RequestPermissionResponse>,
                        _cx| {
                perm_seen.lock().expect("lock").permissions.push(req);
                // The user denies the deletion.
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        PermissionOptionId::new("deny"),
                    )),
                ))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(client_ch, async |cx| {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let ns = cx
                .send_request(NewSessionRequest::new(cwd.clone()))
                .block_task()
                .await?;
            let p1 = cx
                .send_request(PromptRequest::new(
                    ns.session_id.clone(),
                    vec!["Set up the scratch note".into()],
                ))
                .block_task()
                .await?;
            let p2 = cx
                .send_request(PromptRequest::new(
                    ns.session_id.clone(),
                    vec!["And again".into()],
                ))
                .block_task()
                .await?;
            Ok((init, ns, p1, p2))
        });

    let (agent_res, client_res) = tokio::join!(agent.connect_to(agent_ch), client);
    agent_res.expect("agent side");
    let (init, ns, p1, p2) = client_res.expect("client side");

    assert_eq!(init.protocol_version, ProtocolVersion::V1);
    assert_eq!(
        init.agent_info.as_ref().map(|i| i.name.as_str()),
        Some("boubou")
    );
    assert!(!init.agent_capabilities.load_session);
    assert!(ns.session_id.0.starts_with("acp_"));
    assert_eq!(p1.stop_reason, StopReason::EndTurn);
    assert_eq!(p2.stop_reason, StopReason::EndTurn);

    let seen = seen.lock().expect("lock");
    // One permission question, about the delete call, denied → that call failed.
    assert_eq!(seen.permissions.len(), 2, "one per turn");
    let perm = &seen.permissions[0];
    assert_eq!(perm.session_id, ns.session_id);
    assert_eq!(perm.tool_call.fields.kind, Some(ToolKind::Delete));
    assert_eq!(perm.options.len(), 3);

    let calls: Vec<_> = seen
        .updates
        .iter()
        .filter_map(|u| match u {
            SessionUpdate::ToolCall(tc) => Some(tc),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 4, "two tool calls per turn, finish is not one");
    assert_eq!(calls[0].kind, ToolKind::Edit);
    assert_eq!(calls[0].status, ToolCallStatus::InProgress);
    assert_eq!(calls[0].title, "create file: a.txt");
    assert_eq!(calls[1].kind, ToolKind::Delete);
    assert_eq!(perm.tool_call.tool_call_id, calls[1].tool_call_id);

    let updates: Vec<_> = seen
        .updates
        .iter()
        .filter_map(|u| match u {
            SessionUpdate::ToolCallUpdate(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 4);
    assert_eq!(updates[0].fields.status, Some(ToolCallStatus::Completed));
    assert_eq!(updates[1].fields.status, Some(ToolCallStatus::Failed));

    let texts: Vec<&str> = seen.updates.iter().filter_map(text_of).collect();
    assert_eq!(
        texts,
        vec!["all done", "all done"],
        "the finish summary closes each turn"
    );

    // The second prompt continued the first turn's history.
    let opened = opened_with.lock().expect("lock").clone();
    assert_eq!(opened.len(), 2);
    assert_eq!(opened[0], 2, "system + task");
    assert!(
        opened[1] > opened[0] + 1,
        "prior history plus the new user turn"
    );
}

#[tokio::test]
async fn cancel_ends_the_turn_with_the_cancelled_stop_reason() {
    let d = tempfile::tempdir().expect("tmp");
    let cwd = d.path().to_path_buf();
    let factory = ScriptedFactory {
        script: vec![],
        delay: Duration::from_millis(40),
        forever: true,
        turns: Arc::new(AtomicUsize::new(0)),
        opened_with: Arc::new(Mutex::new(Vec::new())),
    };
    let agent = BoubouAcp::with_factory(cwd.join("home"), "scripted:1b".into(), Arc::new(factory));
    let (agent_ch, client_ch) = Channel::duplex();
    let tool_calls = Arc::new(AtomicUsize::new(0));

    let counter = tool_calls.clone();
    let client = Client
        .builder()
        .name("test-editor")
        .on_receive_notification(
            async move |n: SessionNotification, cx| {
                if matches!(n.update, SessionUpdate::ToolCall(_)) {
                    let k = counter.fetch_add(1, Ordering::SeqCst);
                    if k == 1 {
                        cx.send_notification(CancelNotification::new(n.session_id.clone()))?;
                    }
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(client_ch, async |cx| {
            cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let ns = cx
                .send_request(NewSessionRequest::new(cwd.clone()))
                .block_task()
                .await?;
            cx.send_request(PromptRequest::new(
                ns.session_id.clone(),
                vec!["Keep reading files".into()],
            ))
            .block_task()
            .await
        });

    let (agent_res, client_res) = tokio::join!(agent.connect_to(agent_ch), client);
    agent_res.expect("agent side");
    let p = client_res.expect("client side");
    assert_eq!(p.stop_reason, StopReason::Cancelled);
    let n = tool_calls.load(Ordering::SeqCst);
    assert!(
        (2..10).contains(&n),
        "the loop stopped soon after the cancel: {n} calls"
    );
}
