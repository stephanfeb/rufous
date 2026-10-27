//! `web_search`, `web_search_multi`, `web_fetch` and the on-disk result cache.
//!
//! Search scrapes the HTML of the configured backends (Bing, DuckDuckGo's HTML endpoint and
//! Baidu by default, overridable with `AGENT_SEARCH_BACKENDS`); Bing/Baidu use Mingbird's
//! selectors, DuckDuckGo is Boubou's addition. Fetch stores the whole page
//! text under `<workdir>/sources/` and returns an 800-char summary plus the path, so the
//! model can `read_file` any part later without the full text living in the context.
//! Both are cached for an hour under `<agent home>/cache/`, keyed like Mingbird
//! (sha1 of the joined parts, first 24 hex chars) so the two stacks share a cache.

use std::path::{Path, PathBuf};
use std::time::Duration;

use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::text::{clen, head};

/// Western-first (Boubou is used from Southeast Asia); Baidu stays as the last resort for
/// Chinese queries. Override with `AGENT_SEARCH_BACKENDS` (comma-separated URLs).
pub const DEFAULT_BACKENDS: [&str; 3] = [
    "https://www.bing.com/search",
    "https://html.duckduckgo.com/html/",
    "https://www.baidu.com/s",
];
pub const CACHE_TTL: Duration = Duration::from_secs(3600);
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
const FETCH_TIMEOUT: Duration = Duration::from_secs(25);
/// Chars of page text returned inline by `web_fetch`.
pub const FETCH_SUMMARY_CHARS: usize = 800;

// ---------------------------------------------------------------- cache

/// `_cache_key`: sha1 of `parts` joined by `|`, first 24 hex chars.
pub fn cache_key(parts: &[&str]) -> String {
    let raw = parts.join("|");
    use std::fmt::Write as _;
    let digest = Sha1::digest(raw.as_bytes());
    let mut hex = String::with_capacity(40);
    for b in digest {
        let _ = write!(hex, "{b:02x}");
    }
    hex.truncate(24);
    hex
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `_cache_get`: the cached `data` if present, parseable and younger than `ttl`.
pub fn cache_get(dir: &Path, key: &str, ttl: Duration) -> Option<Value> {
    let raw = std::fs::read_to_string(dir.join(format!("{key}.json"))).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let ts = v.get("ts").and_then(Value::as_f64).unwrap_or(0.0);
    if now_secs() - ts < ttl.as_secs_f64() {
        v.get("data").cloned()
    } else {
        None
    }
}

/// `_cache_set`: write `{ts, data}`; failures are silent (the cache only speeds things up).
pub fn cache_set(dir: &Path, key: &str, data: &Value) {
    let _ = std::fs::create_dir_all(dir);
    let v = serde_json::json!({"ts": now_secs(), "data": data});
    if let Ok(s) = serde_json::to_string(&v) {
        let _ = std::fs::write(dir.join(format!("{key}.json")), s);
    }
}

// ---------------------------------------------------------------- parsing

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

/// BeautifulSoup `get_text(strip=True)`: text nodes trimmed and concatenated.
fn text_joined(el: ElementRef<'_>) -> String {
    el.text().map(str::trim).filter(|t| !t.is_empty()).collect()
}

/// BeautifulSoup `get_text(" ", strip=True)`: trimmed text nodes joined by one space.
fn text_spaced(el: ElementRef<'_>) -> String {
    el.text()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Which result-page layout a backend serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Bing,
    Baidu,
    DuckDuckGo,
}

impl Engine {
    /// Guess from the backend URL; anything unknown is parsed with Bing's selectors.
    pub fn for_backend(url: &str) -> Self {
        if url.contains("baidu") {
            Self::Baidu
        } else if url.contains("duckduckgo") {
            Self::DuckDuckGo
        } else {
            Self::Bing
        }
    }
}

/// Result blocks (`• title\n  url\n  snippet`) from a Bing, Baidu or DuckDuckGo results page.
pub fn parse_search_results(html: &str, engine: Engine, max_results: usize) -> Vec<String> {
    let doc = Html::parse_document(html);
    let items = match engine {
        Engine::Bing => sel("li.b_algo"),
        Engine::Baidu => sel("div.result.c-container"),
        Engine::DuckDuckGo => sel("div.result"),
    };
    let (h2a, h3a, ddg_a) = (sel("h2 a"), sel("h3 a"), sel("a.result__a"));
    let (cap_bing, cap_p, cap_baidu, cap_ddg) = (
        sel(".b_caption p"),
        sel("p"),
        sel(".c-abstract"),
        sel(".result__snippet"),
    );
    let mut out = Vec::new();
    for li in doc.select(&items).take(max_results) {
        let a = match engine {
            Engine::DuckDuckGo => li.select(&ddg_a).next(),
            _ => li.select(&h2a).next().or_else(|| li.select(&h3a).next()),
        };
        let Some(a) = a else { continue };
        let title = text_joined(a);
        let href = a.value().attr("href").unwrap_or("");
        let href = if engine == Engine::DuckDuckGo {
            ddg_target(href)
        } else {
            href.to_string()
        };
        let snippet = match engine {
            Engine::DuckDuckGo => li.select(&cap_ddg).next(),
            _ => li
                .select(&cap_bing)
                .next()
                .or_else(|| li.select(&cap_p).next())
                .or_else(|| li.select(&cap_baidu).next()),
        }
        .map(|c| head(&text_spaced(c), 200).to_string())
        .unwrap_or_default();
        if !title.is_empty() {
            out.push(format!("• {title}\n  {href}\n  {snippet}"));
        }
    }
    out
}

/// DuckDuckGo wraps result links as `//duckduckgo.com/l/?uddg=<encoded url>&rut=…`;
/// return the real target (or the href unchanged when it is not a redirect).
fn ddg_target(href: &str) -> String {
    let Some(q) = href.split_once('?').map(|(_, q)| q) else {
        return href.to_string();
    };
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("uddg=") {
            return urldecode(v);
        }
    }
    href.to_string()
}

/// `urllib.parse.unquote_plus`.
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Page text with script/style/nav/footer/header/noscript removed, text nodes joined by
/// single spaces (BeautifulSoup `get_text(" ", strip=True)` after `decompose()`).
pub fn extract_text(html: &str) -> String {
    let mut doc = Html::parse_document(html);
    let junk = sel("script, style, nav, footer, header, noscript");
    let ids: Vec<_> = doc.select(&junk).map(|e| e.id()).collect();
    for id in ids {
        if let Some(mut n) = doc.tree.get_mut(id) {
            n.detach();
        }
    }
    text_spaced(doc.root_element())
}

// ---------------------------------------------------------------- client

/// The network side of the web tools. Cheap to share: `&Web` is `Sync`, so
/// `web_search_multi` and `batch_tools` fan out on threads.
#[derive(Debug, Clone)]
pub struct Web {
    http: reqwest::blocking::Client,
    pub cache_dir: PathBuf,
    /// Search backends in order; `{query}` placeholder or a `q=` parameter.
    pub backends: Vec<String>,
}

impl Web {
    /// Backends from `AGENT_SEARCH_BACKENDS` (comma-separated) or the defaults.
    pub fn new(agent_home: &Path) -> Self {
        let backends = std::env::var("AGENT_SEARCH_BACKENDS")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_BACKENDS.iter().map(|s| s.to_string()).collect());
        Self::with_backends(agent_home, backends)
    }

    pub fn with_backends(agent_home: &Path, backends: Vec<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            http,
            cache_dir: agent_home.join("cache"),
            backends,
        }
    }

    fn get(&self, url: &str, timeout: Duration) -> Result<String, String> {
        self.http
            .get(url)
            .timeout(timeout)
            .send()
            .and_then(|r| r.text())
            .map_err(|e| e.to_string())
    }

    /// `web_search(query, max_results=5)`.
    pub fn search(&self, query: &str, max_results: usize) -> String {
        let mut last_err = String::new();
        let n = max_results.to_string();
        for backend in &self.backends {
            let host = backend.split("//").last().unwrap_or(backend);
            let key = cache_key(&["search", host, query, &n]);
            if let Some(Value::String(s)) = cache_get(&self.cache_dir, &key, CACHE_TTL) {
                return s;
            }
            let url = if backend.contains("{query}") {
                backend.replace("{query}", &urlencode(query))
            } else {
                let sep = if backend.contains('?') { '&' } else { '?' };
                format!("{backend}{sep}q={}", urlencode(query))
            };
            match self.get(&url, SEARCH_TIMEOUT) {
                Ok(body) => {
                    let out =
                        parse_search_results(&body, Engine::for_backend(backend), max_results);
                    if !out.is_empty() {
                        let result = out.join("\n");
                        cache_set(&self.cache_dir, &key, &Value::String(result.clone()));
                        return result;
                    }
                }
                Err(e) => last_err = head(&e, 80).to_string(),
            }
        }
        format!("[web_search error: {last_err}]")
    }

    /// `web_search_multi(queries, max_results=3)`: one thread per query, blocks labelled.
    pub fn search_multi(&self, queries: &[String], max_results: usize) -> String {
        if queries.is_empty() {
            return "[web_search_multi: 需提供 queries 数组]".to_string();
        }
        let results: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = queries
                .iter()
                .map(|q| s.spawn(move || self.search(q, max_results)))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| "[error: search thread panicked]".into())
                })
                .collect()
        });
        queries
            .iter()
            .zip(results)
            .map(|(q, r)| format!("### 查询: {q}\n{r}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// `web_fetch(url, workdir)`: full text to `sources/fetch_<key>.txt`, summary inline.
    pub fn fetch(&self, url: &str, workdir: Option<&Path>) -> String {
        let key = cache_key(&["fetch", url]);
        if let Some(Value::String(s)) = cache_get(&self.cache_dir, &key, CACHE_TTL) {
            return s;
        }
        let body = match self.get(url, FETCH_TIMEOUT) {
            Ok(b) => b,
            Err(e) => return format!("[web_fetch error: {e}]"),
        };
        let full = extract_text(&body);
        if full.is_empty() {
            return "[web_fetch error: 页面无文本内容]".to_string();
        }
        let fname = format!("fetch_{key}.txt");
        if let Some(wd) = workdir {
            let dir = wd.join("sources");
            if std::fs::create_dir_all(&dir).is_ok() {
                let _ = std::fs::write(dir.join(&fname), format!("URL: {url}\n\n{full}"));
            }
        }
        let summary = head(&full, FETCH_SUMMARY_CHARS);
        let out = if workdir.is_some() {
            format!(
                "{summary}\n[全文已存: sources/{fname} ({} 字符)。需要细节时用 read_file 读取对应部分。]",
                clen(&full)
            )
        } else {
            head(summary, 6000).to_string()
        };
        cache_set(&self.cache_dir, &key, &Value::String(out.clone()));
        out
    }
}

/// `urllib.parse.quote_plus`.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
