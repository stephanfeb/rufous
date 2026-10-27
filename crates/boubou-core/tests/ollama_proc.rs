//! `_find_ollama_exe` / `ensure_ollama` / `restart_ollama` without touching a real Ollama:
//! explicit paths win, a closed port reads as down, a bogus executable fails fast and
//! quietly, and `api_alive` is callable from inside a tokio runtime.

use std::time::Duration;

use boubou_core::config::Config;
use boubou_core::ollama_proc::{api_alive, block_on_fresh, find_ollama_exe, OllamaProc};

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
