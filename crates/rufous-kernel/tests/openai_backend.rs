//! Port of `tests/test_v211_backends.py` (OpenAI-compatible local engines) plus the cloud
//! cases from `test_offline_mode.py` / `test_v203_fixes.py`: engine normalisation, inline
//! `<think>` splitting (whole and streamed, torn tags), message normalisation, payload shape,
//! SSE assembly, engine dispatch, and the Ollama-only gates (restart, think probe).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;

use rufous_kernel::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use rufous_kernel::agent::openai::{
    normalize_message, parse_completion, split_inline_thinking, tc_args_from_str,
    to_openai_messages, Flavor, InlineThinkSplitter, OpenAiBackend, SseAssembler,
};
use rufous_kernel::agent::run::model_id_warning;
use rufous_kernel::agent::{Agent, AgentEvent, RunParams, ToolRunner};
use rufous_kernel::config::{
    BackendKind, CloudProvider, Config, LocalBackend, OpenAiEngine, LMSTUDIO_BASE_URL,
};
use rufous_kernel::ollama::{Message, Role};
use serde_json::{json, Value};

fn cfg_with(local_backend: Value, cloud: Value) -> Config {
    Config {
        local_backend: local_backend.as_object().cloned().unwrap_or_default(),
        cloud: cloud.as_object().cloned().unwrap_or_default(),
        ..Config::default()
    }
}

// ---------------- appconfig.local_backend ----------------

// Named backend (bead boubou-56g): --backend over config `backend` over the legacy
// `local_backend.type`, over an enabled cloud; each source is named for the log.

#[test]
fn a_named_backend_resolves_in_order_and_says_where_it_came_from() {
    let mut c = cfg_with(
        json!({"type": "openai", "base_url": "http://127.0.0.1:9931/v1"}),
        json!({}),
    );
    assert_eq!(c.backend_source(), "config.json local_backend");
    assert_eq!(c.engine_display_name(), "OpenAI-compatible");
    c.backend = Some(BackendKind::Ollama);
    assert_eq!(c.local_backend(), LocalBackend::Ollama);
    assert_eq!(c.backend_source(), "config.json backend");
    c.backend_flag = Some(BackendKind::Lmstudio);
    let e = c.backend_openai().expect("lm studio");
    assert_eq!(
        (e.label, e.base_url.as_str()),
        ("LM Studio", "http://127.0.0.1:9931/v1")
    );
    assert_eq!(c.backend_source(), "--backend");
    assert_eq!(Config::default().backend_source(), "default");
}

#[test]
fn lmstudio_defaults_its_url_and_openai_without_one_is_a_problem() {
    let mut c = Config {
        backend: Some(BackendKind::Lmstudio),
        ..Config::default()
    };
    assert_eq!(
        c.backend_openai().map(|e| e.base_url),
        Some(LMSTUDIO_BASE_URL.to_string())
    );
    assert!(c.backend_problem().is_none());
    c.backend = Some(BackendKind::Openai);
    assert_eq!(
        c.backend_problem().as_deref(),
        Some("backend \"openai\" (from config.json backend) needs local_backend.base_url, the server's /v1 URL")
    );
    c.local_backend = json!({"base_url": "http://box:8080/v1/"})
        .as_object()
        .cloned()
        .expect("object");
    assert!(c.backend_problem().is_none());
    let e = c.backend_openai().expect("openai");
    assert_eq!(
        (e.label, e.base_url.as_str()),
        ("OpenAI-compatible", "http://box:8080/v1")
    );
}

#[test]
fn the_backend_key_reads_from_json_and_the_flag_is_never_written() {
    let (c, dropped) = Config::from_json(r#"{"backend": "lmstudio"}"#).expect("json");
    assert!(dropped.is_empty());
    assert_eq!(c.backend, Some(BackendKind::Lmstudio));
    let (c, dropped) = Config::from_json(r#"{"backend": "llamafile"}"#).expect("json");
    assert_eq!(
        (c.backend, dropped.len()),
        (None, 1),
        "an unknown name is dropped with a warning"
    );
    let c = Config {
        backend_flag: Some(BackendKind::Lmstudio),
        ..Config::default()
    };
    let text = serde_json::to_string(&c).expect("ser");
    assert!(
        !text.contains("\"backend\"") && !text.contains("backend_flag"),
        "{text}"
    );
}

#[test]
fn a_named_backend_wins_over_an_enabled_cloud() {
    let mut c = cfg_with(
        json!({}),
        json!({"enabled": true, "base_url": "https://example.invalid/v1", "model": "m"}),
    );
    assert!(c.cloud_provider().is_some());
    assert_eq!(c.backend_source(), "config.json cloud");
    c.backend_flag = Some(BackendKind::Ollama);
    assert!(c.cloud_provider().is_none());
}

#[test]
fn model_ids_written_for_the_other_engine_are_flagged() {
    assert_eq!(
        model_id_warning("gemma-4-12b", true).as_deref(),
        Some("gemma-4-12b looks like an LM Studio id; Ollama names it gemma4:12b")
    );
    assert!(model_id_warning("google/gemma-4-12b", true).is_some());
    assert_eq!(model_id_warning("gemma4:12b", true), None);
    assert_eq!(model_id_warning("llama3", true), None);
    assert_eq!(
        model_id_warning("hf.co/bartowski/Qwen2.5-7B-Instruct-GGUF:Q4_K_M", true),
        None
    );
    assert!(model_id_warning("gemma4:12b", false)
        .is_some_and(|w| w.starts_with("gemma4:12b looks like an Ollama tag")));
    assert_eq!(model_id_warning("google/gemma-4-12b", false), None);
}

#[test]
fn a_failed_request_names_the_model_server() {
    let e = rufous_kernel::Error::Ollama(
        "LM Studio · http://127.0.0.1:1234/v1: error sending request".into(),
    );
    assert_eq!(
        e.to_string(),
        "model server: LM Studio · http://127.0.0.1:1234/v1: error sending request"
    );
}

#[test]
fn local_backend_default_is_ollama() {
    let c = Config::default();
    assert_eq!(c.local_backend(), LocalBackend::Ollama);
    assert!(c.backend_openai().is_none());
    assert_eq!(c.engine_display_name(), "Ollama");
}

#[test]
fn local_backend_openai_normalizes_trailing_slash() {
    let c = cfg_with(
        json!({"type": "openai", "base_url": "http://127.0.0.1:9931/v1/", "api_key": "sk-x"}),
        json!({}),
    );
    let e = c.backend_openai().expect("openai");
    assert_eq!(e.base_url, "http://127.0.0.1:9931/v1");
    assert_eq!(e.api_key, "sk-x");
    assert_eq!(c.engine_display_name(), "OpenAI-compatible");
}

#[test]
fn local_backend_openai_without_base_url_falls_back() {
    let c = cfg_with(json!({"type": "openai", "base_url": "   "}), json!({}));
    assert_eq!(c.local_backend(), LocalBackend::Ollama);
    let c = cfg_with(json!("garbage"), json!({}));
    assert_eq!(c.local_backend(), LocalBackend::Ollama);
}

#[test]
fn cloud_requires_full_config_and_is_off_when_offline() {
    let c = cfg_with(json!({}), json!({"enabled": true, "model": "m"}));
    assert!(c.cloud_provider().is_none()); // no base_url
    let mut c = cfg_with(
        json!({}),
        json!({"enabled": true, "base_url": "https://example.invalid/v1/", "model": "m", "api_key": "k"}),
    );
    let cp = c.cloud_provider().expect("cloud");
    assert_eq!(cp.base_url, "https://example.invalid/v1");
    assert_eq!(cp.model, "m");
    c.offline_mode = true;
    if std::env::var("AGENT_OFFLINE").is_err() {
        assert!(c.cloud_provider().is_none(), "offline = cloud forced off");
    }
}

// ---------------- inline <think>: whole ----------------

#[test]
fn split_inline_thinking_cases() {
    assert_eq!(
        split_inline_thinking("<think>reasoning</think>\n\nbody"),
        ("body".into(), "reasoning".into())
    );
    assert_eq!(
        split_inline_thinking("<think>a</think>x<think>b</think>y"),
        ("xy".into(), "ab".into())
    );
    assert_eq!(
        split_inline_thinking("no tags"),
        ("no tags".into(), String::new())
    );
    assert_eq!(split_inline_thinking(""), (String::new(), String::new()));
    let (body, think) = split_inline_thinking("<think>cut off before it finished");
    assert_eq!(body, "");
    assert!(think.contains("cut off"));
}

// ---------------- inline <think>: streaming state machine ----------------

#[test]
fn splitter_basic_routing() {
    let mut sp = InlineThinkSplitter::new();
    assert_eq!(
        sp.feed("<think>reasoning"),
        (String::new(), "reasoning".into())
    );
    assert_eq!(
        sp.feed(" more</think>body arrives"),
        ("body arrives".into(), " more".into())
    );
    assert_eq!(sp.finish(), (String::new(), String::new()));
}

#[test]
fn splitter_torn_tag_across_chunks() {
    let mut sp = InlineThinkSplitter::new();
    assert_eq!(
        sp.feed("first half<th"),
        ("first half".into(), String::new())
    );
    assert_eq!(
        sp.feed("ink>thought</thin"),
        (String::new(), "thought".into())
    );
    assert_eq!(
        sp.feed("k>second half"),
        ("second half".into(), String::new())
    );
}

#[test]
fn splitter_lone_lt_is_body_and_unclosed_at_finish() {
    let mut sp = InlineThinkSplitter::new();
    assert_eq!(
        sp.feed("a < b and 3<5"),
        ("a < b and 3<5".into(), String::new())
    );
    assert_eq!(sp.finish(), (String::new(), String::new()));
    let mut sp = InlineThinkSplitter::new();
    assert_eq!(
        sp.feed("start<think>thought not"),
        ("start".into(), "thought not".into())
    );
    assert_eq!(sp.finish(), (String::new(), String::new()));
    // A held tail that never became a tag is returned as body at the end.
    let mut sp = InlineThinkSplitter::new();
    assert_eq!(sp.feed("x<t"), ("x".into(), String::new()));
    assert_eq!(sp.finish(), ("<t".into(), String::new()));
}

// ---------------- message normalisation ----------------

#[test]
fn normalize_merges_reasoning_and_inline_think() {
    let m = normalize_message(&json!({
        "role": "assistant", "content": "<think>inline</think>answer", "reasoning_content": "field"
    }));
    assert_eq!(m.content, "answer");
    assert_eq!(m.thinking.as_deref(), Some("fieldinline"));
}

#[test]
fn normalize_tool_args_str_dict_and_broken() {
    let m = normalize_message(&json!({"role": "assistant", "content": "", "tool_calls": [
        {"id": "c1", "function": {"name": "run_bash", "arguments": "{\"cmd\": \"ls\"}"}},
        {"id": "c2", "function": {"name": "read_file", "arguments": {"path": "x"}}},
        {"id": "c3", "function": {"name": "t", "arguments": "{bad JSON"}}]}));
    assert_eq!(m.tool_calls[0].function.arguments, json!({"cmd": "ls"}));
    assert_eq!(m.tool_calls[1].function.arguments, json!({"path": "x"}));
    assert_eq!(m.tool_calls[2].function.arguments["_raw"], "{bad JSON");
    assert_eq!(tc_args_from_str("  "), json!({}));
}

#[test]
fn cloud_message_mapping_pairs_tool_results() {
    let msgs = vec![
        Message::system("s"),
        Message::user("u"),
        {
            let mut m =
                Message::assistant_call("create_file", json!({"path": "a", "content": "x"}));
            m.tool_calls.push(rufous_kernel::ollama::ToolCall {
                function: rufous_kernel::ollama::FunctionCall {
                    name: "read_file".into(),
                    arguments: json!({"path": "b"}),
                },
            });
            m
        },
        Message::tool_output("ok1"),
        Message::tool_output("ok2"),
        Message::assistant("done"),
    ];
    let out = to_openai_messages(&msgs);
    assert_eq!(out[0], json!({"role": "system", "content": "s"}));
    let a = &out[2];
    assert_eq!(a["role"], "assistant");
    assert_eq!(a["content"], Value::Null);
    let tcs = a["tool_calls"].as_array().expect("tool_calls");
    assert_eq!(tcs.len(), 2);
    let args: Value =
        serde_json::from_str(tcs[0]["function"]["arguments"].as_str().expect("str")).expect("json");
    assert_eq!(args["path"], "a");
    assert_eq!(out[3]["role"], "tool");
    assert_eq!(out[3]["tool_call_id"], tcs[0]["id"]);
    assert_eq!(out[4]["tool_call_id"], tcs[1]["id"]);
    assert_eq!(out[5], json!({"role": "assistant", "content": "done"}));
}

// ---------------- payload shape (pure) ----------------

fn params(dir: &Path, num_predict: i32) -> RunParams {
    let mut p = RunParams::plain("qwen", dir);
    p.num_predict = num_predict;
    p.sleep_scale = 0.0;
    p
}

fn local(dir: &Path, key: &str, num_predict: i32) -> OpenAiBackend {
    OpenAiBackend::local(
        &OpenAiEngine {
            base_url: "http://127.0.0.1:1234/v1".into(),
            api_key: key.into(),
            label: "LM Studio",
        },
        &params(dir, num_predict),
    )
    .expect("backend")
}

#[test]
fn local_payload_shape() {
    let d = tempfile::tempdir().expect("tmp");
    let b = local(d.path(), "", 1024);
    let shape = b.shape("qwen");
    let body = shape.body(&[Message::user("hi")], &[]);
    assert_eq!(body["model"], "qwen");
    assert_eq!(body["max_tokens"], 1024);
    assert_eq!(body["stream"], true);
    for absent in [
        "keep_alive",
        "options",
        "num_ctx",
        "think",
        "enable_thinking",
        "tools",
    ] {
        assert!(body.get(absent).is_none(), "{absent}");
    }
    assert!(
        b.auth_header().is_none(),
        "empty key: no Bearer (strict servers 401)"
    );
    assert!(local(d.path(), "", -1)
        .shape("m")
        .body(&[], &[])
        .get("max_tokens")
        .is_none());
    assert!(local(d.path(), "", 0)
        .shape("m")
        .body(&[], &[])
        .get("max_tokens")
        .is_none());
    assert_eq!(
        local(d.path(), "sk-local", 0).auth_header().as_deref(),
        Some("Bearer sk-local")
    );
    let tools = vec![
        json!({"type": "function", "function": {"name": "run_bash", "description": "d",
        "parameters": {"type": "object", "properties": {}}}}),
    ];
    let body = b.shape("m").body(&[], &tools);
    assert_eq!(body["tools"], Value::Array(tools));
    assert_eq!(body["tool_choice"], "auto");
}

#[test]
fn cloud_payload_shape() {
    let d = tempfile::tempdir().expect("tmp");
    let cp = CloudProvider {
        base_url: "https://x/v1".into(),
        api_key: "k".into(),
        model: "cloud-m".into(),
    };
    let b = OpenAiBackend::cloud(&cp, &params(d.path(), -1)).expect("backend");
    assert_eq!(b.flavor, Flavor::Cloud);
    let body = b
        .shape("whatever-the-loop-asked")
        .body(&[Message::user("hi")], &[]);
    assert_eq!(body["model"], "cloud-m", "the configured model wins");
    assert_eq!(body["stream"], false);
    assert_eq!(body["enable_thinking"], false);
    assert!(
        body.get("max_tokens").is_none(),
        "-1 = omit, no negative semantics"
    );
    assert_eq!(b.auth_header().as_deref(), Some("Bearer k"));
}

#[test]
fn parse_completion_shapes() {
    let r = parse_completion(
        &json!({"choices": [{"message": {"role": "assistant", "content": "ok"},
        "finish_reason": "stop"}], "usage": {"prompt_tokens": 11, "completion_tokens": 7}}),
    );
    assert_eq!(r.message.content, "ok");
    assert_eq!((r.prompt_eval_count, r.eval_count), (Some(11), Some(7)));
    assert_eq!(r.done_reason.as_deref(), Some("stop"));
    let r = parse_completion(
        &json!({"choices": [{"message": {"content": "", "tool_calls": [
        {"id": "c1", "function": {"name": "run_bash", "arguments": "{\"cmd\": \"dir\"}"}}]}}]}),
    );
    assert_eq!(
        r.message.tool_calls[0].function.arguments,
        json!({"cmd": "dir"})
    );
    assert_eq!(r.done_reason.as_deref(), Some("tool_calls"));
}

// ---------------- SSE assembly (pure) ----------------

fn sse(chunks: &[Value]) -> String {
    let mut s = String::new();
    for c in chunks {
        s.push_str(&format!("data: {c}\n\n"));
    }
    s.push_str("data: [DONE]\n");
    s
}

#[test]
fn stream_tokens_and_think_routing() {
    let body = sse(&[
        json!({"choices": [{"delta": {"content": "<th"}}]}),
        json!({"choices": [{"delta": {"content": "ink>thought</think>he"}}]}),
        json!({"choices": [{"delta": {"reasoning_content": "fieldthought"}}]}),
        json!({"choices": [{"delta": {"content": "llo"}}], "usage": {"prompt_tokens": 5, "completion_tokens": 3}}),
    ]);
    let mut toks = Vec::new();
    let mut thinks = Vec::new();
    let mut on = |p: StreamPiece<'_>| match p {
        StreamPiece::Token(t) => toks.push(t.to_string()),
        StreamPiece::Think(t) => thinks.push(t.to_string()),
    };
    let mut asm = SseAssembler::new();
    // Feed in awkward byte boundaries to exercise line buffering.
    for chunk in body.as_bytes().chunks(7) {
        asm.push(chunk, &mut on);
    }
    assert!(asm.done);
    let r = asm.finish(&mut on);
    assert_eq!(toks, ["he", "llo"]);
    assert_eq!(thinks.concat(), "thoughtfieldthought");
    assert_eq!(r.message.content, "hello");
    assert_eq!(r.message.thinking.as_deref(), Some("thoughtfieldthought"));
    assert_eq!(r.prompt_eval_count, Some(5));
}

#[test]
fn stream_tool_call_assembly_by_index() {
    let body = sse(&[
        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "run_", "arguments": ""}}]}}]}),
        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"name": "bash", "arguments": "{\"cmd\": \"di"}}]}}]}),
        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "r\"}"}}]}}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
    ]);
    let mut on = |_: StreamPiece<'_>| {};
    let mut asm = SseAssembler::new();
    asm.push(body.as_bytes(), &mut on);
    let r = asm.finish(&mut on);
    let tc = &r.message.tool_calls[0];
    assert_eq!(tc.function.name, "run_bash");
    assert_eq!(tc.function.arguments, json!({"cmd": "dir"}));
    assert_eq!(r.done_reason.as_deref(), Some("tool_calls"));
}

// ---------------- the wire: a loopback server captures the request ----------------

/// Serve one request; returns the base URL and a receiver for (path, headers, body).
fn serve_once(
    status: &'static str,
    ctype: &'static str,
    body: String,
) -> (String, mpsc::Receiver<(String, String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        sock.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let (mut head_end, mut clen) = (None, 0usize);
        loop {
            let n = match sock.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            buf.extend_from_slice(&tmp[..n]);
            if head_end.is_none() {
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    head_end = Some(i + 4);
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    clen = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                }
            }
            if let Some(h) = head_end {
                if buf.len() >= h + clen {
                    break;
                }
            }
        }
        let h = head_end.unwrap_or(buf.len());
        let head = String::from_utf8_lossy(&buf[..h]).to_string();
        let path = head
            .lines()
            .next()
            .unwrap_or("")
            .split(' ')
            .nth(1)
            .unwrap_or("")
            .to_string();
        let body_v: Value = serde_json::from_slice(&buf[h..]).unwrap_or(Value::Null);
        let _ = tx.send((path, head, body_v));
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(resp.as_bytes());
    });
    (format!("http://{addr}/v1"), rx)
}

#[test]
fn local_engine_round_trip_non_stream_fields_and_callbacks() {
    let d = tempfile::tempdir().expect("tmp");
    let reply = json!({"choices": [{"message": {"role": "assistant",
        "content": "<think>t</think>answer", "reasoning_content": "r"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7}});
    let (base, rx) = serve_once("200 OK", "application/json", reply.to_string());
    // Cloud flavor = non-streaming; use it to exercise the once() path against the fake.
    let cp = CloudProvider {
        base_url: base,
        api_key: "k".into(),
        model: "m".into(),
    };
    let mut b = OpenAiBackend::cloud(&cp, &params(d.path(), 1024)).expect("backend");
    let mut toks = Vec::new();
    let mut thinks = Vec::new();
    let r = b
        .chat("ignored", &[Message::user("hi")], &[], &mut |p| match p {
            StreamPiece::Token(t) => toks.push(t.to_string()),
            StreamPiece::Think(t) => thinks.push(t.to_string()),
        })
        .expect("chat");
    let (path, head, body) = rx.recv().expect("captured");
    assert_eq!(path, "/v1/chat/completions");
    assert!(head.to_lowercase().contains("authorization: bearer k"));
    assert_eq!(body["model"], "m");
    assert_eq!(body["max_tokens"], 1024);
    assert_eq!(
        body["messages"][0],
        json!({"role": "user", "content": "hi"})
    );
    assert_eq!(r.message.content, "answer");
    assert_eq!(toks, ["answer"]);
    assert_eq!(thinks, ["rt"]); // field-form first, inline second
    assert_eq!((r.prompt_eval_count, r.eval_count), (Some(11), Some(7)));
}

#[test]
fn local_engine_streams_sse_and_sends_no_bearer_without_key() {
    let d = tempfile::tempdir().expect("tmp");
    let body = sse(&[
        json!({"choices": [{"delta": {"content": "he"}}]}),
        json!({"choices": [{"delta": {"content": "llo"}}], "usage": {"prompt_tokens": 5}}),
    ]);
    let (base, rx) = serve_once("200 OK", "text/event-stream", body);
    let mut b = OpenAiBackend::local(
        &OpenAiEngine {
            base_url: base,
            api_key: String::new(),
            label: "LM Studio",
        },
        &params(d.path(), 0),
    )
    .expect("backend");
    let mut toks = Vec::new();
    let r = b
        .chat("qwen", &[Message::user("hi")], &[], &mut |p| {
            if let StreamPiece::Token(t) = p {
                toks.push(t.to_string())
            }
        })
        .expect("chat");
    let (_, head, body) = rx.recv().expect("captured");
    assert!(!head.to_lowercase().contains("authorization"));
    assert_eq!(body["stream"], true);
    assert_eq!(body["model"], "qwen");
    assert_eq!(toks, ["he", "llo"]);
    assert_eq!(r.message.content, "hello");
    assert_eq!(r.prompt_eval_count, Some(5));
}

#[test]
fn http_error_becomes_status_error() {
    let d = tempfile::tempdir().expect("tmp");
    let (base, _rx) = serve_once("500 Internal Server Error", "text/plain", "boom".into());
    let mut b = OpenAiBackend::local(
        &OpenAiEngine {
            base_url: base,
            api_key: String::new(),
            label: "LM Studio",
        },
        &params(d.path(), 0),
    )
    .expect("backend");
    let e = b
        .chat("m", &[Message::user("hi")], &[], &mut |_| {})
        .expect_err("500");
    assert!(e.is_500(), "{e}");
    assert!(
        !b.restart_engine().is_some(),
        "an OpenAI engine is never restarted by us"
    );
}

// ---------------- engine dispatch + Ollama-only gates ----------------

#[test]
fn standard_picks_engine_from_config() {
    let d = tempfile::tempdir().expect("tmp");
    let p = params(d.path(), 0);
    let a = Agent::standard(p.clone(), &Config::default()).expect("agent");
    assert!(a.engine.starts_with("Ollama"), "{}", a.engine);
    let c = cfg_with(
        json!({"type": "openai", "base_url": "http://127.0.0.1:1234/v1"}),
        json!({}),
    );
    let a = Agent::standard(p.clone(), &c).expect("agent");
    assert!(a.engine.starts_with("OpenAI-compatible"), "{}", a.engine);
    if std::env::var("AGENT_OFFLINE").is_err() {
        let c = cfg_with(
            json!({}),
            json!({"enabled": true, "base_url": "https://x/v1", "model": "cm", "api_key": "k"}),
        );
        let a = Agent::standard(p, &c).expect("agent");
        assert!(a.engine.starts_with("cloud · cm"), "{}", a.engine);
    }
}

#[test]
fn model_can_think_skipped_on_openai_backend_and_unknown_when_down() {
    let c = cfg_with(
        json!({"type": "openai", "base_url": "http://127.0.0.1:9/v1"}),
        json!({}),
    );
    assert_eq!(
        rufous_kernel::agent::chat::model_can_think(&c, "qwen"),
        None
    );
    let c = Config {
        ollama_host: "http://127.0.0.1:9".into(),
        ..Config::default()
    };
    if std::env::var("OLLAMA_HOST").is_err() {
        assert_eq!(
            rufous_kernel::agent::chat::model_can_think(&c, "test:offline"),
            None
        );
    }
}

#[test]
fn probe_openai_backend_false_on_closed_port() {
    assert!(!rufous_kernel::agent::openai::probe_blocking(
        "http://127.0.0.1:9/v1",
        "",
        std::time::Duration::from_secs(2)
    ));
}

// ---------------- the loop: four 500s → restart_engine, OpenAI engines excluded ----------------

struct Flaky {
    fails_left: usize,
    restarts: usize,
    restartable: bool,
}

impl ChatBackend for Flaky {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        if self.fails_left > 0 {
            self.fails_left -= 1;
            return Err(ChatError::Status {
                status: 500,
                body: "degraded".into(),
            });
        }
        let mut r = ChatResponse::call("", "finish", json!({"summary": "ok"}));
        r.prompt_eval_count = Some(10);
        Ok(r)
    }
    fn restart_engine(&mut self) -> Option<bool> {
        if !self.restartable {
            return None;
        }
        self.restarts += 1;
        self.fails_left = 0; // the restart "fixes" the engine
        Some(true)
    }
}

struct Tools;
impl ToolRunner for Tools {
    fn run_tool(&mut self, _n: &str, _a: &Value) -> String {
        "[TASK_COMPLETE] ok".into()
    }
}

fn run_flaky(dir: &Path, restartable: bool, fails: usize) -> (Vec<String>, bool) {
    let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = logs.clone();
    let mut a = Agent::new(
        params(dir, 0),
        Flaky {
            fails_left: fails,
            restarts: 0,
            restartable,
        },
        Tools,
    )
    .with_sink(move |ev| {
        if let AgentEvent::Log(l) = ev {
            if let Ok(mut v) = sink.lock() {
                v.push(l);
            }
        }
    });
    a.sessions_dir = dir.join("sessions");
    let msgs = vec![Message::system("s"), Message::user("convert a.csv to a.md")];
    let ok = a.run(msgs, None, None).is_ok();
    let restarted = a.backend.restarts > 0;
    let l = logs.lock().expect("logs").clone();
    (l, ok && restarted || (!restartable && ok))
}

#[test]
fn four_consecutive_500s_restart_a_restartable_engine() {
    let d = tempfile::tempdir().expect("tmp");
    let (logs, ok) = run_flaky(d.path(), true, 4);
    assert!(ok);
    assert!(
        logs.iter()
            .any(|l| l.contains("restarting automatically (1/3)")),
        "{logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("✓ ollama restarted")),
        "{logs:?}"
    );
    assert!(logs.iter().any(|l| l.contains("TASK COMPLETE")));
}

#[test]
fn openai_engine_is_not_restarted_but_the_run_still_recovers() {
    let d = tempfile::tempdir().expect("tmp");
    let (logs, ok) = run_flaky(d.path(), false, 4);
    assert!(ok);
    assert!(
        logs.iter()
            .any(|l| l.contains("cannot be restarted by rufous")),
        "{logs:?}"
    );
    assert!(!logs
        .iter()
        .any(|l| l.contains("restarting automatically (")));
}

#[test]
fn messages_roles_survive_round_trip() {
    // Sanity on the Role mapping used by to_openai_messages.
    let out = to_openai_messages(&[Message::tool("read_file", "x")]);
    assert_eq!(out[0]["role"], "tool");
    assert_eq!(out[0]["tool_call_id"], "call_0");
    assert_eq!(Message::user("u").role, Role::User);
}

#[test]
fn the_agent_will_not_start_on_an_openai_backend_without_a_url() {
    let d = tempfile::tempdir().expect("tmp");
    let c = Config {
        backend_flag: Some(BackendKind::Openai),
        ..Config::default()
    };
    match Agent::standard(params(d.path(), 0), &c) {
        Err(rufous_kernel::Error::Config(m)) => assert!(m.contains("(from --backend)"), "{m}"),
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("started without a URL"),
    }
}

#[test]
fn the_base_url_flag_overrides_the_config_and_needs_a_backend_that_takes_one() {
    // A host names the engine's URL without writing config.json (boubou-f8d).
    let mut c = Config {
        backend: Some(BackendKind::Lmstudio),
        base_url_flag: Some("http://127.0.0.1:8081/v1/".into()),
        local_backend: json!({"base_url": "http://127.0.0.1:1234/v1"})
            .as_object()
            .cloned()
            .expect("object"),
        ..Config::default()
    };
    assert!(c.backend_problem().is_none());
    assert_eq!(
        c.backend_openai().map(|e| e.base_url),
        Some("http://127.0.0.1:8081/v1".to_string())
    );
    let text = serde_json::to_string(&c).expect("ser");
    assert!(!text.contains("8081"), "the flag is never written: {text}");
    let problem =
        Some("--base-url needs --backend lmstudio or openai (Ollama's address is OLLAMA_HOST)");
    c.backend = None;
    assert_eq!(c.backend_problem().as_deref(), problem);
    c.backend_flag = Some(BackendKind::Ollama);
    assert_eq!(c.backend_problem().as_deref(), problem);
    c.backend_flag = Some(BackendKind::Openai);
    assert!(c.backend_problem().is_none());
}

#[test]
fn a_closed_port_is_unreachable_on_both_engines() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr").port()
    };
    let d = tempfile::tempdir().expect("tmp");
    let p = params(d.path(), 64);
    let msgs = [Message::user("hi")];
    let mut ollama =
        rufous_kernel::agent::OllamaBackend::new(format!("http://127.0.0.1:{port}"), &p)
            .expect("backend");
    let r = ollama.chat("m", &msgs, &[], &mut |_| {});
    assert!(matches!(r, Err(ChatError::Unreachable(_))), "{r:?}");
    let mut lm = OpenAiBackend::local(
        &OpenAiEngine {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            api_key: String::new(),
            label: "LM Studio",
        },
        &p,
    )
    .expect("backend");
    let r = lm.chat("m", &msgs, &[], &mut |_| {});
    assert!(matches!(r, Err(ChatError::Unreachable(_))), "{r:?}");
}

#[test]
fn offline_means_this_machine_for_the_model_engine_too() {
    // docs/egress.md: a host's per-session switch, over AGENT_OFFLINE, and a model engine on
    // another machine is refused under it; a cloud provider is already off.
    let mut c = Config {
        backend: Some(BackendKind::Lmstudio),
        base_url_flag: Some("http://192.168.1.20:1234/v1".into()),
        ..Config::default()
    };
    assert!(
        c.backend_problem().is_none(),
        "online, a LAN engine is fine"
    );
    c.offline_forced = true;
    assert!(c.offline());
    assert_eq!(
        c.backend_problem().as_deref(),
        Some("offline: the model engine at http://192.168.1.20:1234/v1 is not on this machine")
    );
    c.base_url_flag = Some("http://127.0.0.1:1234/v1".into());
    assert!(c.backend_problem().is_none());
    assert_eq!(
        c.engine_endpoint(),
        ("LM Studio", "http://127.0.0.1:1234/v1".to_string())
    );
    let cloud = Config {
        offline_forced: true,
        ..cfg_with(
            json!({}),
            json!({"enabled": true, "base_url": "https://example.invalid/v1", "model": "m"}),
        )
    };
    assert!(cloud.cloud_provider().is_none());
}
