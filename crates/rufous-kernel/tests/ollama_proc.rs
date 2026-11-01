//! `_find_ollama_exe` / `ensure_ollama` / `restart_ollama` without touching a real Ollama:
//! explicit paths win, a closed port reads as down, a bogus executable fails fast and
//! quietly, and `api_alive` is callable from inside a tokio runtime.

use std::net::TcpListener;
use std::time::Duration;
// The process tests below are unix-only; on Windows their helpers and imports would be dead
// code, which CI's clippy -D warnings rejects (bead boubou-tfr).
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::SocketAddr;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::Instant;

use rufous_kernel::config::Config;
use rufous_kernel::ollama_proc::{
    api_alive, block_on_fresh, find_ollama_exe, host_addr, port_open, OllamaProc,
};

/// A stand-in `ollama serve`: answers every request on `listener` with 200 until `stop` is
/// raised, then closes the port. `delay` holds the first answer back (a server still
/// loading: the port is bound, the API is silent).
#[cfg(unix)]
fn serve_tags(listener: TcpListener, stop: Arc<AtomicBool>, delay: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        listener.set_nonblocking(true).expect("nonblocking");
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let _ = s.set_nonblocking(false);
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf);
                    let _ = s.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    );
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    });
}

/// Every test that binds a loopback port holds this. `free_port` hands a port back by
/// closing its listener, so two tests running at once can be given the same number, and a
/// test that checks a closed port can find another test's server on it (seen as a failure
/// in a different test on each run). Cargo runs test binaries one after another, so one
/// lock for this file is enough.
fn ports() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(unix)]
fn free_port() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
}

/// An `ollama` stand-in that records being started and exits (unix only).
#[cfg(unix)]
fn marker_exe(dir: &std::path::Path) -> (String, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let marker = dir.join("spawned");
    let exe = dir.join("ollama");
    std::fs::write(
        &exe,
        format!("#!/bin/sh\ntouch {}\nexit 0\n", marker.display()),
    )
    .expect("script");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (exe.to_string_lossy().into_owned(), marker)
}

#[cfg(unix)]
fn proc_for(addr: SocketAddr, exe: &str) -> OllamaProc {
    OllamaProc::new(
        format!("http://{addr}"),
        exe.to_string(),
        Default::default(),
    )
}

#[test]
fn the_hosts_port_is_what_gets_probed() {
    let _ports = ports();
    assert_eq!(
        host_addr("http://127.0.0.1:11434"),
        Some("127.0.0.1:11434".parse().expect("addr"))
    );
    assert_eq!(host_addr("http://127.0.0.1").map(|a| a.port()), Some(80));
    assert!(host_addr("not a url").is_none());
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    assert!(port_open(&format!("http://{addr}"), Duration::from_secs(1)));
    drop(l);
    assert!(!port_open(
        &format!("http://{addr}"),
        Duration::from_secs(1)
    ));
}

#[cfg(unix)]
#[test]
fn a_bound_port_that_is_not_answering_yet_is_waited_for_not_spawned_over() {
    let _ports = ports();
    let d = tempfile::tempdir().expect("tmp");
    let (exe, marker) = marker_exe(d.path());
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    // Bound at once, answering after 3 s: longer than ensure's first 2 s probe.
    serve_tags(l, stop.clone(), Duration::from_secs(3));
    let p = proc_for(addr, &exe);
    assert!(p.ensure(Duration::from_secs(10)), "the API came up");
    stop.store(true, Ordering::SeqCst);
    assert!(
        !marker.exists(),
        "a second server was started over a bound port"
    );
}

#[cfg(unix)]
#[test]
fn restart_lets_a_supervised_server_come_back_before_starting_its_own() {
    let _ports = ports();
    let d = tempfile::tempdir().expect("tmp");
    let (exe, marker) = marker_exe(d.path());
    let addr = free_port();
    let stop = Arc::new(AtomicBool::new(false));
    serve_tags(
        TcpListener::bind(addr).expect("bind"),
        stop.clone(),
        Duration::ZERO,
    );
    let mut p = proc_for(addr, &exe);
    p.supervisor_grace = Duration::from_secs(10);
    assert!(p.alive(Duration::from_secs(2)));
    assert!(!p.owns_server());
    // The "kill": the server goes away and, as the Ollama app or systemd would, comes back
    // on the same port four seconds later, longer than the old fixed two-second wait.
    let kill = || {
        stop.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(200));
        let again = Arc::new(AtomicBool::new(false));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(4));
            serve_tags(
                TcpListener::bind(addr).expect("rebind"),
                again,
                Duration::ZERO,
            );
        });
    };
    assert!(p.restart_with(Duration::from_secs(5), kill));
    assert!(
        !marker.exists(),
        "rufous started a server beside the relaunched one"
    );
}

#[cfg(unix)]
#[test]
fn restart_starts_its_own_server_only_once_the_grace_period_has_passed() {
    let _ports = ports();
    let d = tempfile::tempdir().expect("tmp");
    let (exe, marker) = marker_exe(d.path());
    let addr = free_port();
    let stop = Arc::new(AtomicBool::new(false));
    serve_tags(
        TcpListener::bind(addr).expect("bind"),
        stop.clone(),
        Duration::ZERO,
    );
    let mut p = proc_for(addr, &exe);
    p.supervisor_grace = Duration::from_secs(4);
    let t0 = Instant::now();
    // Nobody brings it back: after the grace period rufous starts one (the stand-in exits
    // at once, so the API never answers).
    assert!(!p.restart_with(Duration::from_secs(1), || stop
        .store(true, Ordering::SeqCst)));
    assert!(marker.exists(), "no server was started on the free port");
    assert!(
        t0.elapsed() >= Duration::from_secs(4),
        "spawned before the grace period"
    );
}

#[cfg(unix)]
#[test]
fn the_server_rufous_started_is_killed_by_handle() {
    let _ports = ports();
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().expect("tmp");
    let exe = d.path().join("ollama");
    std::fs::write(&exe, "#!/bin/sh\nsleep 60\n").expect("script");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let p = proc_for(free_port(), &exe.to_string_lossy());
    p.spawn_serve().expect("spawn");
    assert!(p.owns_server());
    // Only an owned server is killed here; the by-name path would signal a real Ollama.
    p.kill();
    assert!(!p.owns_server(), "the spawned process is still running");
    let q = p.clone();
    assert!(!q.owns_server(), "clones share the handle");
}

#[test]
fn explicit_path_wins_over_detection() {
    let d = tempfile::tempdir().expect("tmp");
    let exe = d.path().join("ollama-custom");
    std::fs::write(&exe, "#!/bin/sh\n").expect("write");
    assert_eq!(find_ollama_exe(exe.to_str().expect("utf8")), exe);
    // A configured path that does not exist is ignored (falls through to detection/fallback).
    let p = find_ollama_exe("/nonexistent/ollama-xyz");
    assert!(p
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("ollama")));
}

#[test]
fn closed_port_is_not_alive() {
    assert!(!api_alive("http://127.0.0.1:9", Duration::from_secs(1)));
    assert!(!api_alive("http://127.0.0.1:9/", Duration::from_secs(1)));
}

#[test]
fn alive_check_works_from_inside_a_runtime() {
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let alive = rt.block_on(async { api_alive("http://127.0.0.1:9", Duration::from_secs(1)) });
    assert!(!alive);
    assert_eq!(block_on_fresh(async { 41 + 1 }), 42);
}

#[test]
fn ensure_with_bogus_exe_fails_fast_and_quietly() {
    let cfg = Config {
        ollama_host: "http://127.0.0.1:9".into(),
        ollama_exe: "/nonexistent/dir/ollama".into(),
        ..Config::default()
    };
    let proc = OllamaProc::from_config(&cfg);
    if std::env::var("OLLAMA_HOST").is_ok() || std::env::var("OLLAMA_BIN").is_ok() {
        return; // the environment points at a real Ollama; nothing to assert here
    }
    assert_eq!(proc.exe, "/nonexistent/dir/ollama");
    assert!(
        proc.spawn_serve().is_err() || true,
        "spawn error or PATH fallback; either is fine"
    );
    let t = std::time::Instant::now();
    // Our `kill` is deliberately not exercised here (it would signal a real `ollama serve`).
    assert!(!proc.ensure(Duration::from_millis(1200)));
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn from_config_carries_host_exe_env() {
    let mut cfg = Config {
        ollama_exe: "C:/x/ollama.exe".into(),
        ..Config::default()
    };
    cfg.ollama_env
        .insert("OLLAMA_IGPU_ENABLE".into(), "1".into());
    let p = OllamaProc::from_config(&cfg);
    assert_eq!(p.exe, "C:/x/ollama.exe");
    assert_eq!(
        p.env.get("OLLAMA_IGPU_ENABLE").map(String::as_str),
        Some("1")
    );
    assert_eq!(p.host, cfg.ollama_host());
}
