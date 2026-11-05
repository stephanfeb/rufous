//! Where rufous's own traffic goes (docs/egress.md, bead boubou-45n). A host that records
//! every destination a run contacted (Choreo) cannot see rufous's requests, so each
//! network-reaching tool records the hosts it contacted here, and the loop reports them with
//! the call that made them. `run_bash` is not covered: what a command does is its own.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use serde::Serialize;

/// One contacted host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Egress {
    /// The URL's host, with `:port` when the port is not the scheme's default.
    pub host: String,
    /// Why: `web_search`, `web_fetch`, `redirect`, `mcp:<server>`.
    pub purpose: String,
    /// On this machine (loopback).
    pub local: bool,
}

impl Egress {
    /// The destination of `url`, or `None` for a URL with no host.
    pub fn of(url: &str, purpose: &str) -> Option<Self> {
        // `OLLAMA_HOST` is often written without a scheme (`127.0.0.1:11434`, `0.0.0.0`).
        let url = url.trim();
        let u = if url.contains("://") {
            reqwest::Url::parse(url)
        } else {
            reqwest::Url::parse(&format!("http://{url}"))
        }
        .ok()?;
        let host = u.host_str()?.to_string();
        let local = is_loopback_host(&host);
        let host = match u.port() {
            Some(p) => format!("{host}:{p}"),
            None => host,
        };
        Some(Self {
            host,
            purpose: purpose.to_string(),
            local,
        })
    }
}

/// `localhost`, `127.0.0.0/8` or `::1` (with or without brackets), or the unspecified
/// address (`0.0.0.0`, `::`), which a client reaches on this machine.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

/// Is the URL's host on this machine? A URL that does not parse is not.
pub fn is_local_url(url: &str) -> bool {
    Egress::of(url, "").is_some_and(|e| e.local)
}

/// The hosts contacted since the last [`EgressLog::take`]. Clones share one list, so the
/// executor, its web client and its MCP registry record into the same place.
#[derive(Debug, Clone, Default)]
pub struct EgressLog(Arc<Mutex<Vec<Egress>>>);

impl EgressLog {
    /// Record `url`'s host; a repeat of an entry already listed is not added again.
    pub fn record(&self, url: &str, purpose: &str) {
        let Some(e) = Egress::of(url, purpose) else {
            return;
        };
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if !g.contains(&e) {
            g.push(e);
        }
    }

    /// Everything recorded so far, emptying the log.
    pub fn take(&self) -> Vec<Egress> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// The result of a call offline mode refuses: `tool` would have reached another machine.
pub fn offline_refusal(tool: &str) -> String {
    format!("[offline: {tool} would reach the network; this session stays on this machine]")
}

/// `host (purpose), …` for the log line.
pub fn describe(list: &[Egress]) -> String {
    list.iter()
        .map(|e| format!("{} ({})", e.host, e.purpose))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_destination_is_its_host_with_a_non_default_port_and_whether_it_is_here() {
        let e = Egress::of("https://api.search.brave.com/res/v1?q=x", "web_search").expect("url");
        assert_eq!((e.host.as_str(), e.local), ("api.search.brave.com", false));
        let e = Egress::of("http://127.0.0.1:8765/mcp", "mcp:choreo").expect("url");
        assert_eq!((e.host.as_str(), e.local), ("127.0.0.1:8765", true));
        // OLLAMA_HOST as people write it.
        assert!(is_local_url("127.0.0.1:11434"));
        assert!(is_local_url("0.0.0.0"));
        assert!(is_local_url("http://localhost:1234/v1"));
        assert!(is_local_url("http://[::1]:8080"));
        assert!(!is_local_url("http://192.168.1.20:1234/v1"));
        assert!(!is_local_url("not a url"));
    }

    #[test]
    fn the_log_lists_each_destination_once_until_taken() {
        let log = EgressLog::default();
        let shared = log.clone();
        log.record("https://html.duckduckgo.com/html/?q=a", "web_search");
        shared.record("https://html.duckduckgo.com/html/?q=b", "web_search");
        log.record("https://example.org/", "redirect");
        log.record("no host", "web_fetch");
        let got = log.take();
        assert_eq!(
            describe(&got),
            "html.duckduckgo.com (web_search), example.org (redirect)"
        );
        assert!(shared.take().is_empty());
    }
}
