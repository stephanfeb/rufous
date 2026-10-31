//! The coding harness: Boubou as an [Agent Client Protocol](https://agentclientprotocol.com)
//! agent in an editor. [`harness`] is what makes it its own: the `coding` turn policy, its
//! task prompt and the skills built into it; the rest is the protocol server.
//!
//! An editor (Zed, JetBrains, Neovim) spawns `boubou acp --model <tag>` and speaks JSON-RPC
//! 2.0 over stdio. One Boubou session per ACP session, working directory = the session's
//! `cwd`, history kept in memory and saved to Boubou's session files like a web chat.
//! `session/prompt` runs one turn of the loop on a thread; the loop's events stream back as
//! `session/update` notifications ([`bridge`]); the safety gate's questions become
//! `session/request_permission` ([`runner::AcpConfirmer`]); `session/cancel` raises the
//! loop's stop flag.
//!
//! `session/load` reopens a session after an agent restart: the ACP session id is the Boubou
//! session name, so the history is read back from the sessions directory and replayed to the
//! editor as `session/update` notifications before the response ([`bridge::replay_updates`]).
//!
//! When the client advertises `fs.readTextFile` / `fs.writeTextFile` at `initialize`, the
//! text-file tools read and write through the editor ([`runner::AcpFileStore`]), so an
//! unsaved buffer is what the model sees and an edit lands in it. The MCP servers the
//! client passes in `session/new` or `session/load` are merged over Boubou's `mcp.json` and
//! the project's `.mcp.json` for that session ([`runner::mcp_config_from_acp`]).
//!
//! Protocol v1 (stable). Not yet: session modes, the client's `terminal` capability.
//! stdout belongs to the protocol; diagnostics go to stderr via `tracing`.

pub mod bridge;
pub mod harness;
pub mod runner;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{
    AgentCapabilities, CancelNotification, Implementation, InitializeRequest, InitializeResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse,
    PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse, ReadTextFileRequest,
    RequestPermissionOutcome, RequestPermissionRequest, SessionId, SessionNotification, ToolCallId,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, WriteTextFileRequest,
};
use agent_client_protocol::{
    Agent as AcpAgent, Client as AcpClient, ConnectTo, ConnectionTo, Error, Responder, Stdio,
};
use boubou_kernel::ollama::Message;
use boubou_kernel::safety::Verdict;
use tokio::sync::mpsc;

use bridge::{kind_for, prompt_text, replay_updates, TurnBridge};
use runner::{
    AcpConfirmer, AcpFileStore, AgentFactory, BridgeMsg, ClientFs, FsRequest, PermissionAsk,
    StandardFactory, TurnSetup,
};

/// One ACP session: Boubou's conversation state between prompts.
#[derive(Debug)]
struct SessionState {
    cwd: PathBuf,
    /// Boubou session name (files under the sessions directory).
    name: String,
    messages: Vec<Message>,
    stop: Arc<AtomicBool>,
    busy: bool,
    /// The client's `mcpServers` for this session, merged over the files at each turn.
    mcp_servers: boubou_kernel::mcp::McpConfig,
}

type Sessions = Arc<Mutex<HashMap<SessionId, SessionState>>>;

/// The ACP agent component. Implements [`ConnectTo<Client>`] so it runs over stdio
/// ([`BoubouAcp::serve_stdio`]) or an in-process channel (tests).
pub struct BoubouAcp {
    /// Boubou's data directory: sessions are saved under `<home>/sessions` and `session/load`
    /// reads them back from there.
    pub home: PathBuf,
    pub model: String,
    pub factory: Arc<dyn AgentFactory>,
    sessions: Sessions,
    /// The client's `fs` capabilities from `initialize`; none until it has been received.
    client_fs: Arc<Mutex<ClientFs>>,
}

impl std::fmt::Debug for BoubouAcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoubouAcp")
            .field("home", &self.home)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl BoubouAcp {
    /// The production agent: the user's `config.json`, the real engine and executor.
    /// `num_ctx` is `boubou acp --ctx`, the context window for every session.
    pub fn standard(
        cfg: boubou_kernel::config::Config,
        home: PathBuf,
        model: String,
        num_ctx: Option<u32>,
    ) -> Self {
        Self::with_factory(home, model, Arc::new(StandardFactory { cfg, num_ctx }))
    }

    pub fn with_factory(home: PathBuf, model: String, factory: Arc<dyn AgentFactory>) -> Self {
        Self {
            home,
            model,
            factory,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            client_fs: Arc::new(Mutex::new(ClientFs::default())),
        }
    }

    /// Serve the editor on this process's stdin/stdout until it disconnects.
    pub async fn serve_stdio(self) -> Result<(), Error> {
        self.connect_to(Stdio::new()).await
    }
}

impl ConnectTo<AcpClient> for BoubouAcp {
    async fn connect_to(self, client: impl ConnectTo<AcpAgent>) -> Result<(), Error> {
        let BoubouAcp {
            home,
            model,
            factory,
            sessions,
            client_fs,
        } = self;
        let sessions_dir = boubou_kernel::agent::session::sessions_dir(&home);
        AcpAgent
            .builder()
            .name("boubou")
            .on_receive_request(
                {
                    let client_fs = client_fs.clone();
                    async move |req: InitializeRequest,
                                responder: Responder<InitializeResponse>,
                                _cx: ConnectionTo<AcpClient>| {
                    let fs = ClientFs {
                        read: req.client_capabilities.fs.read_text_file,
                        write: req.client_capabilities.fs.write_text_file,
                    };
                    *lock(&client_fs) = fs;
                    tracing::info!(
                        target: "boubou_coding",
                        version = ?req.protocol_version,
                        fs_read = fs.read,
                        fs_write = fs.write,
                        "initialize"
                    );
                    responder.respond(
                        InitializeResponse::new(req.protocol_version)
                            .agent_capabilities(AgentCapabilities::new().load_session(true))
                            .agent_info(
                                Implementation::new("boubou", boubou_kernel::VERSION).title("Boubou"),
                            ),
                    )
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let sessions = sessions.clone();
                    let sessions_dir = sessions_dir.clone();
                    async move |req: NewSessionRequest,
                                responder: Responder<NewSessionResponse>,
                                _cx: ConnectionTo<AcpClient>| {
                        if !req.cwd.is_absolute() {
                            return responder.respond_with_error(
                                Error::invalid_params().data(serde_json::json!({
                                    "message": "cwd must be an absolute path"
                                })),
                            );
                        }
                        let mcp_servers = runner::mcp_config_from_acp(&req.mcp_servers);
                        if !mcp_servers.is_empty() {
                            tracing::info!(
                                target: "boubou_coding",
                                servers = ?mcp_servers.keys().collect::<Vec<_>>(),
                                "session/new mcpServers merged for the session"
                            );
                        }
                        let name = runner::unique_session_name(
                            &sessions_dir,
                            chrono::Local::now(),
                            |n| lock(&sessions).values().any(|s| s.name == n),
                        );
                        let id = SessionId::new(name.clone());
                        let st = SessionState {
                            cwd: req.cwd.clone(),
                            name,
                            messages: Vec::new(),
                            stop: Arc::new(AtomicBool::new(false)),
                            busy: false,
                            mcp_servers,
                        };
                        lock(&sessions).insert(id.clone(), st);
                        tracing::info!(target: "boubou_coding", session = %id, cwd = %req.cwd.display(), "session/new");
                        responder.respond(NewSessionResponse::new(id))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let sessions = sessions.clone();
                    let sessions_dir = sessions_dir.clone();
                    async move |req: LoadSessionRequest,
                                responder: Responder<LoadSessionResponse>,
                                cx: ConnectionTo<AcpClient>| {
                        if !req.cwd.is_absolute() {
                            return responder.respond_with_error(
                                Error::invalid_params().data(serde_json::json!({
                                    "message": "cwd must be an absolute path"
                                })),
                            );
                        }
                        let name = req.session_id.0.to_string();
                        if !is_session_name(&name) {
                            return responder.respond_with_error(
                                Error::invalid_params().data(serde_json::json!({
                                    "message": "not a Boubou session id"
                                })),
                            );
                        }
                        let mcp_servers = runner::mcp_config_from_acp(&req.mcp_servers);
                        if !mcp_servers.is_empty() {
                            tracing::info!(
                                target: "boubou_coding",
                                servers = ?mcp_servers.keys().collect::<Vec<_>>(),
                                "session/load mcpServers merged for the session"
                            );
                        }
                        let Some(messages) =
                            boubou_kernel::agent::session::load_session(&sessions_dir, &name)
                        else {
                            return responder
                                .respond_with_error(Error::session_not_found(req.session_id));
                        };
                        // A session this process already holds is reloaded from disk (the
                        // saved copy is the end of its last turn) unless a turn is running.
                        let stop = {
                            let g = lock(&sessions);
                            match g.get(&req.session_id) {
                                Some(s) if s.busy => {
                                    return responder.respond_with_error(
                                        Error::invalid_request().data(serde_json::json!({
                                            "message": "a prompt is running in this session"
                                        })),
                                    )
                                }
                                Some(s) => s.stop.clone(),
                                None => Arc::new(AtomicBool::new(false)),
                            }
                        };
                        let plan_dir =
                            boubou_kernel::agent::session::harness_dir(&sessions_dir, &name);
                        let updates = replay_updates(&messages, &req.cwd, &plan_dir);
                        let replayed = updates.len();
                        for upd in updates {
                            if let Err(e) = cx.send_notification(SessionNotification::new(
                                req.session_id.clone(),
                                upd,
                            )) {
                                tracing::warn!(target: "boubou_coding", "session/update (replay): {e}");
                            }
                        }
                        let st = SessionState {
                            cwd: req.cwd.clone(),
                            name,
                            messages,
                            stop,
                            busy: false,
                            mcp_servers,
                        };
                        lock(&sessions).insert(req.session_id.clone(), st);
                        tracing::info!(
                            target: "boubou_coding",
                            session = %req.session_id,
                            cwd = %req.cwd.display(),
                            replayed,
                            "session/load"
                        );
                        responder.respond(LoadSessionResponse::new())
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let sessions = sessions.clone();
                    let sessions_dir = sessions_dir.clone();
                    let factory = factory.clone();
                    let model = model.clone();
                    let client_fs = client_fs.clone();
                    async move |req: PromptRequest,
                                responder: Responder<PromptResponse>,
                                cx: ConnectionTo<AcpClient>| {
                        // Take the session's state for the turn; the dispatch loop is held
                        // only for this bookkeeping, the turn itself runs elsewhere.
                        let taken = {
                            let mut g = lock(&sessions);
                            match g.get_mut(&req.session_id) {
                                None => Err(Error::session_not_found(req.session_id.clone())),
                                Some(s) if s.busy => Err(Error::invalid_request().data(
                                    serde_json::json!({"message": "a prompt is already running in this session"}),
                                )),
                                Some(s) => {
                                    s.busy = true;
                                    s.stop.store(false, Ordering::SeqCst);
                                    Ok((
                                        s.cwd.clone(),
                                        s.name.clone(),
                                        std::mem::take(&mut s.messages),
                                        s.stop.clone(),
                                        s.mcp_servers.clone(),
                                    ))
                                }
                            }
                        };
                        let (cwd, name, prior, stop, mcp_servers) = match taken {
                            Ok(t) => t,
                            Err(e) => return responder.respond_with_error(e),
                        };
                        let text = prompt_text(&req.prompt);
                        let plan_dir =
                            boubou_kernel::agent::session::harness_dir(&sessions_dir, &name);
                        // The session's protocol log (bead boubou-bmq): the editor keeps
                        // none of the loop's lines, so they go beside the session files.
                        let log = boubou_kernel::agent::session::SessionLog::open(
                            &sessions_dir,
                            &name,
                        );
                        if let Some(l) = &log {
                            l.line(&format!(
                                "[turn] {}",
                                boubou_kernel::text::head(&text, 120).replace('\n', " ")
                            ));
                        }
                        let session_id = req.session_id.clone();
                        let (tx, rx) = mpsc::unbounded_channel::<BridgeMsg>();
                        spawn_turn(
                            factory.clone(),
                            TurnSetup {
                                cwd: cwd.clone(),
                                model: model.clone(),
                                sessions_dir: sessions_dir.clone(),
                                session: name.clone(),
                                stop: stop.clone(),
                                sink: {
                                    let tx = tx.clone();
                                    let send = move |ev| {
                                        let _ = tx.send(BridgeMsg::Event(ev));
                                    };
                                    match log {
                                        Some(l) => Box::new(l.tee(send)),
                                        None => Box::new(send),
                                    }
                                },
                                confirmer: Box::new(AcpConfirmer { tx: tx.clone() }),
                                files: AcpFileStore::for_turn(
                                    tx.clone(),
                                    *lock(&client_fs),
                                    cwd.clone(),
                                ),
                                mcp_servers,
                            },
                            prior.clone(),
                            text,
                            name,
                            tx,
                        );
                        let sessions = sessions.clone();
                        let task_cx = cx.clone();
                        cx.spawn(async move {
                            let cx = task_cx;
                            let outcome =
                                drive_turn(&cx, session_id.clone(), cwd, plan_dir, rx, stop.clone())
                                    .await;
                            let (result, bridge) = outcome;
                            let cancelled = stop.load(Ordering::SeqCst);
                            let mut g = lock(&sessions);
                            if let Some(s) = g.get_mut(&session_id) {
                                s.busy = false;
                                s.messages = match &result {
                                    Ok(m) => m.clone(),
                                    Err(_) => prior,
                                };
                            }
                            drop(g);
                            match result {
                                Ok(_) => responder
                                    .respond(PromptResponse::new(bridge.stop_reason(cancelled))),
                                Err(e) => responder.respond_with_error(
                                    Error::new(-32603, format!("agent loop: {e}")),
                                ),
                            }
                        })?;
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_notification(
                {
                    let sessions = sessions.clone();
                    async move |notif: CancelNotification, _cx: ConnectionTo<AcpClient>| {
                        if let Some(s) = lock(&sessions).get(&notif.session_id) {
                            s.stop.store(true, Ordering::SeqCst);
                            tracing::info!(target: "boubou_coding", session = %notif.session_id, "session/cancel");
                        }
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_to(client)
            .await
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A Boubou session name as `session/new` issues them (and as the web UI names chats): one
/// path component of letters, digits, `_` and `-`. Anything else cannot name a file under
/// the sessions directory and is refused before touching the disk.
fn is_session_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Run one turn of the loop on its own thread; everything it has to say goes through `tx`,
/// ending with [`BridgeMsg::Done`].
fn spawn_turn(
    factory: Arc<dyn AgentFactory>,
    setup: TurnSetup,
    prior: Vec<Message>,
    text: String,
    session_name: String,
    tx: mpsc::UnboundedSender<BridgeMsg>,
) {
    let spawned = std::thread::Builder::new()
        .name("boubou-coding-turn".into())
        .spawn(move || {
            let result = (|| {
                let mut agent = factory.new_turn(setup)?;
                let messages = if prior.is_empty() {
                    agent.task_messages(&text)
                } else {
                    let mut m = prior;
                    m.push(Message::user(&text));
                    m
                };
                agent.run(messages, Some(&session_name))
            })();
            let _ = tx.send(BridgeMsg::Done(result));
        });
    if let Err(e) = spawned {
        tracing::error!(target: "boubou_coding", "turn thread: {e}");
    }
}

/// The connection side of a turn: forward events as notifications, relay permission
/// questions to the client, return the loop's result.
async fn drive_turn(
    cx: &ConnectionTo<AcpClient>,
    session_id: SessionId,
    cwd: PathBuf,
    plan_dir: PathBuf,
    mut rx: mpsc::UnboundedReceiver<BridgeMsg>,
    stop: Arc<AtomicBool>,
) -> (boubou_kernel::Result<Vec<Message>>, TurnBridge) {
    let mut bridge = TurnBridge::new(cwd).with_plan_dir(plan_dir);
    let mut perm_n = 0u64;
    while let Some(msg) = rx.recv().await {
        match msg {
            BridgeMsg::Event(ev) => {
                if let boubou_kernel::agent::AgentEvent::Log(l) = &ev {
                    tracing::info!(target: "boubou_coding", "{l}");
                }
                for upd in bridge.on_event(ev) {
                    if let Err(e) =
                        cx.send_notification(SessionNotification::new(session_id.clone(), upd))
                    {
                        tracing::warn!(target: "boubou_coding", "session/update: {e}");
                    }
                }
            }
            BridgeMsg::Permission { ask, reply } => {
                perm_n += 1;
                let verdict = ask_permission(cx, &session_id, &bridge, &ask, perm_n, &stop).await;
                let _ = reply.send(verdict);
            }
            BridgeMsg::Fs { req, reply } => {
                let _ = reply.send(editor_fs(cx, &session_id, req).await);
            }
            BridgeMsg::Done(result) => return (result, bridge),
        }
    }
    (
        Err(boubou_kernel::Error::Config(
            "turn thread ended without a result".to_string(),
        )),
        bridge,
    )
}

/// `session/request_permission` for one gate question. The subject is the tool call in
/// flight; the options map onto the gate's verdicts (allow once, allow for the rest of this
/// run, deny). A cancelled turn, or a connection error, denies.
async fn ask_permission(
    cx: &ConnectionTo<AcpClient>,
    session_id: &SessionId,
    bridge: &TurnBridge,
    ask: &PermissionAsk,
    n: u64,
    stop: &AtomicBool,
) -> Verdict {
    if stop.load(Ordering::SeqCst) {
        return Verdict::Deny;
    }
    let id = bridge
        .current_call()
        .unwrap_or_else(|| ToolCallId::new(format!("permission_{n}")));
    let subject = ToolCallUpdate::new(
        id,
        ToolCallUpdateFields::new()
            .title(format!("{}: {}", ask.action, ask.target))
            .kind(kind_for(&ask.tool))
            .status(ToolCallStatus::Pending)
            .raw_input(serde_json::json!({
                "tool": ask.tool,
                "action": ask.action,
                "target": ask.target,
                "workdir": ask.workdir,
            })),
    );
    let options = vec![
        PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
        PermissionOption::new(
            "allow_all",
            format!("Allow {} for the rest of this turn", ask.tool),
            PermissionOptionKind::AllowAlways,
        ),
        PermissionOption::new("deny", "Deny", PermissionOptionKind::RejectOnce),
    ];
    let req = RequestPermissionRequest::new(session_id.clone(), subject, options);
    match cx.send_request(req).block_task().await {
        Ok(resp) => match resp.outcome {
            RequestPermissionOutcome::Selected(s) => match s.option_id.0.as_ref() {
                "allow" => Verdict::Allow,
                "allow_all" => Verdict::AllowAll,
                _ => Verdict::Deny,
            },
            RequestPermissionOutcome::Cancelled => {
                stop.store(true, Ordering::SeqCst);
                Verdict::Deny
            }
            // The enum is non-exhaustive: an outcome this version does not know is a denial.
            _ => Verdict::Deny,
        },
        Err(e) => {
            tracing::warn!(target: "boubou_coding", "request_permission: {e}");
            Verdict::Deny
        }
    }
}

/// One file operation through the editor: `fs/read_text_file` returns the text (the open
/// buffer, else the disk); `fs/write_text_file` returns nothing. The client's error, or the
/// connection's, is passed back as text for the tool result.
async fn editor_fs(
    cx: &ConnectionTo<AcpClient>,
    session_id: &SessionId,
    req: FsRequest,
) -> Result<String, String> {
    match req {
        FsRequest::Read { path } => {
            tracing::info!(target: "boubou_coding", path = %path.display(), "fs/read_text_file");
            cx.send_request(ReadTextFileRequest::new(session_id.clone(), path))
                .block_task()
                .await
                .map(|r| r.content)
                .map_err(|e| e.to_string())
        }
        FsRequest::Write { path, content } => {
            tracing::info!(
                target: "boubou_coding",
                path = %path.display(),
                bytes = content.len(),
                "fs/write_text_file"
            );
            cx.send_request(WriteTextFileRequest::new(session_id.clone(), path, content))
                .block_task()
                .await
                .map(|_| String::new())
                .map_err(|e| e.to_string())
        }
    }
}

/// Re-exported for embedders that compose the component themselves.
pub use agent_client_protocol as acp;
