//! The Ollama HTTP client must give up on a server that stops talking.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use boubou_core::ollama::{ChatRequest, Client, Message, Options};

/// A server that accepts one connection, reads the request head, replies with `head`
/// (nothing when empty) and then holds the socket open until told to stop.
fn stalling_server(head: &'static str) -> (String, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("addr"));
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        if !head.is_empty() {
            let _ = s.write_all(head.as_bytes());
            let _ = s.flush();
        }
        let _ = rx.recv_timeout(Duration::from_secs(30));
    });
    (addr, tx)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt")
}

#[test]
fn a_request_the_server_never_answers_times_out() {
    let (host, stop) = stalling_server("");
    let client = Client::with_timeouts(host, Duration::from_secs(5), Duration::from_millis(300));
    let t0 = Instant::now();
    let res = rt().block_on(client.version());
    let dt = t0.elapsed();
    drop(stop);
    assert!(res.is_err(), "{res:?}");
    assert!(dt < Duration::from_secs(5), "took {dt:?}");
}

#[test]
fn a_chat_stream_that_stalls_mid_way_times_out_instead_of_hanging() {
    // Headers, one token, then silence: a live stream is not cut off, a dead one is.
    let (host, stop) = stalling_server(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\n\r\n\
         4c\r\n{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":false}\n\r\n",
    );
    let client = Client::with_timeouts(host, Duration::from_secs(5), Duration::from_millis(300));
    let req = ChatRequest {
        model: "m".into(),
        messages: vec![Message::user("hello")],
        tools: vec![],
        stream: true,
        think: None,
        keep_alive: None,
        options: Options::default(),
    };
    let mut seen = Vec::new();
    let t0 = Instant::now();
    let res = rt().block_on(client.chat_stream(&req, |c| {
        seen.push(c.message.content.clone());
        true
    }));
    let dt = t0.elapsed();
    drop(stop);
    assert_eq!(
        seen,
        vec!["hi".to_string()],
        "the token before the stall arrived"
    );
    assert!(res.is_err(), "{res:?}");
    assert!(dt < Duration::from_secs(5), "took {dt:?}");
}
