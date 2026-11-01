//! The general-purpose harness over the small-model kernel. [`harness`] is what makes it
//! its own: the `general` turn policy, its task prompt and the skills built into it.
//! [`chat`] is the harness at a terminal (`rufous chat`). The rest is the local web UI
//! (`webui/server.py` in Mingbird): a loopback HTTP server that serves the single-page
//! frontend and the `/api/*` contract it speaks, over the in-process agent loop.
//!
//! Endpoints (unchanged from Mingbird so the page works as is):
//! `GET /`, `/api/status`, `/api/sessions`, `/api/session?name=`, `/api/plan`, `/api/events`
//! (SSE); `POST /api/chat`, `/api/stop`, `/api/ask`, `/api/newchat`, `/api/offline`,
//! `/api/cloud`, `/api/session`, `/api/prefs`.
//!
//! Mingbird spawned `ollama_agent.py` and re-published its stdout line protocol as SSE.
//! rufous runs [`rufous_kernel::agent::Agent`] on a thread and maps [`AgentEvent`]s to the same
//! SSE event types (`tok`, `think`, `ask`, `dispatch`, `ctx`, `note`, `exit`).

pub mod api;
pub mod chat;
pub mod events;
pub mod harness;
pub mod prefs;
pub mod runner;
pub mod state;

use std::net::SocketAddr;
use std::sync::Arc;

pub use rufous_kernel::agent::AgentEvent;
pub use state::AppState;

/// Bind `addr` (port 0 = any free port) and return the bound address plus the server future.
pub async fn bind(
    addr: SocketAddr,
    state: Arc<AppState>,
) -> std::io::Result<(
    SocketAddr,
    impl std::future::Future<Output = std::io::Result<()>>,
)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let app = api::router(state);
    Ok((local, async move { axum::serve(listener, app).await }))
}
