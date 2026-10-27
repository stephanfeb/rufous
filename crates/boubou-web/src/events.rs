//! The SSE event bus: a broadcast channel of `{id, type, data, …}` JSON objects with a
//! monotonic id (`push_event` in Mingbird). Late subscribers only see new events, as there.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Map, Value};
use tokio::sync::broadcast;

/// Enough for a burst of token events while a client is slow; lagging clients skip ahead.
const CAPACITY: usize = 4096;

#[derive(Debug)]
pub struct Bus {
    tx: broadcast::Sender<Value>,
    seq: AtomicU64,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self {
            tx,
            seq: AtomicU64::new(0),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }

    /// `{"id": n, "type": kind, "data": data, ..extra}`; returns the id.
    pub fn push(&self, kind: &str, data: Value, extra: Map<String, Value>) -> u64 {
        let id = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut ev = json!({"id": id, "type": kind, "data": data});
        if let Some(obj) = ev.as_object_mut() {
            for (k, v) in extra {
                obj.insert(k, v);
            }
        }
        let _ = self.tx.send(ev);
        id
    }

    pub fn note(&self, text: impl Into<String>) {
        self.push("note", Value::String(text.into()), Map::new());
    }

    pub fn last_id(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }
}
