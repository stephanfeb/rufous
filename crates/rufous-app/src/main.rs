//! Desktop shell: starts the local web UI server (`rufous-general`) on a free loopback port and
//! opens a native window on it. The page and its `/api/*` contract are the same ones
//! `rufous serve` exposes to a browser, so the shell adds only the window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::SocketAddr;
use std::sync::{mpsc, Arc};

use rufous_general::state::default_workdir;
use rufous_general::AppState;
use rufous_kernel::config::agent_home;
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Run the server on its own runtime thread; return the bound address once listening.
fn start_server() -> std::io::Result<SocketAddr> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rufous-general".into())
        .spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            rt.block_on(async move {
                let state = Arc::new(AppState::new(agent_home(), default_workdir()));
                match rufous_general::bind(SocketAddr::from(([127, 0, 0, 1], 0)), state).await {
                    Ok((local, server)) => {
                        let _ = tx.send(Ok(local));
                        if let Err(e) = server.await {
                            eprintln!("web server stopped: {e}");
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            });
        })?;
    rx.recv()
        .map_err(|_| std::io::Error::other("server thread exited before binding"))?
}

fn main() {
    let addr = match start_server() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rufous: cannot start the local server: {e}");
            std::process::exit(1);
        }
    };
    let url = format!("http://{addr}/");
    tauri::Builder::default()
        .setup(move |app| {
            let target = url.parse().map_err(|e| format!("bad url {url}: {e}"))?;
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(target))
                .title("rufous")
                .inner_size(1200.0, 800.0)
                .min_inner_size(800.0, 600.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running rufous");
}
