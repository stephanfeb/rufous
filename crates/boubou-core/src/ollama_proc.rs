//! The Ollama *process* (`_find_ollama_exe`, `ensure_ollama`, `restart_ollama`): start it
//! when the API is down, and restart it when a long run has degraded it (four consecutive
//! 500s at temperature 0 replay the same bad path; compaction and waiting cannot escape, a
//! restart does, and the checkpoint keeps the task).
//!
//! Everything here is synchronous and runtime-free on the outside: the HTTP liveness probe
//! runs on a scoped thread with its own runtime, so it is safe from inside the loop thread
//! (which owns a runtime of its own) and from a tokio worker alike.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

/// How to reach and (re)start this machine's Ollama. Built from the config; carried by the
/// Ollama backend so the loop can ask for a restart without knowing any of this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaProc {
    pub host: String,
    pub exe: String,
    pub env: BTreeMap<String, String>,
}

impl OllamaProc {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            host: cfg.ollama_host(),
            exe: cfg.ollama_exe.clone(),
            env: cfg.ollama_env.clone(),
        }
    }

    pub fn alive(&self, timeout: Duration) -> bool {
        api_alive(&self.host, timeout)
    }

    /// Spawn `ollama serve` detached, with the configured extra environment (GPU flags etc.).
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
        cmd.spawn().map(|_| ())
    }

    /// `ensure_ollama`: reachable → true at once; otherwise start `serve` and wait up to
    /// `timeout` for the API.
    pub fn ensure(&self, timeout: Duration) -> bool {
        if self.alive(Duration::from_secs(2)) {
            return true;
        }
        let _ = self.spawn_serve();
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

    /// Kill the serving process (and its runner children): `taskkill /F /IM ollama.exe`
    /// on Windows, `pkill -f "ollama serve"` elsewhere (matches the server only, never us).
    pub fn kill(&self) {
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

    /// `restart_ollama`: kill → wait for the API to die (≤15 s) → 2 s for the port →
    /// [`ensure`](Self::ensure). Returns whether the API answered afterwards.
    pub fn restart(&self, timeout: Duration) -> bool {
        self.kill();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && self.alive(Duration::from_secs(1)) {
            std::thread::sleep(Duration::from_millis(500));
        }
        std::thread::sleep(Duration::from_secs(2));
        self.ensure(timeout)
    }
}
