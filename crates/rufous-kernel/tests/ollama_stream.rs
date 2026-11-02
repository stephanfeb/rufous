//! The Ollama backend must keep every tool call a streamed turn carries.

use std::io::{Read, Write};
use std::net::TcpListener;

use rufous_kernel::agent::{ChatBackend, OllamaBackend, RunParams};
use rufous_kernel::ollama::Message;
use serde_json::json;

/// A server that answers one `/api/chat` with the given NDJSON lines and closes.
fn ndjson_server(lines: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = s.read(&mut buf);
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(body.as_bytes());
        let _ = s.flush();
    });
    addr
}

fn chunk(tool_calls: serde_json::Value, done: bool) -> String {
    json!({
        "model": "m",
        "message": {"role": "assistant", "content": "", "tool_calls": tool_calls},
        "done": done,
        "done_reason": if done { Some("stop") } else { None },
    })
    .to_string()
}

fn call(index: u64, path: &str) -> serde_json::Value {
    json!({"id": format!("call_{index}"), "function": {"index": index, "name": "list_dir", "arguments": {"path": path}}})
}

fn turn(host: String) -> Vec<(String, String)> {
    let mut b = OllamaBackend::new(host, &RunParams::plain("m", "/tmp")).expect("backend");
    let r = b
        .chat("m", &[Message::user("go")], &[], &mut |_| {})
        .expect("chat");
    r.message
        .tool_calls
        .iter()
        .map(|t| {
            (
                t.function.name.clone(),
                t.function.arguments["path"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect()
}

#[test]
fn tool_calls_streamed_one_per_chunk_all_arrive() {
    // Ollama 0.40: each parsed tool call is its own chunk.
    let host = ndjson_server(vec![
        chunk(json!([call(0, "alpha")]), false),
        chunk(json!([call(1, "beta")]), false),
        chunk(json!([]), true),
    ]);
    assert_eq!(
        turn(host),
        vec![
            ("list_dir".to_string(), "alpha".to_string()),
            ("list_dir".to_string(), "beta".to_string())
        ]
    );
}

#[test]
fn a_cumulative_tool_call_list_is_not_doubled() {
    let host = ndjson_server(vec![
        chunk(json!([call(0, "alpha")]), false),
        chunk(json!([call(0, "alpha"), call(1, "beta")]), false),
        chunk(json!([]), true),
    ]);
    assert_eq!(
        turn(host),
        vec![
            ("list_dir".to_string(), "alpha".to_string()),
            ("list_dir".to_string(), "beta".to_string())
        ]
    );
}

#[test]
fn the_done_chunks_durations_arrive_as_timing() {
    // bead boubou-xwi: load and prefill time are what tell a reload from a full prefill.
    use std::time::Duration;
    let mut done: serde_json::Value = serde_json::from_str(&chunk(json!([]), true)).expect("json");
    done["prompt_eval_count"] = json!(48806);
    done["load_duration"] = json!(1_500_000_000u64);
    done["prompt_eval_duration"] = json!(160_200_000_000u64);
    done["eval_duration"] = json!(9_400_000_000u64);
    let host = ndjson_server(vec![done.to_string()]);
    let mut b = OllamaBackend::new(host, &RunParams::plain("m", "/tmp")).expect("backend");
    let r = b
        .chat("m", &[Message::user("go")], &[], &mut |_| {})
        .expect("chat");
    let t = r.timing.expect("timing");
    assert_eq!(t.load, Duration::from_millis(1500));
    assert_eq!(t.prompt_eval, Duration::from_millis(160_200));
    assert_eq!(t.eval, Duration::from_millis(9400));
    // A server that sends no durations leaves it unset.
    let host = ndjson_server(vec![chunk(json!([]), true)]);
    let mut b = OllamaBackend::new(host, &RunParams::plain("m", "/tmp")).expect("backend");
    let r = b
        .chat("m", &[Message::user("go")], &[], &mut |_| {})
        .expect("chat");
    assert_eq!(r.timing, None);
}
