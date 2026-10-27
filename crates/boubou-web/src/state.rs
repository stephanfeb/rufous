//! Shared server state: the agent home, workdir, current session, the pending approval
//! request, the running agent's handles, and the event bus.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use boubou_core::agent::session::sessions_dir;
use boubou_core::config::Config;
use boubou_core::safety::{ConfirmRequest, Confirmer, Verdict};
use serde_json::{json, Map, Value};

use crate::events::Bus;
use crate::runner::{AgentSpawner, RunRequest, Spawner};

/// `~/agent_tasks/work`, Mingbird's default workdir for the web UI.
pub fn default_workdir() -> PathBuf {
    dirs_home().join("agent_tasks").join("work")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// How long an approval dialog may stay unanswered before it counts as "deny" (Mingbird: 120 s).
pub const ASK_TIMEOUT: Duration = Duration::from_secs(120);

struct PendingAsk {
    id: String,
    answer: mpsc::Sender<String>,
}

/// The human-in-the-loop channel for the web UI: the gate blocks on it, `POST /api/ask`
/// answers it. Only the *current* request's id is accepted, so a stale "allow" can never
/// pre-approve the next out-of-bounds call.
#[derive(Default)]
pub struct AskHub {
    pending: Mutex<Option<PendingAsk>>,
    seq: AtomicU64,
}

impl std::fmt::Debug for AskHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskHub")
            .field("pending", &self.pending_id())
            .finish()
    }
}

impl AskHub {
    pub fn pending_id(&self) -> Option<String> {
        self.pending
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|p| p.id.clone()))
    }

    /// Deliver an answer for `id`; `false` when nothing (or something else) is pending.
    pub fn answer(&self, id: &str, answer: &str) -> bool {
        let Ok(mut g) = self.pending.lock() else {
            return false;
        };
        match g.as_ref() {
            Some(p) if p.id == id => {
                let _ = p.answer.send(answer.to_string());
                *g = None;
                true
            }
            _ => false,
        }
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.pending.lock() {
            *g = None;
        }
    }

    /// Register a new request and block for its answer (or the timeout).
    fn ask(&self, bus: &Bus, req: Value) -> Verdict {
        let id = format!("ask_{}", self.seq.fetch_add(1, Ordering::SeqCst) + 1);
        let (tx, rx) = mpsc::channel();
        if let Ok(mut g) = self.pending.lock() {
            *g = Some(PendingAsk {
                id: id.clone(),
                answer: tx,
            });
        }
        let mut extra = Map::new();
        extra.insert("ask_id".into(), Value::String(id.clone()));
        bus.push("ask", req, extra);
        let verdict = match rx.recv_timeout(ASK_TIMEOUT).as_deref() {
            Ok("allow") => Verdict::Allow,
            Ok("allow_all") => Verdict::AllowAll,
            _ => Verdict::Deny,
        };
        if let Ok(mut g) = self.pending.lock() {
            if g.as_ref().is_some_and(|p| p.id == id) {
                *g = None;
            }
        }
        verdict
    }
}

/// The [`Confirmer`] handed to the agent's gate: publishes an `ask` event and waits.
#[derive(Debug, Clone)]
pub struct WebConfirmer {
    pub hub: Arc<AskHub>,
    pub bus: Arc<Bus>,
}

impl Confirmer for WebConfirmer {
    fn confirm(&mut self, req: &ConfirmRequest<'_>) -> Verdict {
        // Same payload Mingbird printed after `@@ASK@@`.
        let payload = json!({
            "tool": req.tool,
            "path": req.target,
            "action": req.action,
            "workdir": req.workdir.to_string_lossy(),
        });
        self.hub.ask(&self.bus, payload)
    }
}

/// Handles of the agent currently running (one at a time, as in Mingbird).
#[derive(Debug)]
pub struct RunHandle {
    pub stop: Arc<AtomicBool>,
    pub done: Arc<AtomicBool>,
}

pub struct AppState {
    pub home: PathBuf,
    /// Where bundled assets (skills, `stt/` models) sit next to the executable.
    pub install_dir: PathBuf,
    pub bus: Arc<Bus>,
    pub ask: Arc<AskHub>,
    pub workdir: Mutex<PathBuf>,
    pub session: Mutex<Option<String>>,
    run: Mutex<Option<Arc<RunHandle>>>,
    spawner: Box<dyn Spawner>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("home", &self.home)
            .field("busy", &self.busy())
            .finish()
    }
}

impl AppState {
    /// Production state: the real agent runner.
    pub fn new(home: PathBuf, workdir: PathBuf) -> Self {
        Self::with_spawner(home, workdir, Box::new(AgentSpawner))
    }

    pub fn with_spawner(home: PathBuf, workdir: PathBuf, spawner: Box<dyn Spawner>) -> Self {
        Self {
            home,
            install_dir: std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| PathBuf::from(".")),
            bus: Arc::new(Bus::new()),
            ask: Arc::new(AskHub::default()),
            workdir: Mutex::new(workdir),
            session: Mutex::new(None),
            run: Mutex::new(None),
            spawner,
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.home.join("config.json")
    }

    pub fn load_config(&self) -> Config {
        Config::load_from(&self.config_path()).unwrap_or_default()
    }

    pub fn sessions_dir(&self) -> PathBuf {
        sessions_dir(&self.home)
    }

    pub fn workdir(&self) -> PathBuf {
        self.workdir
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| default_workdir())
    }

    pub fn session(&self) -> Option<String> {
        self.session.lock().ok().and_then(|g| g.clone())
    }

    pub fn set_session(&self, s: Option<String>) {
        if let Ok(mut g) = self.session.lock() {
            *g = s;
        }
    }

    pub fn busy(&self) -> bool {
        self.run
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|h| !h.done.load(Ordering::SeqCst)))
            .unwrap_or(false)
    }

    /// `start_agent`: `false` when a run is already in progress.
    pub fn start(self: &Arc<Self>, prompt: String, model_key: String, resume: bool) -> bool {
        let Ok(mut g) = self.run.lock() else {
            return false;
        };
        if g.as_ref().is_some_and(|h| !h.done.load(Ordering::SeqCst)) {
            return false;
        }
        self.ask.clear();
        let session = self
            .session()
            .unwrap_or_else(|| chrono::Local::now().format("chat_%m%d_%H%M%S").to_string());
        self.set_session(Some(session.clone()));
        let cfg = self.load_config();
        let model = cfg.models.get(&model_key).cloned().unwrap_or(model_key);
        let req = RunRequest {
            prompt,
            model,
            resume,
            session,
            workdir: self.workdir(),
            home: self.home.clone(),
            prefs: crate::prefs::load(&self.home),
            config: cfg,
        };
        let handle = Arc::new(RunHandle {
            stop: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(false)),
        });
        *g = Some(handle.clone());
        drop(g);
        self.bus
            .note(format!("====== start · {} ======", req.model));
        let confirmer = WebConfirmer {
            hub: self.ask.clone(),
            bus: self.bus.clone(),
        };
        let st = self.clone();
        std::thread::Builder::new()
            .name("boubou-agent".into())
            .spawn(move || {
                st.spawner
                    .run(req, &handle.stop, st.bus.clone(), Box::new(confirmer));
                handle.done.store(true, Ordering::SeqCst);
                st.ask.clear();
                st.bus.push("exit", json!(0), Map::new());
            })
            .is_ok()
    }

    /// `kill_tree`: raise the stop flag; the loop ends at its next check.
    pub fn stop(&self) {
        if let Ok(g) = self.run.lock() {
            if let Some(h) = g.as_ref() {
                h.stop.store(true, Ordering::SeqCst);
            }
        }
        self.ask.clear();
    }

    pub fn home_path(&self) -> &Path {
        &self.home
    }
}
