//! The `/api/*` contract against a real loopback server with a scripted agent runner:
//! status, prefs (with the `prefver` migration), sessions, plan, chat/busy, SSE events,
//! the approval round-trip (`ask` → `/api/ask`), stop, offline and cloud config writes.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boubou_core::safety::{ConfirmRequest, Confirmer};
use boubou_web::events::Bus;
use boubou_web::runner::{RunRequest, Spawner};
use boubou_web::{prefs, AppState};
use serde_json::{json, Value};

/// A runner that streams two tokens, asks for approval, records the verdict, then idles
/// until stopped (or, when `ask` is false, returns at once).
struct Scripted {
    ask: bool,
    idle: bool,
    seen: Mutex<Vec<String>>,
}

impl Spawner for Scripted {
    fn run(
        &self,
        req: RunRequest,
        stop: &Arc<AtomicBool>,
        bus: Arc<Bus>,
        mut confirmer: Box<dyn Confirmer>,
    ) {
        self.seen
            .lock()
            .expect("lock")
            .push(format!("run:{}:{}:{}", req.model, req.resume, req.session));
        bus.push("tok", json!("hel"), Default::default());
        bus.push("tok", json!("lo"), Default::default());
        if self.ask {
            let v = confirmer.confirm(&ConfirmRequest {
                tool: "read_file",
                target: "/etc/hosts".into(),
                action: "access",
                workdir: &req.workdir,
            });
            self.seen
                .lock()
                .expect("lock")
                .push(format!("verdict:{v:?}"));
        }
        if self.idle {
            let t0 = Instant::now();
            while !stop.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(20));
            }
            self.seen
                .lock()
                .expect("lock")
                .push(format!("stopped:{}", stop.load(Ordering::SeqCst)));
        }
    }
}

struct Server {
    addr: SocketAddr,
    home: tempfile::TempDir,
    _rt: tokio::runtime::Runtime,
}

fn serve(spawner: Box<dyn Spawner>) -> Server {
    let home = tempfile::tempdir().expect("tmp");
    let wd = home.path().join("work");
    std::fs::create_dir_all(&wd).expect("mkdir");
    // Unreachable Ollama so status is deterministic.
    std::fs::write(
        home.path().join("config.json"),
        r#"{"ollama_host": "http://127.0.0.1:9", "models": {"Gemma": "gemma4:e2b"}}"#,
    )
    .expect("cfg");
    let state = Arc::new(AppState::with_spawner(
        home.path().to_path_buf(),
        wd,
        spawner,
    ));
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let (addr, fut) = rt
        .block_on(boubou_web::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            state,
        ))
        .expect("bind");
    rt.spawn(fut);
    Server {
        addr,
        home,
        _rt: rt,
    }
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client")
}

impl Server {
    fn url(&self, p: &str) -> String {
        format!("http://{}{}", self.addr, p)
    }
    fn get(&self, p: &str) -> Value {
        http()
            .get(self.url(p))
            .send()
            .expect("get")
            .json()
            .expect("json")
    }
    fn post(&self, p: &str, body: Value) -> (u16, Value) {
        let r = http().post(self.url(p)).json(&body).send().expect("post");
        let code = r.status().as_u16();
        (code, r.json().unwrap_or(Value::Null))
    }
    /// Open the SSE stream and collect events until `pred` matches one (or timeout).
    fn sse_until(&self, pred: impl Fn(&Value) -> bool, timeout: Duration) -> Vec<Value> {
        let resp = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .expect("client")
            .get(self.url("/api/events"))
            .send()
            .expect("sse");
        assert!(resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.starts_with("text/event-stream")));
        let mut out = Vec::new();
        let rd = BufReader::new(resp);
        for line in rd.lines() {
            let Ok(line) = line else { break };
            if let Some(d) = line.strip_prefix("data: ") {
                if let Ok(v) = serde_json::from_str::<Value>(d) {
                    let hit = pred(&v);
                    out.push(v);
                    if hit {
                        break;
                    }
                }
            }
        }
        out
    }
}

fn scripted(ask: bool, idle: bool) -> (Arc<Scripted>, Box<dyn Spawner>) {
    let s = Arc::new(Scripted {
        ask,
        idle,
        seen: Mutex::new(Vec::new()),
    });
    struct Shared(Arc<Scripted>);
    impl Spawner for Shared {
        fn run(
            &self,
            req: RunRequest,
            stop: &Arc<AtomicBool>,
            bus: Arc<Bus>,
            confirmer: Box<dyn Confirmer>,
        ) {
            self.0.run(req, stop, bus, confirmer)
        }
    }
    (s.clone(), Box::new(Shared(s)))
}

fn wait_until(mut f: impl FnMut() -> bool, timeout: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    f()
}

#[test]
fn index_and_404() {
    let (_, sp) = scripted(false, false);
    let s = serve(sp);
    let r = http().get(s.url("/")).send().expect("get");
    assert!(r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/html")));
    let body = r.text().expect("text");
    assert!(body.contains("<title>Boubou</title>") && body.contains("/api/events"));
    assert!(!body.contains("Mingbird"));
    let r = http().get(s.url("/api/nope")).send().expect("get");
    assert_eq!(r.status().as_u16(), 404);
}

#[test]
fn status_shape_with_unreachable_ollama() {
    let (_, sp) = scripted(false, false);
    let s = serve(sp);
    let st = s.get("/api/status");
    assert_eq!(st["version"], json!(boubou_core::VERSION));
    assert_eq!(st["busy"], json!(false));
    assert_eq!(st["ollama_ok"], json!(false));
    assert_eq!(st["models"], json!([]));
    assert_eq!(st["session"], Value::Null);
    assert_eq!(st["offline"], json!(false));
    assert_eq!(st["ask_pending"], json!(false));
    assert_eq!(
        st["cloud"],
        json!({"enabled": false, "base_url": "", "model": "", "has_key": false})
    );
    assert!(st["workdir"].as_str().is_some_and(|w| w.ends_with("work")));
    let p = &st["prefs"];
    assert_eq!(p["ctx"], json!(131072));
    assert_eq!(p["temp"], Value::Null);
    assert_eq!(p["think"], Value::Null);
    assert_eq!(p["num_predict"], json!(2048));
    assert_eq!(p["ui_mode"], json!("auto"));
    assert_eq!(p.as_object().map(|m| m.len()), Some(8)); // + stt_lang (step 7)
}

#[test]
fn prefs_roundtrip_and_legacy_migration() {
    let (_, sp) = scripted(false, false);
    let s = serve(sp);
    let (code, r) = s.post(
        "/api/prefs",
        json!({"ctx": 32768, "temp": 0.2, "think": false, "bogus": 1}),
    );
    assert_eq!(code, 200);
    assert_eq!(r["prefs"]["ctx"], json!(32768));
    assert_eq!(r["prefs"]["prefver"], json!(2));
    assert!(r["prefs"].get("bogus").is_none());
    let st = s.get("/api/status");
    assert_eq!(st["prefs"]["temp"], json!(0.2));
    assert_eq!(st["prefs"]["think"], json!(false));
    let on_disk: Value = serde_json::from_str(
        &std::fs::read_to_string(s.home.path().join("gui_prefs.json")).expect("read"),
    )
    .expect("json");
    assert_eq!(on_disk["ctx"], json!(32768));

    // A pre-prefver file: temp 0 and think true were hidden defaults → unset.
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(
        d.path().join("gui_prefs.json"),
        r#"{"temp": 0.0, "think": true, "ctx": 8192}"#,
    )
    .expect("write");
    let p = prefs::load(d.path());
    assert_eq!(p["temp"], Value::Null);
    assert_eq!(p["think"], Value::Null);
    assert_eq!(p["ctx"], json!(8192));
    // With the marker, explicit 0 / false survive.
    std::fs::write(
        d.path().join("gui_prefs.json"),
        r#"{"temp": 0.0, "think": true, "prefver": 2}"#,
    )
    .expect("write");
    let p = prefs::load(d.path());
    assert_eq!(p["temp"], json!(0.0));
    assert_eq!(p["think"], json!(true));
}

#[test]
fn sessions_list_detail_select_and_newchat() {
    let (_, sp) = scripted(false, false);
    let s = serve(sp);
    let sd = s.home.path().join("sessions");
    std::fs::create_dir_all(&sd).expect("mkdir");
    std::fs::write(
        sd.join("a.json"),
        json!([
            {"role": "system", "content": "sys"},
            {"role": "user", "content": "⏱ Time budget: skip me"},
            {"role": "user", "content": "  write   a poem   about rust  "},
            {"role": "assistant", "content": "ok"}
        ])
        .to_string(),
    )
    .expect("w");
    std::fs::write(
        sd.join("a.meta.json"),
        r#"{"status": "done", "task": "poem"}"#,
    )
    .expect("w");
    std::fs::write(sd.join("a.todo.json"), "[]").expect("w");
    std::thread::sleep(Duration::from_millis(1100)); // distinct mtimes (1 s resolution)
    std::fs::write(
        sd.join("b.json"),
        json!([{"role": "user", "content": "second"}]).to_string(),
    )
    .expect("w");
    std::fs::write(sd.join("b.meta.json"), r#"{"title": " Titled "}"#).expect("w");
    let rows = s.get("/api/sessions")["sessions"].clone();
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["name"], json!("b")); // newest first
    assert_eq!(rows[0]["title"], json!("Titled"));
    assert_eq!(rows[1]["title"], json!("write a poem about rust"));
    assert_eq!(rows[1]["status"], json!("done"));
    assert_eq!(rows[1]["task"], json!("poem"));
    assert!(rows[1]["mtime"].as_u64().is_some());

    let d = s.get("/api/session?name=../a");
    assert_eq!(d["name"], json!("a"));
    assert_eq!(d["messages"].as_array().map(|m| m.len()), Some(4));
    assert_eq!(s.get("/api/session?name=zzz")["messages"], json!([]));

    let (code, r) = s.post("/api/session", json!({"name": "a"}));
    assert_eq!((code, r["session"].clone()), (200, json!("a")));
    assert_eq!(s.get("/api/status")["session"], json!("a"));
    assert_eq!(s.post("/api/session", json!({})).0, 400);
    s.post("/api/newchat", json!({}));
    assert_eq!(s.get("/api/status")["session"], Value::Null);
}

#[test]
fn plan_reads_workdir_todo() {
    let (_, sp) = scripted(false, false);
    let s = serve(sp);
    assert_eq!(s.get("/api/plan")["plan"], json!([]));
    std::fs::write(
        s.home.path().join("work").join("todo.json"),
        r#"[{"item": "a", "done": true}, {"item": "b", "done": false}]"#,
    )
    .expect("w");
    // The file belongs to the last run; it is nobody's plan until a session is current.
    assert_eq!(s.get("/api/plan")["plan"], json!([]));
    s.post("/api/session", json!({"name": "mysess"}));
    assert_eq!(s.get("/api/plan")["plan"][1]["item"], json!("b"));
    s.post("/api/newchat", json!({}));
    assert_eq!(s.get("/api/plan")["plan"], json!([]));
}

#[test]
fn new_chat_never_resumes_the_previous_conversation() {
    // 2026-10-28: New chat, then a message with the default `resume: true`, continued the
    // previous chat because the fresh session name had no file and the runner fell back to
    // the workdir checkpoint. A minted session now runs with resume off.
    let (sc, sp) = scripted(false, false);
    let s = serve(sp);
    std::fs::write(
        s.home.path().join("work").join(".agent_state.json"),
        json!([{"role": "user", "content": "old chat"}]).to_string(),
    )
    .expect("w");
    s.post("/api/newchat", json!({}));
    assert_eq!(
        s.post(
            "/api/chat",
            json!({"prompt": "hi", "model": "gemma4:e2b", "resume": true})
        )
        .0,
        200
    );
    assert!(wait_until(
        || s.get("/api/status")["busy"] == json!(false),
        Duration::from_secs(5)
    ));
    // The same session, now current, is continued by the next message.
    assert_eq!(
        s.post(
            "/api/chat",
            json!({"prompt": "more", "model": "gemma4:e2b"})
        )
        .0,
        200
    );
    assert!(wait_until(
        || s.get("/api/status")["busy"] == json!(false),
        Duration::from_secs(5)
    ));
    // A session picked in the rail resumes from its own file.
    s.post("/api/session", json!({"name": "old"}));
    assert_eq!(
        s.post(
            "/api/chat",
            json!({"prompt": "again", "model": "gemma4:e2b"})
        )
        .0,
        200
    );
    assert!(wait_until(
        || s.get("/api/status")["busy"] == json!(false),
        Duration::from_secs(5)
    ));
    let seen = sc.seen.lock().expect("lock").clone();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(
        seen[0].starts_with("run:gemma4:e2b:false:chat_"),
        "{seen:?}"
    );
    let minted = seen[0].rsplit(':').next().expect("session");
    assert_eq!(seen[1], format!("run:gemma4:e2b:true:{minted}"));
    assert_eq!(seen[2], "run:gemma4:e2b:true:old");
}

#[test]
fn chat_streams_events_asks_and_exits() {
    let (sc, sp) = scripted(true, false);
    let s = serve(sp);
    assert_eq!(
        s.post("/api/ask", json!({"answer": "allow", "id": "ask_1"}))
            .0,
        409
    );
    // Subscribe first so nothing is missed, then start.
    let addr = s.addr;
    let collector = std::thread::spawn(move || {
        let c = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client");
        let resp = c
            .get(format!("http://{addr}/api/events"))
            .send()
            .expect("sse");
        let mut out = Vec::new();
        for line in BufReader::new(resp).lines() {
            let Ok(line) = line else { break };
            if let Some(d) = line.strip_prefix("data: ") {
                let v: Value = serde_json::from_str(d).expect("json");
                let t = v["type"].clone();
                out.push(v);
                if t == json!("exit") {
                    break;
                }
            }
        }
        out
    });
    std::thread::sleep(Duration::from_millis(150));
    let (code, _) = s.post(
        "/api/chat",
        json!({"prompt": "hi", "model": "Gemma", "resume": false}),
    );
    assert_eq!(code, 200);
    // The runner is now blocked on the approval dialog.
    assert!(wait_until(
        || s.get("/api/status")["ask_pending"] == json!(true),
        Duration::from_secs(5)
    ));
    assert_eq!(s.get("/api/status")["busy"], json!(true));
    assert_eq!(
        s.post("/api/chat", json!({"prompt": "again"})).0,
        409,
        "busy"
    );
    // Wrong id is refused; the right one is accepted once.
    assert_eq!(
        s.post("/api/ask", json!({"answer": "allow", "id": "ask_999"}))
            .0,
        409
    );
    let st = s.get("/api/status");
    let pending = &st["ask_pending"];
    assert_eq!(pending, &json!(true));
    let events = s.sse_until(|_| true, Duration::from_millis(500)); // just proves retry header path works
    let _ = events;
    // Find the ask id from the collector later; answer via the id the server published.
    // We cannot read the collector yet, so derive it: ids are ask_1, ask_2, … per hub.
    assert_eq!(
        s.post("/api/ask", json!({"answer": "allow_all", "id": "ask_1"}))
            .0,
        200
    );
    assert_eq!(
        s.post("/api/ask", json!({"answer": "allow_all", "id": "ask_1"}))
            .0,
        409
    );
    let events = collector.join().expect("collector");
    let types: Vec<&str> = events.iter().filter_map(|e| e["type"].as_str()).collect();
    assert_eq!(types[0], "note", "{types:?}");
    assert!(events[0]["data"]
        .as_str()
        .is_some_and(|d| d.contains("start · gemma4:e2b")));
    assert_eq!(&types[1..], ["tok", "tok", "ask", "exit"], "{types:?}");
    assert_eq!(events[1]["data"], json!("hel"));
    let ask = &events[3];
    assert_eq!(ask["ask_id"], json!("ask_1"));
    assert_eq!(ask["data"]["tool"], json!("read_file"));
    assert_eq!(ask["data"]["path"], json!("/etc/hosts"));
    assert_eq!(ask["data"]["action"], json!("access"));
    assert!(events.iter().all(|e| e["id"].as_u64().is_some()));
    let seen = sc.seen.lock().expect("lock").clone();
    assert!(
        seen[0].starts_with("run:gemma4:e2b:false:chat_"),
        "{seen:?}"
    ); // display name mapped to tag, session auto-named
    assert_eq!(seen[1], "verdict:AllowAll");
    assert!(wait_until(
        || s.get("/api/status")["busy"] == json!(false),
        Duration::from_secs(5)
    ));
    let st = s.get("/api/status");
    assert_eq!(st["ask_pending"], json!(false));
    assert!(st["session"]
        .as_str()
        .is_some_and(|n| n.starts_with("chat_")));
}

#[test]
fn stop_raises_the_flag_and_resume_uses_selected_session() {
    let (sc, sp) = scripted(false, true);
    let s = serve(sp);
    s.post("/api/session", json!({"name": "mysess"}));
    assert_eq!(
        s.post("/api/chat", json!({"prompt": "go", "model": "unmapped:1b"}))
            .0,
        200
    );
    assert!(wait_until(
        || s.get("/api/status")["busy"] == json!(true),
        Duration::from_secs(5)
    ));
    assert_eq!(s.post("/api/stop", json!({})).0, 200);
    assert!(wait_until(
        || s.get("/api/status")["busy"] == json!(false),
        Duration::from_secs(5)
    ));
    let seen = sc.seen.lock().expect("lock").clone();
    assert_eq!(seen, vec!["run:unmapped:1b:true:mysess", "stopped:true"]);
    // After the run the session sticks (next chat appends to it).
    assert_eq!(s.get("/api/status")["session"], json!("mysess"));
}

#[test]
fn offline_and_cloud_write_config() {
    let (_, sp) = scripted(false, false);
    let s = serve(sp);
    let (code, r) = s.post("/api/offline", json!({"offline": true}));
    assert_eq!((code, r["offline"].clone()), (200, json!(true)));
    assert_eq!(s.get("/api/status")["offline"], json!(true));
    let (code, r) = s.post("/api/cloud", json!({"base_url": "https://api.example/v1/", "model": "m", "api_key": "sk-secret", "enabled": true}));
    assert_eq!(code, 200);
    assert_eq!(
        r["cloud"],
        json!({"enabled": true, "base_url": "https://api.example/v1", "model": "m", "has_key": true})
    );
    // Enabling cloud switched offline off; and offline now refuses while cloud is on.
    assert_eq!(s.get("/api/status")["offline"], json!(false));
    let (code, r) = s.post("/api/offline", json!({"offline": true}));
    assert_eq!(
        (code, r["error"].clone()),
        (409, json!("cloud_requires_network"))
    );
    // Blank key keeps the saved one; the key never comes back.
    let (_, r) = s.post("/api/cloud", json!({"api_key": "", "enabled": false}));
    assert_eq!(r["cloud"]["has_key"], json!(true));
    assert_eq!(r["cloud"]["enabled"], json!(false));
    let cfg: Value = serde_json::from_str(
        &std::fs::read_to_string(s.home.path().join("config.json")).expect("read"),
    )
    .expect("json");
    assert_eq!(cfg["cloud"]["api_key"], json!("sk-secret"));
    assert_eq!(
        cfg["ollama_host"],
        json!("http://127.0.0.1:9"),
        "other keys preserved"
    );
}

#[test]
fn finish_summary_is_surfaced_as_answer_text() {
    use boubou_core::agent::AgentEvent;
    use boubou_web::runner::publish;
    let bus = Bus::new();
    let mut rx = bus.subscribe();
    publish(
        &bus,
        AgentEvent::Finished {
            summary: " created hello.txt ".into(),
        },
    );
    publish(&bus, AgentEvent::Finished { summary: "".into() });
    publish(&bus, AgentEvent::Log("[ctx: 1/2 = 50%]".into()));
    publish(
        &bus,
        AgentEvent::ContextUsage {
            used: 300,
            limit: 1000,
        },
    );
    let mut got = Vec::new();
    while let Ok(v) = rx.try_recv() {
        got.push(v);
    }
    let types: Vec<&str> = got.iter().filter_map(|e| e["type"].as_str()).collect();
    assert_eq!(types, ["tok", "note", "note", "ctx"], "{got:?}");
    assert_eq!(got[0]["data"], json!("\n\ncreated hello.txt"));
    assert_eq!(
        got[3]["data"],
        json!({"used": 300, "total": 1000, "pct": 30})
    );
    assert_eq!(bus.last_id(), 4);
}

// ---------------- step 7: engine label and voice ----------------

#[test]
fn status_reports_engine_and_voice_state() {
    let (_s, sp) = scripted(false, false);
    let srv = serve(sp);
    let d = srv.get("/api/status");
    assert_eq!(d["engine"], "Ollama");
    let v = &d["voice"];
    assert!(v.get("local").is_some() && v.get("langs").is_some());
    assert_eq!(v["lang"], "en");
    assert_eq!(v["feature"], cfg!(feature = "voice"));
    // No model installed under the scratch home → a hint tells the user where it goes.
    if v["local"] == false {
        assert!(v["hint"].as_str().is_some_and(|h| h.contains("stt")));
    }
    // An OpenAI-compatible local engine changes the label; its model list is probed (and
    // the closed port makes it empty), never the Ollama tags.
    std::fs::write(
        srv.home.path().join("config.json"),
        r#"{"ollama_host": "http://127.0.0.1:9", "local_backend": {"type": "openai", "base_url": "http://127.0.0.1:9/v1"}}"#,
    )
    .expect("cfg");
    let d = srv.get("/api/status");
    assert_eq!(d["engine"], "OpenAI-compatible");
    assert_eq!(d["ollama_ok"], false);
    assert_eq!(d["models"], json!([]));
}

#[test]
fn voice_endpoint_status_codes() {
    let (_s, sp) = scripted(false, false);
    let srv = serve(sp);
    let post_wav = |bytes: Vec<u8>| -> (u16, Value) {
        let r = http()
            .post(srv.url("/api/voice?lang=en"))
            .header("Content-Type", "audio/wav")
            .body(bytes)
            .send()
            .expect("post");
        let code = r.status().as_u16();
        (code, r.json().unwrap_or(Value::Null))
    };
    let (code, body) = post_wav(b"this is not audio".to_vec());
    assert_eq!(code, 400, "{body}");
    assert_eq!(body["error"], "not_a_wav");
    let silent = boubou_voice::encode_wav16(&boubou_voice::Pcm {
        samples: vec![0.0; 8000],
        sample_rate: 16_000,
    });
    let (code, body) = post_wav(silent);
    assert_eq!(code, 422, "{body}");
    assert_eq!(body["error"], "no_voice");
    // Loud audio with no local model and an unreachable Ollama: no engine (503), unless a
    // model happens to be installed where the test binary lives.
    let loud = boubou_voice::encode_wav16(&boubou_voice::Pcm {
        samples: (0..16_000)
            .map(|i| 0.4 * ((i as f32) * 0.3).sin())
            .collect(),
        sample_rate: 16_000,
    });
    let (code, body) = post_wav(loud);
    // 503 without an engine; with a local model present (BOUBOU_STT_DIR) a sine tone decodes
    // to nothing (422) or to some text (200).
    assert!(matches!(code, 200 | 422 | 503), "{code} {body}");
    if code == 503 {
        assert!(body["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("no_stt_model") || e.starts_with("built_without")));
    }
}

#[test]
fn prefs_accept_stt_lang() {
    let (_s, sp) = scripted(false, false);
    let srv = serve(sp);
    let (code, d) = srv.post("/api/prefs", json!({"stt_lang": "en"}));
    assert_eq!(code, 200);
    assert_eq!(d["prefs"]["stt_lang"], "en");
    assert_eq!(srv.get("/api/status")["voice"]["lang"], "en");
}

#[test]
fn tool_results_and_compaction_reach_the_page() {
    use boubou_core::agent::AgentEvent;
    use boubou_web::runner::publish;
    let bus = Bus::new();
    let mut rx = bus.subscribe();
    publish(
        &bus,
        AgentEvent::ToolResult {
            name: "read_file".into(),
            args: json!({"path": "notes.md"}),
            output: "x".repeat(500),
            blocked: false,
        },
    );
    publish(
        &bus,
        AgentEvent::Compacted {
            level: 2,
            before: 40,
            after: 12,
        },
    );
    let mut got = Vec::new();
    while let Ok(v) = rx.try_recv() {
        got.push(v);
    }
    let types: Vec<&str> = got.iter().filter_map(|e| e["type"].as_str()).collect();
    assert_eq!(types, ["tool_done", "note"], "{got:?}");
    assert_eq!(got[0]["data"]["name"], json!("read_file"));
    assert_eq!(got[0]["data"]["args"]["path"], json!("notes.md"));
    assert_eq!(got[0]["data"]["blocked"], json!(false));
    assert!(got[0]["data"]["output"]
        .as_str()
        .is_some_and(|o| o.len() < 500));
    assert!(got[1]["data"]
        .as_str()
        .is_some_and(|n| n.contains("compacted") && n.contains("40 → 12")));
}
