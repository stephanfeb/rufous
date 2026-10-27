//! Desktop shell. The embedded `webui/index.html` is Mingbird's web UI; it talks to the
//! `/api/*` surface listed in `docs/PORTING.md`, which this shell will expose as Tauri
//! commands (or a loopback server) once the agent loop is ported.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tauri::command]
fn version() -> String {
    boubou_core::VERSION.to_string()
}

#[tauri::command]
fn config() -> Result<boubou_core::config::Config, String> {
    boubou_core::config::Config::load().map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![version, config])
        .run(tauri::generate_context!())
        .expect("error while running Boubou");
}
