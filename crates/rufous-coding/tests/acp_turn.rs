//! End to end over an in-process channel: an ACP client (as an editor would be) talks to
//! `RufousAcp` driving a scripted model and scripted tools. No network, no Ollama.
//!
//! Covers the bead's acceptance list minus the real editor: initialize, session/new, a
//! prompt turn with streamed tool calls and statuses, a permission round trip through the
//! safety gate's confirmer, the finish summary as the closing message, a second prompt on
//! the same session (history kept), session/cancel ending a turn, and session/load in a
//! second server over the same home replaying the history and continuing it. Then the
//! client's `fs` capability (bead boubou-m64.2): with it the real executor's text-file
//! tools read and write through the client; without it they use the disk.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities,
    InitializeRequest, LoadSessionRequest, NewSessionRequest, PermissionOptionId, PromptRequest,
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
    SessionUpdate, StopReason, ToolCallStatus, ToolKind, WriteTextFileRequest,
    WriteTextFileResponse,
};
use agent_client_protocol::schema::v1::{
    EnvVariable, HttpHeader, McpServer, McpServerHttp, McpServerStdio,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Channel, Client, ConnectTo, Responder};
use rufous_coding::runner::{AgentFactory, TurnRunner, TurnSetup};
use rufous_coding::RufousAcp;
use rufous_kernel::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use rufous_kernel::agent::{Agent, RunParams, ToolRunner};
use rufous_kernel::ollama::Message;
use rufous_kernel::safety::{ConfirmRequest, Confirmer, Verdict};
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
    ) -> rufous_kernel::Result<Vec<Message>> {
        self.opened_with.lock().expect("lock").push(messages.len());
        self.inner.run(messages, session, None)
    }
}

impl AgentFactory for ScriptedFactory {
    fn new_turn(&self, setup: TurnSetup) -> rufous_kernel::Result<Box<dyn TurnRunner>> {
        self.turns.fetch_add(1, Ordering::SeqCst);
        let mut params = RunParams::plain(setup.model, &setup.cwd);
        params.policy = rufous_coding::harness::policy();
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
        a.checkpoint = Some(rufous_kernel::agent::session::state_path(
            &setup.sessions_dir,
            &setup.session,
        ));
        a.sessions_dir = setup.sessions_dir.clone();
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
    let agent = RufousAcp::with_factory(cwd.join("home"), "scripted:1b".into(), Arc::new(factory));
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
        Some("rufous")
    );
    assert!(init.agent_capabilities.load_session);
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

    // The editor's project holds no harness state (boubou-ll7): the live checkpoint sits
    // beside the session files, and a listing of the cwd shows only the model's own work.
    assert!(
        !cwd.join(".agent_state.json").exists(),
        "checkpoint must not be written into the editor's cwd"
    );
    let state = cwd
        .join("home")
        .join("sessions")
        .join(format!("{}.state.json", ns.session_id.0));
    assert!(state.is_file(), "missing {}", state.display());
    let saved: Vec<Message> =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("read")).expect("json");
    assert!(
        saved.len() >= opened[1],
        "the checkpoint holds the second turn"
    );
    let listing: Vec<String> = std::fs::read_dir(&cwd)
        .expect("ls")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".agent_state"))
        .collect();
    assert!(listing.is_empty(), "{listing:?}");

    // The session log (bead boubou-bmq): the editor keeps none of the loop's protocol
    // lines, so they sit beside the session files, one `[turn]` line per prompt and the
    // `⚙` line per call, each behind a timestamp the bench parser strips.
    let log = cwd
        .join("home")
        .join("sessions")
        .join(format!("{}.log", ns.session_id.0));
    let text = std::fs::read_to_string(&log).unwrap_or_else(|e| panic!("{}: {e}", log.display()));
    let turns: Vec<&str> = text.lines().filter(|l| l.contains("[turn] ")).collect();
    assert_eq!(turns.len(), 2, "{text}");
    assert!(
        turns[0].ends_with("[turn] Set up the scratch note"),
        "{}",
        turns[0]
    );
    assert!(
        text.lines().filter(|l| l.contains(" ⚙ ")).count() >= 2,
        "a ⚙ line per tool call: {text}"
    );
    assert!(
        text.lines()
            .all(|l| l.len() > 20 && &l[4..5] == "-" && &l[19..20] == " "),
        "every line starts with a timestamp: {text}"
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
    let agent = RufousAcp::with_factory(cwd.join("home"), "scripted:1b".into(), Arc::new(factory));
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

/// A client that only records what it is told (the builder's handler types are not
/// nameable, hence a macro). The test drives the requests from `connect_with`.
macro_rules! recording_client {
    ($seen:expr) => {{
        let seen = $seen;
        Client
            .builder()
            .name("test-editor")
            .on_receive_notification(
                async move |n: SessionNotification, _cx| {
                    seen.lock().expect("lock").push(n.update);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
    }};
}

#[tokio::test]
async fn a_restarted_agent_loads_the_session_replays_it_and_continues() {
    let d = tempfile::tempdir().expect("tmp");
    let cwd = d.path().to_path_buf();
    let home = cwd.join("home");
    let opened_with = Arc::new(Mutex::new(Vec::new()));
    let factory = |script: Script| ScriptedFactory {
        script,
        delay: Duration::ZERO,
        forever: false,
        turns: Arc::new(AtomicUsize::new(0)),
        opened_with: opened_with.clone(),
    };

    // First life of the agent: one turn, then the process "exits".
    let first = RufousAcp::with_factory(
        home.clone(),
        "scripted:1b".into(),
        Arc::new(factory(vec![(
            "create_file",
            json!({"path": "a.txt", "content": "hello"}),
        )])),
    );
    let (agent_ch, client_ch) = Channel::duplex();
    let seen1 = Arc::new(Mutex::new(Vec::new()));
    let client = recording_client!(seen1.clone()).connect_with(client_ch, {
        let cwd = cwd.clone();
        async move |cx| {
            cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let ns = cx
                .send_request(NewSessionRequest::new(cwd.clone()))
                .block_task()
                .await?;
            cx.send_request(PromptRequest::new(
                ns.session_id.clone(),
                vec!["Set up the scratch note".into()],
            ))
            .block_task()
            .await?;
            Ok(ns.session_id)
        }
    });
    let (a, c) = tokio::join!(first.connect_to(agent_ch), client);
    a.expect("agent side");
    let session_id = c.expect("client side");
    assert!(
        home.join("sessions")
            .join(format!("{}.json", session_id.0))
            .is_file(),
        "the turn saved the session under the server's home"
    );

    // Second life: a fresh server over the same home; the editor reopens the thread.
    let second = RufousAcp::with_factory(
        home.clone(),
        "scripted:1b".into(),
        Arc::new(factory(vec![("read_file", json!({"path": "a.txt"}))])),
    );
    let (agent_ch, client_ch) = Channel::duplex();
    let seen2 = Arc::new(Mutex::new(Vec::new()));
    let client = recording_client!(seen2.clone()).connect_with(client_ch, {
        let cwd = cwd.clone();
        let sid = session_id.clone();
        async move |cx| {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let unknown = cx
                .send_request(LoadSessionRequest::new(
                    SessionId::new("acp_0101_000000"),
                    cwd.clone(),
                ))
                .block_task()
                .await
                .is_err();
            let bad_name = cx
                .send_request(LoadSessionRequest::new(
                    SessionId::new("../etc/passwd"),
                    cwd.clone(),
                ))
                .block_task()
                .await
                .is_err();
            cx.send_request(LoadSessionRequest::new(sid.clone(), cwd.clone()))
                .block_task()
                .await?;
            let p = cx
                .send_request(PromptRequest::new(sid.clone(), vec!["And again".into()]))
                .block_task()
                .await?;
            Ok((init, unknown, bad_name, p))
        }
    });
    let (a, c) = tokio::join!(second.connect_to(agent_ch), client);
    a.expect("agent side");
    let (init, unknown, bad_name, p) = c.expect("client side");
    assert!(init.agent_capabilities.load_session);
    assert!(unknown, "an unknown session id is an error");
    assert!(bad_name, "a path-shaped session id is refused");
    assert_eq!(p.stop_reason, StopReason::EndTurn);

    let seen = seen2.lock().expect("lock");
    // The replay is the only source of user chunks; the first turn's create_file comes back
    // completed, then the new turn's read_file runs.
    let users: Vec<&str> = seen
        .iter()
        .filter_map(|u| match u {
            SessionUpdate::UserMessageChunk(c) => match &c.content {
                ContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(users, vec!["Set up the scratch note"]);
    let calls: Vec<_> = seen
        .iter()
        .filter_map(|u| match u {
            SessionUpdate::ToolCall(tc) => Some(tc),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2, "{seen:#?}");
    assert_eq!(calls[0].title, "create file: a.txt");
    assert_eq!(calls[0].tool_call_id.0.as_ref(), "hist_1");
    assert_eq!(calls[1].title, "read file: a.txt");
    let statuses: Vec<_> = seen
        .iter()
        .filter_map(|u| match u {
            SessionUpdate::ToolCallUpdate(t) => t.fields.status,
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        vec![ToolCallStatus::Completed, ToolCallStatus::Completed]
    );
    // The replay carries the model's prose from the saved record: the scripted model says
    // "done" alongside its finish, accepted first time (an editor turn has no finish
    // gates); with prose on record the finish summary is not repeated. The live second
    // turn then closes with its summary, as the scripted backend streams no tokens.
    let texts: Vec<&str> = seen.iter().filter_map(text_of).collect();
    assert_eq!(texts, vec!["done", "all done"]);

    // The new turn ran on the loaded history, not on a fresh task prompt.
    let opened = opened_with.lock().expect("lock").clone();
    assert_eq!(opened.len(), 2);
    assert!(opened[1] > opened[0] + 1, "{opened:?}");
}

#[test]
fn an_editor_turn_is_interactive_keeps_its_files_beside_the_session_and_takes_the_given_window() {
    use rufous_coding::runner::turn_params;
    let d = tempfile::tempdir().expect("tmp");
    let sessions = d.path().join("sessions");
    let p = turn_params(
        "m".into(),
        d.path().to_path_buf(),
        &sessions,
        "acp_1030_120000",
        Some(32_768),
    )
    .expect("params");
    assert_eq!(p.policy, rufous_coding::harness::policy());
    assert_eq!(p.num_ctx, 32_768);
    assert_eq!(
        p.harness_dir.as_deref(),
        Some(sessions.join("acp_1030_120000.files").as_path())
    );
    // Without the flag the environment decides, as for the CLI.
    let p = turn_params("m".into(), d.path().to_path_buf(), &sessions, "s", None).expect("params");
    if std::env::var("AGENT_CTX").is_err() {
        assert_eq!(p.num_ctx, rufous_kernel::agent::DEFAULT_NUM_CTX);
    }
}

/// The real executor under a scripted model: the tools read and write files for real,
/// through whatever store the server hands the turn.
struct RealToolsFactory {
    script: Script,
}

impl AgentFactory for RealToolsFactory {
    fn new_turn(&self, setup: TurnSetup) -> rufous_kernel::Result<Box<dyn TurnRunner>> {
        let mut params = RunParams::plain(setup.model, &setup.cwd);
        params.policy = rufous_coding::harness::policy();
        let backend = Scripted {
            calls: self.script.clone(),
            i: 0,
            delay: Duration::ZERO,
            forever: false,
        };
        let mut tools = rufous_kernel::exec::Executor::new(&setup.cwd);
        tools.store = setup.files;
        let a = Agent::new(params, backend, tools)
            .with_sink(setup.sink)
            .with_stop(setup.stop);
        Ok(Box::new(a))
    }
}

/// What an editor holding `notes.txt` in a buffer sees of a turn: reads and writes.
#[derive(Default)]
struct EditorFiles {
    reads: Vec<PathBuf>,
    writes: Vec<(PathBuf, String)>,
}

/// One turn: read notes.txt, change a word in it, create new.txt, finish.
fn fs_script() -> Script {
    vec![
        ("read_file", json!({"path": "notes.txt"})),
        (
            "edit_file",
            json!({"path": "notes.txt", "old": "in the buffer", "new": "in the editor"}),
        ),
        ("create_file", json!({"path": "new.txt", "content": "made"})),
    ]
}

/// Runs the turn against a client that does or does not offer `fs`; returns the tool
/// results the editor was shown, in call order, and what the editor's file handlers saw.
async fn run_fs_turn(cwd: PathBuf, offer_fs: bool) -> (Vec<String>, EditorFiles) {
    let factory = RealToolsFactory {
        script: fs_script(),
    };
    let agent = RufousAcp::with_factory(cwd.join("home"), "scripted:1b".into(), Arc::new(factory));
    let (agent_ch, client_ch) = Channel::duplex();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let files = Arc::new(Mutex::new(EditorFiles::default()));

    let client_seen = seen.clone();
    let read_files = files.clone();
    let write_files = files.clone();
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
            async move |req: ReadTextFileRequest,
                        responder: Responder<ReadTextFileResponse>,
                        _cx| {
                let mut f = read_files.lock().expect("lock");
                f.reads.push(req.path.clone());
                // The editor's buffer differs from the disk: the user has not saved.
                let text = match f.writes.iter().rev().find(|(p, _)| *p == req.path) {
                    Some((_, written)) => written.clone(),
                    None if req.path.ends_with("notes.txt") => "in the buffer\n".to_string(),
                    None => String::new(),
                };
                responder.respond(ReadTextFileResponse::new(text))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: WriteTextFileRequest,
                        responder: Responder<WriteTextFileResponse>,
                        _cx| {
                write_files
                    .lock()
                    .expect("lock")
                    .writes
                    .push((req.path, req.content));
                responder.respond(WriteTextFileResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(client_ch, async move |cx| {
            let mut init = InitializeRequest::new(ProtocolVersion::V1);
            if offer_fs {
                init = init.client_capabilities(
                    ClientCapabilities::new().fs(FileSystemCapabilities::new()
                        .read_text_file(true)
                        .write_text_file(true)),
                );
            }
            cx.send_request(init).block_task().await?;
            let ns = cx
                .send_request(NewSessionRequest::new(cwd.clone()))
                .block_task()
                .await?;
            let p = cx
                .send_request(PromptRequest::new(
                    ns.session_id.clone(),
                    vec!["Tidy the notes".into()],
                ))
                .block_task()
                .await?;
            Ok(p)
        });

    let (agent_res, client_res) = tokio::join!(agent.connect_to(agent_ch), client);
    agent_res.expect("agent side");
    let p = client_res.expect("client side");
    assert_eq!(p.stop_reason, StopReason::EndTurn);

    let seen = seen.lock().expect("lock");
    let results: Vec<String> = seen
        .updates
        .iter()
        .filter_map(|u| match u {
            SessionUpdate::ToolCallUpdate(t) => Some(t),
            _ => None,
        })
        .filter_map(|t| {
            t.fields.content.as_ref().and_then(|c| {
                c.iter().find_map(|b| match b {
                    agent_client_protocol::schema::v1::ToolCallContent::Content(c) => {
                        match &c.content {
                            ContentBlock::Text(t) => Some(t.text.clone()),
                            _ => None,
                        }
                    }
                    _ => None,
                })
            })
        })
        .collect();
    let files = std::mem::take(&mut *files.lock().expect("lock"));
    (results, files)
}

#[tokio::test]
async fn with_the_client_fs_capability_the_tools_read_and_write_through_the_editor() {
    let d = tempfile::tempdir().expect("tmp");
    let cwd = d.path().to_path_buf();
    std::fs::write(cwd.join("notes.txt"), "on disk\n").expect("fixture");

    let (results, files) = run_fs_turn(cwd.clone(), true).await;

    // The model read the buffer, not the disk.
    assert!(
        results.iter().any(|r| r == "in the buffer\n"),
        "read_file result: {results:?}"
    );
    // edit_file matched the buffer's text and handed the editor the new text; the disk
    // beneath the buffer was left alone.
    assert!(
        results
            .iter()
            .any(|r| r.starts_with("[edited notes.txt (backed up)")),
        "{results:?}"
    );
    assert_eq!(
        files.reads,
        vec![cwd.join("notes.txt"), cwd.join("notes.txt")],
        "one read for read_file, one for edit_file"
    );
    assert_eq!(
        files.writes,
        vec![
            (cwd.join("notes.txt"), "in the editor\n".to_string()),
            (cwd.join("new.txt"), "made".to_string()),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("notes.txt")).expect("read"),
        "on disk\n"
    );
    assert!(
        !cwd.join("new.txt").exists(),
        "created in the editor, not on disk"
    );
    // The safety net still runs: the backup is the text the model saw.
    assert_eq!(
        std::fs::read_to_string(cwd.join("notes.txt.bak")).expect("bak"),
        "in the buffer\n"
    );
}

#[tokio::test]
async fn without_the_capability_the_tools_use_the_disk_and_ask_the_editor_nothing() {
    let d = tempfile::tempdir().expect("tmp");
    let cwd = d.path().to_path_buf();
    std::fs::write(cwd.join("notes.txt"), "in the buffer\n").expect("fixture");

    let (results, files) = run_fs_turn(cwd.clone(), false).await;

    assert!(files.reads.is_empty(), "{:?}", files.reads);
    assert!(files.writes.is_empty(), "{:?}", files.writes);
    assert!(
        results
            .iter()
            .any(|r| r.starts_with("[edited notes.txt (backed up)")),
        "{results:?}"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("notes.txt")).expect("read"),
        "in the editor\n"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("new.txt")).expect("read"),
        "made"
    );
}

/// Records what the server hands a turn as MCP servers; the turn itself finishes at once.
struct McpSeeingFactory {
    seen: Arc<Mutex<Vec<rufous_kernel::mcp::McpConfig>>>,
}

impl AgentFactory for McpSeeingFactory {
    fn new_turn(&self, setup: TurnSetup) -> rufous_kernel::Result<Box<dyn TurnRunner>> {
        self.seen
            .lock()
            .expect("lock")
            .push(setup.mcp_servers.clone());
        let mut params = RunParams::plain(setup.model, &setup.cwd);
        params.policy = rufous_coding::harness::policy();
        let backend = Scripted {
            calls: vec![],
            i: 0,
            delay: Duration::ZERO,
            forever: false,
        };
        let tools = Tools {
            confirmer: setup.confirmer,
            cwd: setup.cwd.clone(),
        };
        Ok(Box::new(
            Agent::new(params, backend, tools)
                .with_sink(setup.sink)
                .with_stop(setup.stop),
        ))
    }
}

#[tokio::test]
async fn the_clients_mcp_servers_reach_the_turn_as_server_configs() {
    let d = tempfile::tempdir().expect("tmp");
    let cwd = d.path().to_path_buf();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = RufousAcp::with_factory(
        cwd.join("home"),
        "scripted:1b".into(),
        Arc::new(McpSeeingFactory { seen: seen.clone() }),
    );
    let (agent_ch, client_ch) = Channel::duplex();
    let client = Client
        .builder()
        .name("test-editor")
        .connect_with(client_ch, async move |cx| {
            cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let mut notes = McpServerStdio::new("notes", "python3");
            notes.args = vec!["notes_server.py".into()];
            notes.env = vec![EnvVariable::new("NOTES_DIR", "/tmp/notes")];
            let mut docs = McpServerHttp::new("docs", "http://127.0.0.1:1/mcp");
            docs.headers = vec![HttpHeader::new("Authorization", "Bearer x")];
            let ns = cx
                .send_request(
                    NewSessionRequest::new(cwd.clone())
                        .mcp_servers(vec![McpServer::Stdio(notes), McpServer::Http(docs)]),
                )
                .block_task()
                .await?;
            cx.send_request(PromptRequest::new(
                ns.session_id.clone(),
                vec!["hello".into()],
            ))
            .block_task()
            .await?;
            // A second session without servers gets none.
            let ns2 = cx
                .send_request(NewSessionRequest::new(cwd.clone()))
                .block_task()
                .await?;
            cx.send_request(PromptRequest::new(ns2.session_id, vec!["hello".into()]))
                .block_task()
                .await?;
            Ok(())
        });
    let (agent_res, client_res) = tokio::join!(agent.connect_to(agent_ch), client);
    agent_res.expect("agent side");
    client_res.expect("client side");

    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 2);
    let first = &seen[0];
    assert_eq!(first.len(), 2, "{first:?}");
    let notes = &first["notes"];
    assert_eq!(notes.command.as_deref(), Some("python3"));
    assert_eq!(notes.args, vec!["notes_server.py".to_string()]);
    assert_eq!(
        notes.env.get("NOTES_DIR").map(String::as_str),
        Some("/tmp/notes")
    );
    assert_eq!(notes.server_categories(), vec!["mcp".to_string()]);
    let docs = &first["docs"];
    assert_eq!(docs.url.as_deref(), Some("http://127.0.0.1:1/mcp"));
    assert_eq!(
        docs.headers.get("Authorization").map(String::as_str),
        Some("Bearer x")
    );
    assert!(seen[1].is_empty());
}

/// The editor owns the project's files and nothing else (beads boubou-m64.3 and m64.5):
/// a read the editor cannot serve falls back to the disk; a write the editor refuses falls
/// back to the disk; a path outside the session cwd never reaches the editor at all (Zed
/// answered such writes with "invalid path" or left 0-byte files).
#[tokio::test]
async fn the_editor_is_asked_only_inside_the_cwd_and_the_disk_backs_it_up() {
    use rufous_coding::runner::{AcpFileStore, BridgeMsg, ClientFs, FsRequest};
    use rufous_kernel::exec::FileStore;
    let d = tempfile::tempdir().expect("tmp");
    let outside = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("a.txt"), "on disk\n").expect("w");
    std::fs::create_dir(d.path().join("dir.yaml")).expect("mkdir");
    let asked = Arc::new(Mutex::new(Vec::<(String, PathBuf)>::new()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BridgeMsg>();
    let seen = asked.clone();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if let BridgeMsg::Fs { req, reply } = m {
                let answer = match &req {
                    FsRequest::Read { path } => {
                        seen.lock()
                            .expect("lock")
                            .push(("read".into(), path.clone()));
                        Err("Internal error: reading bytes of the file".to_string())
                    }
                    FsRequest::Write { path, .. } => {
                        seen.lock()
                            .expect("lock")
                            .push(("write".into(), path.clone()));
                        if path.ends_with("refused.txt") {
                            Err("Internal error: \"invalid path\"".to_string())
                        } else {
                            Ok(String::new())
                        }
                    }
                };
                let _ = reply.send(answer);
            }
        }
    });
    let cwd = d.path().to_path_buf();
    let (file, dir, kept, refused, away) = (
        cwd.join("a.txt"),
        cwd.join("dir.yaml"),
        cwd.join("kept.txt"),
        cwd.join("refused.txt"),
        outside.path().join("skill.md"),
    );
    let (text, err) = tokio::task::spawn_blocking({
        let cwd = cwd.clone();
        let (file, dir, kept, refused, away) = (
            file.clone(),
            dir.clone(),
            kept.clone(),
            refused.clone(),
            away.clone(),
        );
        move || {
            let mut store = AcpFileStore {
                tx,
                caps: ClientFs {
                    read: true,
                    write: true,
                },
                cwd,
            };
            let text = store.read(&file).expect("the disk answers");
            let err = store.read(&dir).expect_err("a directory fails on disk too");
            store.write(&kept, "editor's").expect("the editor took it");
            store.write(&refused, "fallback").expect("the disk took it");
            store.write(&away, "outside").expect("the disk took it");
            (text, err)
        }
    })
    .await
    .expect("join");
    assert_eq!(text, "on disk\n");
    let msg = err.to_string();
    assert!(
        msg.contains("editor: Internal error") && msg.contains("os error"),
        "{msg}"
    );
    // The editor accepted kept.txt: nothing on disk (an unsaved buffer is the editor's).
    assert!(!kept.exists());
    // The editor refused refused.txt: the disk has it.
    assert_eq!(std::fs::read_to_string(&refused).expect("disk"), "fallback");
    // Outside the cwd the editor was never asked and the disk has it.
    assert_eq!(std::fs::read_to_string(&away).expect("disk"), "outside");
    let asked = asked.lock().expect("lock");
    let writes: Vec<&PathBuf> = asked
        .iter()
        .filter(|(k, _)| k == "write")
        .map(|(_, p)| p)
        .collect();
    assert_eq!(writes, vec![&kept, &refused], "{asked:?}");
}

// Bead boubou-vj3: the server builds a new gate per prompt, so the declined-write list
// (bead boubou-dml) has to be carried by the session. A write declined in one editor turn
// and tried under the same name inside the project in the next is asked about, not done.

/// The real executor with its gate on the editor's dialog, one script per prompt.
struct GatedFactory {
    scripts: Vec<Script>,
    turns: Arc<AtomicUsize>,
}

impl AgentFactory for GatedFactory {
    fn new_turn(&self, setup: TurnSetup) -> rufous_kernel::Result<Box<dyn TurnRunner>> {
        let k = self.turns.fetch_add(1, Ordering::SeqCst);
        let mut params = RunParams::plain(setup.model, &setup.cwd);
        params.policy = rufous_coding::harness::policy();
        let backend = Scripted {
            calls: self.scripts[k.min(self.scripts.len() - 1)].clone(),
            i: 0,
            delay: Duration::ZERO,
            forever: false,
        };
        let mut tools = rufous_kernel::exec::Executor::new(&setup.cwd);
        tools.gate = rufous_kernel::safety::Gate::new(
            rufous_kernel::safety::SafetyPolicy::default(),
            setup.confirmer,
        );
        let a = Agent::new(params, backend, tools)
            .with_sink(setup.sink)
            .with_stop(setup.stop);
        Ok(Box::new(a))
    }
}

#[tokio::test]
async fn a_write_declined_in_one_turn_is_asked_about_under_its_name_in_the_next() {
    let d = tempfile::tempdir().expect("tmp");
    let outside = tempfile::tempdir().expect("tmp");
    let cwd = d.path().to_path_buf();
    let target = outside.path().join("boubou_convo_perm.txt");
    let factory = GatedFactory {
        scripts: vec![
            vec![(
                "create_file",
                json!({"path": target.to_string_lossy(), "content": "hello"}),
            )],
            vec![(
                "create_file",
                json!({"path": "boubou_convo_perm.txt", "content": "hello"}),
            )],
        ],
        turns: Arc::new(AtomicUsize::new(0)),
    };
    let agent = RufousAcp::with_factory(cwd.join("home"), "scripted:1b".into(), Arc::new(factory));
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
                // The user says no both times.
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        PermissionOptionId::new("deny"),
                    )),
                ))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(client_ch, async |cx| {
            cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
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
                    vec!["Try again, please".into()],
                ))
                .block_task()
                .await?;
            Ok((p1, p2))
        });
    let (agent_res, client_res) = tokio::join!(agent.connect_to(agent_ch), client);
    agent_res.expect("agent side");
    let (p1, p2) = client_res.expect("client side");
    assert_eq!(p1.stop_reason, StopReason::EndTurn);
    assert_eq!(p2.stop_reason, StopReason::EndTurn);

    let seen = seen.lock().expect("lock");
    let actions: Vec<String> = seen
        .permissions
        .iter()
        .map(|p| {
            p.tool_call
                .fields
                .raw_input
                .as_ref()
                .and_then(|v| v.get("action"))
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string()
        })
        .collect();
    assert_eq!(
        actions,
        ["access", "write the declined file name elsewhere"],
        "{:#?}",
        seen.permissions
    );
    let second = seen.permissions[1]
        .tool_call
        .fields
        .raw_input
        .as_ref()
        .expect("raw");
    let target_text = second["target"].as_str().expect("target");
    assert!(
        target_text.starts_with("boubou_convo_perm.txt (declined at ")
            && target_text.contains("in an earlier turn), now "),
        "{target_text}"
    );
    assert!(!target.exists());
    assert!(!cwd.join("boubou_convo_perm.txt").exists());
}
