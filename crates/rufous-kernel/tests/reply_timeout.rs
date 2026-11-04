//! A model server that goes quiet past `AGENT_APITIMEOUT` is reported as a timeout, before
//! the first token or mid-reply, not as a generic transport error (bead boubou-yqu). Its own
//! test binary: it sets the timeout to one second for the whole process.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use rufous_kernel::agent::chat::ChatError;
use rufous_kernel::agent::{ChatBackend, OllamaBackend, RunParams};
use rufous_kernel::ollama::Message;

/// Accepts one request, writes `reply` (possibly nothing), then holds the socket open.
fn stalling_server(reply: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let _ = s.read(&mut buf);
        let _ = s.write_all(reply.as_bytes());
        let _ = s.flush();
        std::thread::sleep(Duration::from_secs(5));
    });
    addr
}

fn chat(host: String) -> Result<(), ChatError> {
    std::env::set_var("AGENT_APITIMEOUT", "1");
    let mut b = OllamaBackend::new(host, &RunParams::plain("m", "/tmp")).expect("backend");
    b.chat("m", &[Message::user("go")], &[], &mut |_| {})
        .map(|_| ())
}

#[test]
fn silence_before_the_headers_is_a_timeout_before_the_first_token() {
    let host = stalling_server("");
    assert_eq!(chat(host), Err(ChatError::Timeout { mid_reply: false }));
}

#[test]
fn silence_after_a_chunk_is_a_timeout_mid_reply() {
    const ONE_CHUNK: &str = concat!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\n\r\n",
        "4a\r\n",
        r#"{"model":"m","message":{"role":"assistant","content":"Hel"},"done":false}"#,
        "\n\r\n"
    );
    let host = stalling_server(ONE_CHUNK);
    assert_eq!(chat(host), Err(ChatError::Timeout { mid_reply: true }));
}
