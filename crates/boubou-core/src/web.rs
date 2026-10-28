//! `web_search`, `web_search_multi`, `web_fetch` and the on-disk result cache.
//!
//! Search scrapes the HTML of the configured backends (Brave, DuckDuckGo's HTML endpoint,
//! Bing and Baidu by default, overridable with `AGENT_SEARCH_BACKENDS`); Bing/Baidu use
//! Mingbird's selectors, Brave and DuckDuckGo are Boubou's additions. Three guards sit on top
//! of the scraping (2026-10-28, after Bing answered "jevai.net" with ten Microsoft Forms
//! pages): a result must mention at least one word of the query or it is dropped, a backend
//! whose results all fail that floor is skipped and never cached, a bot-challenge page is
//! reported instead of counting as "no results", and a query that names a site
//! (`jevai.net`) is fetched directly before any engine is asked.
//! Fetch stores the whole page text under `<workdir>/sources/` and returns an 800-char
//! summary plus the path, so the model can `read_file` any part later without the full text
//! living in the context. Both are cached for an hour under `<agent home>/cache/`, keyed like
//! Mingbird (sha1 of the joined parts, first 24 hex chars) so the two stacks share a cache.

use std::path::{Path, PathBuf};
use std::time::Duration;

use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::text::{clen, head};

/// Brave first: it serves full HTML to Boubou's user agent and indexes new sites quickly.
/// DuckDuckGo's HTML endpoint second (it currently answers bots with a challenge page, which
/// `search` reports rather than treating as "no results"). Bing third: its filler results
/// for unindexed terms are caught by the relevance floor. Baidu last, for Chinese queries.
/// Override with `AGENT_SEARCH_BACKENDS` (comma-separated URLs).
pub const DEFAULT_BACKENDS: [&str; 4] = [
    "https://search.brave.com/search?source=web",
    "https://html.duckduckgo.com/html/",
    "https://www.bing.com/search",
    "https://www.baidu.com/s",
];
pub const CACHE_TTL: Duration = Duration::from_secs(3600);
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
const FETCH_TIMEOUT: Duration = Duration::from_secs(25);
/// Chars of page text returned inline by `web_fetch`.
pub const FETCH_SUMMARY_CHARS: usize = 800;
/// Chars of a directly fetched site shown as the lead result of `web_search`.
const DIRECT_FETCH_CHARS: usize = 400;
/// Leading words of a directly fetched page (its title) that widen the relevance floor.
const DIRECT_TITLE_WORDS: usize = 6;

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

/// Text nodes concatenated as written, whitespace collapsed: keeps `hello@<strong>x</strong>`
/// as `hello@x` where `text_spaced` would insert a space and `text_joined` would eat one.
fn text_flat(el: ElementRef<'_>) -> String {
    el.text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Which result-page layout a backend serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Bing,
    Baidu,
    DuckDuckGo,
    Brave,
}

impl Engine {
    /// Guess from the backend URL; anything unknown is parsed with Bing's selectors.
    pub fn for_backend(url: &str) -> Self {
        if url.contains("baidu") {
            Self::Baidu
        } else if url.contains("duckduckgo") {
            Self::DuckDuckGo
        } else if url.contains("brave") {
            Self::Brave
        } else {
            Self::Bing
        }
    }
}

/// Result blocks (`• title\n  url\n  snippet`) from a Brave, Bing, Baidu or DuckDuckGo
/// results page.
pub fn parse_search_results(html: &str, engine: Engine, max_results: usize) -> Vec<String> {
    let doc = Html::parse_document(html);
    if engine == Engine::Brave {
        return parse_brave(&doc, max_results);
    }
    let items = match engine {
        Engine::Bing => sel("li.b_algo"),
        Engine::Baidu => sel("div.result.c-container"),
        Engine::DuckDuckGo => sel("div.result"),
        Engine::Brave => unreachable!("handled above"),
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

/// Brave (`search.brave.com/search?q=…`, markup of 2026-10-28): each organic hit is a
/// `div.snippet[data-type="web"]`; its first anchor carries the target URL and holds the
/// `div.title`; the description is `div.generic-snippet div.content`. Site-link clusters
/// nested inside a hit (`div.result-cluster`) repeat the same site and are not emitted.
fn parse_brave(doc: &Html, max_results: usize) -> Vec<String> {
    let items = sel(r#"div.snippet[data-type="web"]"#);
    let (anchor, title, content) = (
        sel("a[href]"),
        sel("div.title"),
        sel("div.generic-snippet div.content"),
    );
    let mut out = Vec::new();
    for hit in doc.select(&items).take(max_results) {
        let Some(a) = hit.select(&anchor).next() else {
            continue;
        };
        let href = a.value().attr("href").unwrap_or("");
        let title = a
            .select(&title)
            .next()
            .map(text_flat)
            .filter(|t| !t.is_empty())
            .or_else(|| a.value().attr("title").map(str::to_string))
            .unwrap_or_default();
        let snippet = hit
            .select(&content)
            .next()
            .map(|c| head(&text_flat(c), 200).to_string())
            .unwrap_or_default();
        if !title.is_empty() && !href.is_empty() {
            out.push(format!("• {title}\n  {href}\n  {snippet}"));
        }
    }
    out
}

/// A backend answered with a bot check instead of results. Only DuckDuckGo's page is
/// recognised today (`anomaly-modal`, "bots use DuckDuckGo too"); a generic "captcha"
/// substring would misfire on Brave, whose pages carry that word in a string table.
pub fn challenge_notice(engine: Engine, body: &str) -> Option<&'static str> {
    match engine {
        Engine::DuckDuckGo
            if body.contains("anomaly-modal") || body.contains("bots use DuckDuckGo too") =>
        {
            Some("served a bot challenge instead of results")
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- relevance

/// Words carrying no information about what the user wants, plus URL scaffolding, so a
/// query like `jevai.net` is judged on `jevai` alone and `what is the capital of France` on
/// `capital` and `france`.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "in", "on", "for", "with", "is", "are", "was", "be",
    "what", "which", "who", "how", "why", "when", "where", "do", "does", "did", "can", "i", "me",
    "my", "you", "it", "this", "that", "about", "from", "by", "at", "as", "vs", "com", "net",
    "org", "www", "http", "https",
];

/// Words of `query` that a relevant result should echo: lowercased alphanumeric runs of two
/// characters or more, minus [`STOP_WORDS`], deduplicated.
pub fn query_tokens(query: &str) -> Vec<String> {
    let mut toks: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2 && !STOP_WORDS.contains(w))
        .map(String::from)
        .collect();
    toks.sort();
    toks.dedup();
    toks
}

/// The relevance floor: `block` (title, URL and snippet) must contain at least one token as
/// a whole word, or share a five-letter prefix with one (plurals and verb forms). Non-ASCII
/// tokens (Chinese has no word boundaries) match as substrings. No tokens means no floor.
pub fn is_relevant(tokens: &[String], block: &str) -> bool {
    if tokens.is_empty() {
        return true;
    }
    let lower = block.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    tokens.iter().any(|t| {
        if !t.is_ascii() {
            return lower.contains(t.as_str());
        }
        words.iter().any(|w| {
            *w == t.as_str()
                || (t.len() >= 5 && w.starts_with(t.as_str()))
                || (w.len() >= 5 && t.starts_with(w))
        })
    })
}

/// A query that names one site (`jevai.net`, `typesafe.ai/blog`, `https://example.org`):
/// the site's own page is a better first result than whatever an engine has indexed about
/// it. Returns the URL to fetch, `https` unless the host is an IPv4 address or has a port.
pub fn direct_url(query: &str) -> Option<String> {
    let q = query.trim();
    if q.is_empty() || q.contains(char::is_whitespace) {
        return None;
    }
    let rest = q
        .strip_prefix("https://")
        .or_else(|| q.strip_prefix("http://"))
        .unwrap_or(q);
    let hostport = rest.split('/').next()?;
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        _ => (hostport, None),
    };
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok =
        |l: &&str| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if labels.len() < 2 || !labels.iter().all(label_ok) {
        return None;
    }
    let ipv4 = labels.len() == 4 && labels.iter().all(|l| l.bytes().all(|b| b.is_ascii_digit()));
    let tld_ok = labels
        .last()
        .is_some_and(|t| t.len() >= 2 && t.bytes().all(|b| b.is_ascii_alphabetic()));
    if !ipv4 && !tld_ok {
        return None;
    }
    if q.starts_with("http://") || q.starts_with("https://") {
        return Some(q.to_string());
    }
    let scheme = if ipv4 || port.is_some() {
        "http"
    } else {
        "https"
    };
    Some(format!("{scheme}://{rest}"))
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

    /// `web_search(query, max_results=5)`. Backends are tried in order; the first whose
    /// results pass the relevance floor wins and is cached. A query that names a site is
    /// fetched directly first and that page leads the output whatever the engines say.
    pub fn search(&self, query: &str, max_results: usize) -> String {
        let n = max_results.to_string();
        let mut tokens = query_tokens(query);
        let direct = direct_url(query).and_then(|url| {
            let text = self.fetch(&url, None);
            (!text.starts_with("[web_fetch error")).then_some((url, text))
        });
        if let Some((_, text)) = &direct {
            // The site's own title says what it is about, and engine results about a site
            // echo those words rather than its domain label ("Jev" for jevai.net).
            let title = text
                .split_whitespace()
                .take(DIRECT_TITLE_WORDS)
                .collect::<Vec<_>>()
                .join(" ");
            tokens.extend(
                query_tokens(&title)
                    .into_iter()
                    .filter(|t| t.chars().count() >= 3),
            );
            tokens.sort();
            tokens.dedup();
        }
        let direct = direct.map(|(url, text)| {
            format!(
                "• {url} (fetched directly)\n  {url}\n  {}",
                head(&text, DIRECT_FETCH_CHARS)
            )
        });
        let lead = |body: String| match &direct {
            Some(d) => format!("{d}\n{body}"),
            None => body,
        };
        // Why each backend gave nothing, for the final message and the log.
        let mut notes: Vec<String> = Vec::new();
        let mut any_body = false;
        for backend in &self.backends {
            let host = backend.split("//").last().unwrap_or(backend);
            let site = host.split('/').next().unwrap_or(host);
            let key = cache_key(&["search", host, query, &n]);
            if let Some(Value::String(s)) = cache_get(&self.cache_dir, &key, CACHE_TTL) {
                return lead(s);
            }
            let url = if backend.contains("{query}") {
                backend.replace("{query}", &urlencode(query))
            } else {
                let sep = if backend.contains('?') { '&' } else { '?' };
                format!("{backend}{sep}q={}", urlencode(query))
            };
            let body = match self.get(&url, SEARCH_TIMEOUT) {
                Ok(b) => b,
                Err(e) => {
                    notes.push(format!("{site}: {}", head(&e, 80)));
                    continue;
                }
            };
            any_body = true;
            let engine = Engine::for_backend(backend);
            if let Some(why) = challenge_notice(engine, &body) {
                tracing::warn!("web_search: {site} {why}");
                notes.push(format!("{site} {why}"));
                continue;
            }
            let parsed = parse_search_results(&body, engine, max_results);
            if parsed.is_empty() {
                notes.push(format!("{site}: no results"));
                continue;
            }
            let total = parsed.len();
            let kept: Vec<String> = parsed
                .into_iter()
                .filter(|b| is_relevant(&tokens, b))
                .collect();
            if kept.is_empty() {
                tracing::warn!(
                    "web_search: {site} returned {total} results for {query:?}, none mentioning it"
                );
                notes.push(format!(
                    "{site}: {total} results, none mentioning the query"
                ));
                continue;
            }
            let result = kept.join("\n");
            cache_set(&self.cache_dir, &key, &Value::String(result.clone()));
            return lead(result);
        }
        let why = if notes.is_empty() {
            "no search backends configured".to_string()
        } else {
            notes.join("; ")
        };
        match (direct, any_body) {
            (Some(d), _) => format!("{d}\n[web_search: no engine results ({why})]"),
            (None, true) => format!(
                "[web_search: no relevant results for {query:?} ({why}). Rephrase the query with distinctive words, or web_fetch the site directly.]"
            ),
            (None, false) => format!("[web_search error: {why}]"),
        }
    }

    /// `web_search_multi(queries, max_results=3)`: one thread per query, blocks labelled.
    pub fn search_multi(&self, queries: &[String], max_results: usize) -> String {
        if queries.is_empty() {
            return "[web_search_multi: a queries array is required]".to_string();
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
            .map(|(q, r)| format!("### Query: {q}\n{r}"))
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
            return "[web_fetch error: the page has no text content]".to_string();
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
                "{summary}\n[full text saved: sources/{fname} ({} chars). Use read_file on the relevant part when you need details.]",
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
