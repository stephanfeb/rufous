//! Running one prompt turn of the synchronous agent loop for the ACP server.
//!
//! The loop blocks on one model call at a time, so a turn runs on its own OS thread; the
//! thread talks to the connection task through one channel ([`BridgeMsg`]) that carries
//! loop events, permission requests (answered through a reply channel the thread blocks
//! on) and the final message list, in order. [`AgentFactory`] builds the agent for a turn;
//! the production factory is [`StandardFactory`] (the configured engine, the real tool
//! executor, the safety gate with an [`AcpConfirmer`]); tests script one.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use boubou_core::agent::session;
use boubou_core::agent::{Agent, AgentEvent, ChatBackend, RunParams, ToolRunner, TurnPolicy};
use boubou_core::config::Config;
use boubou_core::ollama::Message;
use boubou_core::safety::{ConfirmRequest, Confirmer, Gate, SafetyPolicy, Verdict};
use tokio::sync::mpsc::UnboundedSender;

/// A permission question from the safety gate, owned (the gate's request borrows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionAsk {
    pub tool: String,
    pub target: String,
    pub action: String,
    pub workdir: PathBuf,
}

/// What the turn thread sends to the connection task.
#[derive(Debug)]
pub enum BridgeMsg {
    Event(AgentEvent),
    Permission {
        ask: PermissionAsk,
        reply: std::sync::mpsc::Sender<Verdict>,
    },
    /// The loop returned: the final history, or the loop's error after the session was saved.
    Done(boubou_core::Result<Vec<Message>>),
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

/// One turn's worth of the agent: build the opening messages, run the loop.
pub trait TurnRunner {
    /// The opening message list for a fresh session (system prompt + task).
    fn task_messages(&mut self, task: &str) -> Vec<Message>;
    /// Run the loop to the end of the turn; `session` names the Boubou session files.
    fn run(
        &mut self,
        messages: Vec<Message>,
        session: Option<&str>,
    ) -> boubou_core::Result<Vec<Message>>;
}

impl<B: ChatBackend, T: ToolRunner> TurnRunner for Agent<B, T> {
    fn task_messages(&mut self, task: &str) -> Vec<Message> {
        Agent::task_messages(self, task)
    }
    fn run(
        &mut self,
        messages: Vec<Message>,
        session: Option<&str>,
    ) -> boubou_core::Result<Vec<Message>> {
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
    fn new_turn(&self, setup: TurnSetup) -> boubou_core::Result<Box<dyn TurnRunner>>;
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
) -> boubou_core::Result<RunParams> {
    let mut params = RunParams::from_env(model, cwd)?;
    params.policy = TurnPolicy::coding();
    params.harness_dir = Some(session::harness_dir(sessions_dir, session));
    if let Some(n) = num_ctx {
        params.num_ctx = n;
    }
    Ok(params)
}

impl AgentFactory for StandardFactory {
    fn new_turn(&self, setup: TurnSetup) -> boubou_core::Result<Box<dyn TurnRunner>> {
        let params = turn_params(
            setup.model,
            setup.cwd,
            &setup.sessions_dir,
            &setup.session,
            self.num_ctx,
        )?;
        let mut agent = Agent::standard(params, &self.cfg)?
            .with_sink(setup.sink)
            .with_stop(setup.stop);
        agent.tools.gate = Gate::new(SafetyPolicy::from_env(), setup.confirmer);
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
