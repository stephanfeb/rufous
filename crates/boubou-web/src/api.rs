//! Route handlers. Response shapes are Mingbird's, field for field.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use boubou_core::agent::session::load_session_meta;
use boubou_core::ollama::Client;
use boubou_core::todo::todo_file;
use futures_util::stream::{self, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio_stream::wrappers::BroadcastStream;

use crate::prefs;
use crate::state::AppState;

/// The single-page frontend (Mingbird's page, rebranded).
pub const INDEX_HTML: &str = include_str!("../static/index.html");

type St = State<Arc<AppState>>;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/api/status", get(status))
        .route("/api/sessions", get(sessions))
        .route("/api/session", get(session_get).post(session_post))
        .route("/api/plan", get(plan))
        .route("/api/events", get(events))
        .route("/api/chat", post(chat))
        .route("/api/stop", post(stop))
        .route("/api/ask", post(ask))
        .route("/api/newchat", post(newchat))
        .route("/api/offline", post(offline))
        .route("/api/cloud", post(cloud))
        .route("/api/prefs", get(prefs_get).post(prefs_post))
        .fallback(not_found)
        .with_state(state)
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn ok(v: Value) -> Response {
    no_store(Json(v).into_response())
}

fn err(code: StatusCode, msg: &str) -> Response {
    no_store((code, Json(json!({"error": msg}))).into_response())
}

async fn index() -> Response {
    no_store(Html(INDEX_HTML).into_response())
}

async fn not_found() -> Response {
    no_store((StatusCode::NOT_FOUND, Json(json!({}))).into_response())
}

/// `_cloud_public`: the cloud section without the key, only whether one is saved.
fn cloud_public(cfg: &boubou_core::config::Config) -> Value {
    let c = &cfg.cloud;
    let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    json!({
        "enabled": c.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        "base_url": s("base_url"),
        "model": s("model"),
        "has_key": !s("api_key").is_empty(),
    })
}

async fn status(State(st): St) -> Response {
    let cfg = st.load_config();
    let p = prefs::load(&st.home);
    // Live model list from Ollama (3 s), deduped by digest; offline or unreachable → empty.
    let client = Client::new(cfg.ollama_host());
    let (models, ollama_ok) =
        match tokio::time::timeout(Duration::from_secs(3), client.list_models()).await {
            Ok(Ok(list)) => (
                cfg.dedupe_model_entries(&list)
                    .into_iter()
                    .map(|(d, _)| Value::String(d))
                    .collect::<Vec<_>>(),
                true,
            ),
            _ => (Vec::new(), false),
        };
    ok(json!({
        "version": boubou_core::VERSION,
        "busy": st.busy(),
        "models": models,
        "ollama_ok": ollama_ok,
        "session": st.session(),
        "workdir": st.workdir().to_string_lossy(),
        "offline": cfg.offline(),
        "cloud": cloud_public(&cfg),
        "ask_pending": st.ask.pending_id().is_some(),
        "prefs": prefs::public(&p),
    }))
}

/// `_first_user_text`: sidebar title from the first real user message.
fn first_user_text(msgs: &[Value], limit: usize) -> String {
    for m in msgs {
        if m.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let c = m
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if c.is_empty()
            || c.starts_with("Continue: keep making")
            || c.starts_with("⚠️ 交付前自查")
            || c.starts_with("[时间预算]")
        {
            continue;
        }
        let n = c.chars().count();
        return if n > limit {
            format!("{}…", c.chars().take(limit).collect::<String>())
        } else {
            c
        };
    }
    String::new()
}

/// `_session_rows`: newest first, 60 rows.
pub fn session_rows(st: &AppState, limit: usize) -> Vec<Value> {
    let sd = st.sessions_dir();
    let Ok(rd) = std::fs::read_dir(&sd) else {
        return Vec::new();
    };
    let mut files: Vec<(std::path::PathBuf, u64)> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.ends_with(".json") && !n.ends_with(".meta.json") && !n.ends_with(".todo.json")
        })
        .map(|p| {
            let mtime = p
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (p, mtime)
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    files
        .into_iter()
        .take(limit)
        .map(|(p, mtime)| {
            let name = p
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let meta = load_session_meta(&sd, &name);
            let s = |k: &str| {
                meta.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let mut title = s("title").trim().to_string();
            if title.is_empty() {
                let msgs: Vec<Value> = std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                title = first_user_text(&msgs, 60);
            }
            json!({
                "name": name,
                "title": if title.is_empty() { name.clone() } else { title },
                "mtime": mtime,
                "status": s("status"),
                "task": s("task").chars().take(80).collect::<String>(),
            })
        })
        .collect()
}

async fn sessions(State(st): St) -> Response {
    ok(json!({"sessions": session_rows(&st, 60)}))
}

#[derive(Deserialize)]
struct NameQuery {
    #[serde(default)]
    name: String,
}

fn basename(s: &str) -> String {
    s.rsplit(['/', '\\']).next().unwrap_or("").to_string()
}

async fn session_get(State(st): St, Query(q): Query<NameQuery>) -> Response {
    let safe = basename(&q.name);
    let fp = st.sessions_dir().join(format!("{safe}.json"));
    let msgs = std::fs::read_to_string(fp)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]));
    ok(json!({"name": safe, "messages": msgs}))
}

async fn session_post(State(st): St, body: Option<Json<Value>>) -> Response {
    let name = body
        .as_ref()
        .and_then(|b| b.get("name"))
        .and_then(Value::as_str)
        .map(basename)
        .unwrap_or_default();
    if name.is_empty() {
        return err(StatusCode::BAD_REQUEST, "name required");
    }
    st.set_session(Some(name.clone()));
    ok(json!({"session": name}))
}

async fn plan(State(st): St) -> Response {
    let p = todo_file(&st.workdir());
    let plan = std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]));
    ok(json!({"plan": plan}))
}

async fn events(State(st): St) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = st.bus.subscribe();
    let head = stream::once(async { Ok(Event::default().retry(Duration::from_millis(3000))) });
    let body = BroadcastStream::new(rx).filter_map(|r| async move {
        // A lagging client simply skips ahead (Mingbird's ring buffer did the same).
        r.ok().map(|v| Ok(Event::default().data(v.to_string())))
    });
    Sse::new(head.chain(body)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

async fn chat(State(st): St, body: Option<Json<Value>>) -> Response {
    let b = body.map(|j| j.0).unwrap_or(Value::Null);
    let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let resume = b.get("resume").and_then(Value::as_bool).unwrap_or(true);
    if st.busy() {
        return err(StatusCode::CONFLICT, "busy");
    }
    if !st.start(s("prompt"), s("model"), resume) {
        return err(StatusCode::CONFLICT, "busy");
    }
    ok(json!({}))
}

async fn stop(State(st): St) -> Response {
    st.stop();
    ok(json!({}))
}

async fn ask(State(st): St, body: Option<Json<Value>>) -> Response {
    let b = body.map(|j| j.0).unwrap_or(Value::Null);
    let id = b.get("id").and_then(Value::as_str).unwrap_or("");
    let answer = b.get("answer").and_then(Value::as_str).unwrap_or("deny");
    if id.is_empty() || !st.ask.answer(id, answer) {
        return err(StatusCode::CONFLICT, "no pending ask");
    }
    ok(json!({}))
}

async fn newchat(State(st): St) -> Response {
    st.set_session(None);
    ok(json!({}))
}

async fn offline(State(st): St, body: Option<Json<Value>>) -> Response {
    let wants_off = body
        .as_ref()
        .and_then(|b| b.get("offline"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut cfg = st.load_config();
    let cl = &cfg.cloud;
    let truthy = |k: &str| match cl.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    };
    if wants_off && truthy("enabled") && truthy("base_url") && truthy("model") {
        return err(StatusCode::CONFLICT, "cloud_requires_network");
    }
    cfg.offline_mode = wants_off;
    if let Err(e) = cfg.save_to(&st.config_path()) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    ok(json!({"offline": cfg.offline()}))
}

async fn cloud(State(st): St, body: Option<Json<Value>>) -> Response {
    let b = body.map(|j| j.0).unwrap_or(Value::Null);
    let mut cfg = st.load_config();
    let mut c = cfg.cloud.clone();
    for k in ["base_url", "model", "api_key"] {
        if let Some(v) = b.get(k) {
            let v = v.as_str().unwrap_or("").trim().to_string();
            if k == "api_key" && v.is_empty() {
                continue; // blank = keep the saved key
            }
            let v = if k == "base_url" {
                v.trim_end_matches('/').to_string()
            } else {
                v
            };
            c.insert(k.to_string(), Value::String(v));
        }
    }
    if let Some(en) = b.get("enabled") {
        c.insert("enabled".into(), Value::Bool(en.as_bool().unwrap_or(false)));
    }
    let nonempty = |k: &str| {
        c.get(k)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    };
    let enabled = c.get("enabled").and_then(Value::as_bool).unwrap_or(false)
        && nonempty("base_url")
        && nonempty("model");
    c.insert("enabled".into(), Value::Bool(enabled));
    if enabled {
        cfg.offline_mode = false;
    }
    cfg.cloud = c;
    if let Err(e) = cfg.save_to(&st.config_path()) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    ok(json!({"cloud": cloud_public(&cfg)}))
}

async fn prefs_get(State(st): St) -> Response {
    ok(json!({"prefs": prefs::load(&st.home)}))
}

async fn prefs_post(State(st): St, body: Option<Json<Value>>) -> Response {
    let b = body.map(|j| j.0).unwrap_or(Value::Null);
    let mut p = prefs::load(&st.home);
    for k in prefs::KEYS {
        if let Some(v) = b.get(k) {
            p.insert(k.to_string(), v.clone());
        }
    }
    if let Err(e) = prefs::save(&st.home, &p) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    let _: Map<String, Value> = Map::new();
    ok(json!({"prefs": p}))
}
