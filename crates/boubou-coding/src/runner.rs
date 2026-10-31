//! Running one prompt turn of the synchronous agent loop for the ACP server.
//!
//! The loop blocks on one model call at a time, so a turn runs on its own OS thread; the
//! thread talks to the connection task through one channel ([`BridgeMsg`]) that carries
//! loop events, permission requests and file reads and writes for the editor (each
//! answered through a reply channel the thread blocks on) and the final message list, in
//! order. [`AgentFactory`] builds the agent for a turn; the production factory is
//! [`StandardFactory`] (the configured engine, the real tool executor, the safety gate with
//! an [`AcpConfirmer`], the executor's files through an [`AcpFileStore`]); tests script one.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use boubou_kernel::agent::session;
use boubou_kernel::agent::{Agent, AgentEvent, ChatBackend, RunParams, ToolRunner};
use boubou_kernel::config::Config;
use boubou_kernel::exec::{DiskStore, FileStore};
use boubou_kernel::mcp::{McpConfig, ServerConfig};
use boubou_kernel::ollama::Message;
use boubou_kernel::safety::{ConfirmRequest, Confirmer, Gate, SafetyPolicy, Verdict};
use tokio::sync::mpsc::UnboundedSender;

/// A permission question from the safety gate, owned (the gate's request borrows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionAsk {
    pub tool: String,
    pub target: String,
    pub action: String,
    pub workdir: PathBuf,
}

/// A file operation for the editor ([`AcpFileStore`]): `fs/read_text_file` or
/// `fs/write_text_file`, with the absolute path the protocol wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsRequest {
    Read { path: PathBuf },
    Write { path: PathBuf, content: String },
}

/// What the turn thread sends to the connection task.
#[derive(Debug)]
pub enum BridgeMsg {
    Event(AgentEvent),
    Permission {
        ask: PermissionAsk,
        reply: std::sync::mpsc::Sender<Verdict>,
    },
    /// A file read or write for the editor; the reply is the text read (empty for a
    /// write) or the client's error message.
    Fs {
        req: FsRequest,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
    },
    /// The loop returned: the final history, or the loop's error after the session was saved.
    Done(boubou_kernel::Result<Vec<Message>>),
}

/// The attended confirmer for ACP: the gate's question becomes `session/request_permission`
/// in the editor; the tool thread blocks until the user answers (or the turn is cancelled,
/// which the client reports as `cancelled` and we treat as a denial).
#[derive(Debug, Clone)]
pub struct AcpConfirmer {
    pub tx: UnboundedSender<BridgeMsg>,
}

impl Confirmer for AcpConfirmer {
    fn confirm(&mut self, req: &ConfirmRequest<'_>) -> Verdict {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let ask = PermissionAsk {
            tool: req.tool.to_string(),
            target: req.target.clone(),
            action: req.action.to_string(),
            workdir: req.workdir.to_path_buf(),
        };
        if self
            .tx
            .send(BridgeMsg::Permission { ask, reply: rtx })
            .is_err()
        {
            return Verdict::Deny;
        }
        rrx.recv().unwrap_or(Verdict::Deny)
    }
}

/// What the client said it can do for us at `initialize`
/// (`clientCapabilities.fs`). Per method: an editor may offer reads without writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientFs {
    pub read: bool,
    pub write: bool,
}

/// The executor's [`FileStore`] for an editor session: a read asks the client for the
/// file's text (`fs/read_text_file`: the buffer when the file is open, the disk otherwise)
/// and a write hands the client the new text (`fs/write_text_file`: into the buffer, saved
/// as the editor decides), each only when the client advertised that method; the rest goes
/// to the disk as for the CLI. The tool thread blocks on the reply as the confirmer does,
/// and a connection error becomes the tool's error text.
#[derive(Debug, Clone)]
pub struct AcpFileStore {
    pub tx: UnboundedSender<BridgeMsg>,
    pub caps: ClientFs,
}

impl AcpFileStore {
    /// The store for a turn: the editor's methods when it offers any, else the disk alone.
    pub fn for_turn(tx: UnboundedSender<BridgeMsg>, caps: ClientFs) -> Box<dyn FileStore> {
        if caps.read || caps.write {
            Box::new(Self { tx, caps })
        } else {
            Box::new(DiskStore)
        }
    }

    fn relay(&self, req: FsRequest) -> std::io::Result<String> {
        let (rtx, rrx) = std::sync::mpsc::channel();
        if self.tx.send(BridgeMsg::Fs { req, reply: rtx }).is_err() {
            return Err(std::io::Error::other("the editor connection is gone"));
        }
        match rrx.recv() {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(msg)) => Err(std::io::Error::other(format!("editor: {msg}"))),
            Err(_) => Err(std::io::Error::other("the editor connection is gone")),
        }
    }
}

impl FileStore for AcpFileStore {
    fn read(&mut self, path: &Path) -> std::io::Result<String> {
        if !self.caps.read {
            return DiskStore.read(path);
        }
        self.relay(FsRequest::Read {
            path: path.to_path_buf(),
        })
    }
    fn write(&mut self, path: &Path, content: &str) -> std::io::Result<()> {
        if !self.caps.write {
            return DiskStore.write(path, content);
        }
        self.relay(FsRequest::Write {
            path: path.to_path_buf(),
            content: content.to_string(),
        })
        .map(|_| ())
    }
}

/// One turn's worth of the agent: build the opening messages, run the loop.
pub trait TurnRunner {
    /// The opening message list for a fresh session (system prompt + task).
    fn task_messages(&mut self, task: &str) -> Vec<Message>;
    /// Run the loop to the end of the turn; `session` names the Boubou session files.
    fn run(
        &mut self,
        messages: Vec<Message>,
        session: Option<&str>,
    ) -> boubou_kernel::Result<Vec<Message>>;
}

impl<B: ChatBackend, T: ToolRunner> TurnRunner for Agent<B, T> {
    fn task_messages(&mut self, task: &str) -> Vec<Message> {
        Agent::task_messages(self, task)
    }
    fn run(
        &mut self,
        messages: Vec<Message>,
        session: Option<&str>,
    ) -> boubou_kernel::Result<Vec<Message>> {
        Agent::run(self, messages, session, None)
    }
}

/// Everything a turn needs from the server: where, which model, how to report, how to ask,
/// how to stop.
pub struct TurnSetup {
    pub cwd: PathBuf,
    pub model: String,
    /// Where the loop saves this session's files; `session/load` reads them back from the
    /// same place, so the server hands it down rather than letting the loop pick its own.
    pub sessions_dir: PathBuf,
    /// The Boubou session name (`acp_MMDD_HHMMSS`). The live checkpoint goes to
    /// `<sessions_dir>/<session>.state.json`, not into the editor's project.
    pub session: String,
    pub stop: Arc<AtomicBool>,
    pub sink: Box<dyn FnMut(AgentEvent) + Send + 'static>,
    pub confirmer: Box<dyn Confirmer>,
    /// Where the text-file tools read and write: the editor's buffers when the client
    /// offered `fs`, else the disk ([`AcpFileStore::for_turn`]).
    pub files: Box<dyn FileStore>,
    /// The MCP servers the client passed in `session/new` or `session/load`
    /// ([`mcp_config_from_acp`]), merged over the home and project files for the turn.
    pub mcp_servers: McpConfig,
}

/// The client's `mcpServers` as Boubou's server configs: a stdio server's command, args
/// and env, an HTTP or SSE server's url and headers (SSE is tried as streamable HTTP; the
/// probe says if that fails). Categories default to `mcp`, as for an entry without any.
pub fn mcp_config_from_acp(servers: &[agent_client_protocol::schema::v1::McpServer]) -> McpConfig {
    use agent_client_protocol::schema::v1::McpServer;
    let mut out = McpConfig::new();
    for s in servers {
        match s {
            McpServer::Stdio(st) => {
                out.insert(
                    st.name.clone(),
                    ServerConfig {
                        command: Some(st.command.display().to_string()),
                        args: st.args.clone(),
                        env: st
                            .env
                            .iter()
                            .map(|e| (e.name.clone(), e.value.clone()))
                            .collect(),
                        ..ServerConfig::default()
                    },
                );
            }
            McpServer::Http(h) => {
                out.insert(
                    h.name.clone(),
                    ServerConfig {
                        url: Some(h.url.clone()),
                        headers: h
                            .headers
                            .iter()
                            .map(|e| (e.name.clone(), e.value.clone()))
                            .collect(),
                        ..ServerConfig::default()
                    },
                );
            }
            McpServer::Sse(h) => {
                out.insert(
                    h.name.clone(),
                    ServerConfig {
                        url: Some(h.url.clone()),
                        headers: h
                            .headers
                            .iter()
                            .map(|e| (e.name.clone(), e.value.clone()))
                            .collect(),
                        ..ServerConfig::default()
                    },
                );
            }
            other => {
                tracing::warn!(target: "boubou_coding", "mcpServers: unknown transport skipped: {other:?}")
            }
        }
    }
    out
}

impl std::fmt::Debug for TurnSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnSetup")
            .field("cwd", &self.cwd)
            .field("model", &self.model)
            .field("sessions_dir", &self.sessions_dir)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// Builds the agent for one turn. Called on the turn thread, so the agent itself need not
/// be `Send`; the factory is shared across sessions.
pub trait AgentFactory: Send + Sync {
    fn new_turn(&self, setup: TurnSetup) -> boubou_kernel::Result<Box<dyn TurnRunner>>;
}

/// The production factory: `Agent::standard` on the user's `config.json`, the real executor
/// with the safety gate wired to the editor's permission dialog.
#[derive(Debug, Clone)]
pub struct StandardFactory {
    pub cfg: Config,
    /// `boubou acp --ctx`: the context window for every editor session, over `AGENT_CTX`.
    pub num_ctx: Option<u32>,
}

/// The loop parameters for one editor turn: the environment's defaults as for the CLI
/// (`AGENT_CTX` and the rest), then what an editor session needs. The turn runs under the
/// coding policy, so a text reply or `finish` ends it and the batch gates are off; the plan, backups
/// and trash go beside the session files, not into the project; and `num_ctx`, from
/// `boubou acp --ctx`, sets the context window. An editor passes no environment, so without
/// the flag the window is the 131072-token default and the model loads with a KV cache sized
/// for it (qwen3.5:9b: 17 GB under Zed, bead boubou-0lg).
pub fn turn_params(
    model: String,
    cwd: PathBuf,
    sessions_dir: &Path,
    session: &str,
    num_ctx: Option<u32>,
) -> boubou_kernel::Result<RunParams> {
    let mut params = RunParams::from_env(model, cwd)?;
    params.policy = crate::harness::policy();
    params.harness_dir = Some(session::harness_dir(sessions_dir, session));
    if let Some(n) = num_ctx {
        params.num_ctx = n;
    }
    Ok(params)
}

impl AgentFactory for StandardFactory {
    fn new_turn(&self, setup: TurnSetup) -> boubou_kernel::Result<Box<dyn TurnRunner>> {
        let params = turn_params(
            setup.model,
            setup.cwd,
            &setup.sessions_dir,
            &setup.session,
            self.num_ctx,
        )?;
        let mut agent = Agent::standard(params, &self.cfg)?
            .with_sink(setup.sink)
            .with_stop(setup.stop)
            .with_builtin_skills(crate::harness::skills());
        agent.tools.gate = Gate::new(SafetyPolicy::from_env(), setup.confirmer);
        agent.tools.store = setup.files;
        if !setup.mcp_servers.is_empty() {
            let mut servers = agent.tools.mcp.servers.clone();
            servers.extend(setup.mcp_servers);
            agent.tools.set_mcp_servers(servers);
        }
        agent.checkpoint = Some(session::state_path(&setup.sessions_dir, &setup.session));
        agent.sessions_dir = setup.sessions_dir;
        Ok(Box::new(agent))
    }
}

/// Boubou's session name for an ACP session: the same `<kind>_MMDD_HHMMSS` shape the web UI
/// uses, so the session rail lists editor conversations next to chat ones.
pub fn session_name(now: chrono::DateTime<chrono::Local>) -> String {
    now.format("acp_%m%d_%H%M%S").to_string()
}

/// A session name nothing else uses: not a live session (`taken`) and not a set of files
/// under `sessions_dir`. Two `session/new` in the same second (an editor opening several
/// threads, a restart) would otherwise share one `acp_MMDD_HHMMSS` and overwrite each
/// other's transcript and checkpoint; the second one gets `_2`, the third `_3`.
pub fn unique_session_name(
    sessions_dir: &Path,
    now: chrono::DateTime<chrono::Local>,
    taken: impl Fn(&str) -> bool,
) -> String {
    let base = session_name(now);
    let used = |n: &str| taken(n) || session_on_disk(sessions_dir, n);
    if !used(&base) {
        return base;
    }
    (2..)
        .map(|i| format!("{base}_{i}"))
        .find(|n| !used(n))
        .expect("an unused suffix exists")
}

/// Any file the loop writes for a session of this name.
fn session_on_disk(sessions_dir: &Path, name: &str) -> bool {
    [
        ".json",
        ".meta.json",
        ".state.json",
        ".transcript.jsonl",
        ".files",
    ]
    .iter()
    .any(|s| sessions_dir.join(format!("{name}{s}")).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_sessions_in_the_same_second_get_different_names() {
        let d = tempfile::tempdir().expect("tmp");
        let now = chrono::Local::now();
        let base = session_name(now);
        // Nothing live, nothing on disk: the plain name.
        assert_eq!(unique_session_name(d.path(), now, |_| false), base);
        // A live session with that name: the next suffix.
        let live = base.clone();
        assert_eq!(
            unique_session_name(d.path(), now, |n| n == live),
            format!("{base}_2")
        );
        // Files from an earlier process count too, whichever of them survived.
        std::fs::write(d.path().join(format!("{base}.json")), "[]").expect("w");
        std::fs::create_dir_all(d.path().join(format!("{base}_2.files"))).expect("d");
        assert_eq!(
            unique_session_name(d.path(), now, |_| false),
            format!("{base}_3")
        );
    }
}
