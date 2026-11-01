//! The Ollama *process* (`_find_ollama_exe`, `ensure_ollama`, `restart_ollama`): start it
//! when the API is down, and restart it when a long run has degraded it (four consecutive
//! 500s at temperature 0 replay the same bad path; compaction and waiting cannot escape, a
//! restart does, and the checkpoint keeps the task).
//!
//! The server is usually not ours: the Ollama app on macOS and the systemd unit on Linux
//! supervise their own `ollama serve` and relaunch it when it dies. Seen live on
//! 2026-10-29: the self-heal killed the app's server, the app relaunched it, and rufous,
//! finding the API still down two seconds later, started a second one; both bound 11434,
//! the model was loaded twice and the machine swapped. So a server rufous did not start is
//! given [`OllamaProc::supervisor_grace`] to come back on its own after a kill, a bound
//! port is waited for rather than spawned over, and the one server rufous does start is
//! remembered and killed by handle, not by name.
//!
//! Everything here is synchronous and runtime-free on the outside: the HTTP liveness probe
//! runs on a scoped thread with its own runtime, so it is safe from inside the loop thread
//! (which owns a runtime of its own) and from a tokio worker alike.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::Config;

/// Run a future to completion on a fresh current-thread runtime, on its own thread (never
/// nests inside a caller's runtime).
pub fn block_on_fresh<T: Send>(fut: impl std::future::Future<Output = T> + Send) -> T {
    std::thread::scope(|s| {
        s.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match rt {
                Ok(rt) => Some(rt.block_on(fut)),
                Err(_) => None,
            }
        })
        .join()
        .ok()
        .flatten()
    })
    .expect("fresh runtime thread panicked or failed to build")
}

/// `GET {host}/api/tags` within `timeout` → Ollama is serving.
pub fn api_alive(host: &str, timeout: Duration) -> bool {
    let url = format!("{}/api/tags", host.trim_end_matches('/'));
    block_on_fresh(async move {
        let Ok(http) = reqwest::Client::builder().timeout(timeout).build() else {
            return false;
        };
        http.get(url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    })
}

/// `_find_ollama_exe`: `OLLAMA_BIN` env > `config.ollama_exe` > `PATH` > common install
/// directories; falls back to the bare name `ollama`. Nothing machine-specific is hard-coded.
pub fn find_ollama_exe(configured: &str) -> PathBuf {
    for cand in [
        std::env::var("OLLAMA_BIN").unwrap_or_default(),
        configured.to_string(),
    ] {
        let c = cand.trim();
        if !c.is_empty() && Path::new(c).is_file() {
            return PathBuf::from(c);
        }
    }
    if let Some(p) = which("ollama") {
        return p;
    }
    let mut common: Vec<PathBuf> = Vec::new();
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        if !la.is_empty() {
            common.push(
                Path::new(&la)
                    .join("Programs")
                    .join("Ollama")
                    .join("ollama.exe"),
            );
        }
    }
    common.push(PathBuf::from(r"C:\Program Files\Ollama\ollama.exe"));
    common.push(PathBuf::from("/usr/local/bin/ollama"));
    common.push(PathBuf::from("/opt/homebrew/bin/ollama"));
    common.push(PathBuf::from(
        "/Applications/Ollama.app/Contents/Resources/ollama",
    ));
    common
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("ollama"))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        vec![".exe".into(), String::new()]
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// The socket a host URL names (`http://127.0.0.1:11434` → `127.0.0.1:11434`).
pub fn host_addr(host: &str) -> Option<SocketAddr> {
    let url = reqwest::Url::parse(host.trim()).ok()?;
    let h = url.host_str()?;
    let port = url.port_or_known_default()?;
    (h, port).to_socket_addrs().ok()?.next()
}

/// Does something accept connections on the host's port? True while a server is loading
/// or restarting and not yet answering `/api/tags`; false once the port is free.
pub fn port_open(host: &str, timeout: Duration) -> bool {
    host_addr(host).is_some_and(|a| TcpStream::connect_timeout(&a, timeout).is_ok())
}

/// How long a server rufous did not start is given to be relaunched by its supervisor.
pub const DEFAULT_SUPERVISOR_GRACE: Duration = Duration::from_secs(20);

/// How to reach and (re)start this machine's Ollama. Built from the config; carried by the
/// Ollama backend so the loop can ask for a restart without knowing any of this. Clones
/// share the handle of the server rufous started, if any.
#[derive(Debug, Clone)]
pub struct OllamaProc {
    pub host: String,
    pub exe: String,
    pub env: BTreeMap<String, String>,
    /// After killing a server rufous did not start, how long its own supervisor (the
    /// Ollama app, systemd, `brew services`) gets to bring it back before rufous starts one.
    pub supervisor_grace: Duration,
    /// The `ollama serve` rufous spawned, while it runs.
    owned: Arc<Mutex<Option<Child>>>,
}

impl PartialEq for OllamaProc {
    fn eq(&self, o: &Self) -> bool {
        self.host == o.host && self.exe == o.exe && self.env == o.env
    }
}
impl Eq for OllamaProc {}

impl OllamaProc {
    pub fn from_config(cfg: &Config) -> Self {
        Self::new(
            cfg.ollama_host(),
            cfg.ollama_exe.clone(),
            cfg.ollama_env.clone(),
        )
    }

    pub fn new(host: String, exe: String, env: BTreeMap<String, String>) -> Self {
        Self {
            host,
            exe,
            env,
            supervisor_grace: DEFAULT_SUPERVISOR_GRACE,
            owned: Arc::new(Mutex::new(None)),
        }
    }

    pub fn alive(&self, timeout: Duration) -> bool {
        api_alive(&self.host, timeout)
    }

    /// Is the server rufous started still running?
    pub fn owns_server(&self) -> bool {
        let mut guard = match self.owned.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        match guard.as_mut() {
            Some(c) => match c.try_wait() {
                Ok(None) => true,
                _ => {
                    *guard = None;
                    false
                }
            },
            None => false,
        }
    }

    /// Spawn `ollama serve` detached, with the configured extra environment (GPU flags etc.),
    /// and remember it as ours.
    pub fn spawn_serve(&self) -> std::io::Result<()> {
        let exe = find_ollama_exe(&self.exe);
        let mut cmd = Command::new(&exe);
        cmd.arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(dir) = exe.parent().filter(|d| !d.as_os_str().is_empty()) {
            cmd.current_dir(dir);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = cmd.spawn()?;
        match self.owned.lock() {
            Ok(mut g) => *g = Some(child),
            Err(p) => *p.into_inner() = Some(child),
        }
        Ok(())
    }

    /// `ensure_ollama`: reachable → true at once; a bound port that is not answering yet
    /// (a server loading or being relaunched) is waited for; only a free port gets a
    /// `serve` of our own. Waits up to `timeout` for the API either way.
    pub fn ensure(&self, timeout: Duration) -> bool {
        if self.alive(Duration::from_secs(2)) {
            return true;
        }
        if !port_open(&self.host, Duration::from_secs(1)) {
            let _ = self.spawn_serve();
        }
        self.wait_alive(timeout)
    }

    fn wait_alive(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.alive(Duration::from_secs(1)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        false
    }

    /// Kill the serving process. The server rufous started is killed by handle; any other
    /// by name (`taskkill /F /IM ollama.exe` on Windows, `pkill -f "ollama serve"`
    /// elsewhere, which matches the server only, never us), and its supervisor is then
    /// expected to relaunch it (see [`restart`](Self::restart)).
    pub fn kill(&self) {
        if self.owns_server() {
            if let Ok(mut g) = self.owned.lock() {
                if let Some(mut c) = g.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
            return;
        }
        let r = if cfg!(windows) {
            Command::new("taskkill")
                .args(["/F", "/IM", "ollama.exe"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        } else {
            Command::new("pkill")
                .args(["-f", "ollama serve"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        };
        let _ = r;
    }

    /// `restart_ollama`: kill → wait for the API to die (≤15 s) → for a server that was not
    /// ours, up to [`supervisor_grace`](Self::supervisor_grace) for its supervisor to bring
    /// it back → [`ensure`](Self::ensure), which starts one only on a free port. Returns
    /// whether the API answered afterwards.
    pub fn restart(&self, timeout: Duration) -> bool {
        self.restart_with(timeout, || self.kill())
    }

    /// [`restart`](Self::restart) with the kill step supplied (tests stop a fake server
    /// instead of signalling a real one).
    pub fn restart_with(&self, timeout: Duration, kill: impl FnOnce()) -> bool {
        let ours = self.owns_server();
        kill();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && self.alive(Duration::from_secs(1)) {
            std::thread::sleep(Duration::from_millis(500));
        }
        if ours {
            std::thread::sleep(Duration::from_secs(2));
        } else if self.wait_alive(self.supervisor_grace) {
            return true;
        }
        self.ensure(timeout)
    }
}
