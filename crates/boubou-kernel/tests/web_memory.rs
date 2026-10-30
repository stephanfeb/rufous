//! Web search/fetch parsing and cache (`tests/test_core.py::TestCache`, `TestBatchTools`),
//! memory store/recall, and `web_fetch` against a loopback HTTP server. No real network:
//! search backends point at a closed loopback port.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use boubou_kernel::exec::Executor;
use boubou_kernel::memory::{load_memory, memory_recall, memory_store};
use boubou_kernel::safety::SafetyPolicy;
use boubou_kernel::web::{
    cache_get, cache_key, cache_set, direct_url, extract_text, is_relevant, parse_search_results,
    query_tokens, Engine, Web, CACHE_TTL, DEFAULT_BACKENDS,
};
use serde_json::json;

const BING: &str = r#"<html><body><ol id="b_results">
<li class="b_algo"><h2><a href="https://a.example/1">Alpha <b>One</b></a></h2>
  <div class="b_caption"><p>First snippet here.</p></div></li>
<li class="b_algo"><h2><a href="https://a.example/2">Beta</a></h2><p>Loose paragraph.</p></li>
<li class="b_algo"><h2>no link</h2></li>
<li class="b_algo"><h2><a href="https://a.example/3">Gamma</a></h2></li>
</ol></body></html>"#;

fn home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmp")
}

/// An executor whose search backends fail instantly (closed loopback port).
fn ex(dir: &Path) -> Executor {
    let mut e = Executor::with_policy(dir, SafetyPolicy::default());
    e.agent_home = dir.join("home");
    e.web = Web::with_backends(
        &e.agent_home,
        vec!["http://127.0.0.1:9/search?q={query}".to_string()],
    );
    e
}

// ---- cache ----

#[test]
fn cache_key_matches_mingbird_shape() {
    let k = cache_key(&["t", "k"]);
    assert_eq!(k.len(), 24);
    assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
    // hashlib.sha1(b"t|k").hexdigest()[:24], so Boubou and Mingbird share cache entries
    assert_eq!(k, "e2f8a658fee8670b31fe7295");
    assert_ne!(k, cache_key(&["t", "x"]));
}

#[test]
fn cache_roundtrip_and_missing() {
    let d = home();
    let key = cache_key(&["t", "k"]);
    cache_set(d.path(), &key, &json!({"v": 42}));
    assert_eq!(cache_get(d.path(), &key, CACHE_TTL), Some(json!({"v": 42})));
    assert_eq!(
        cache_get(d.path(), &cache_key(&["nope", "x"]), CACHE_TTL),
        None
    );
    assert_eq!(cache_get(d.path(), &key, Duration::ZERO), None); // expired
}

// ---- parsing ----

#[test]
fn bing_results_parse_like_bs4() {
    let out = parse_search_results(BING, Engine::Bing, 5);
    assert_eq!(out.len(), 3); // the h2 without a link is skipped
    assert_eq!(
        out[0],
        "• AlphaOne\n  https://a.example/1\n  First snippet here."
    );
    assert_eq!(out[1], "• Beta\n  https://a.example/2\n  Loose paragraph.");
    assert_eq!(out[2], "• Gamma\n  https://a.example/3\n  ");
    assert_eq!(parse_search_results(BING, Engine::Bing, 1).len(), 1);
}

const DDG: &str = r#"<html><body><div id="links" class="results">
<div class="result results_links results_links_deep web-result ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fen.wikipedia.org%2Fwiki%2FSouthern_Boubou&amp;rut=78d2">Southern boubou - Wikipedia</a></h2>
    <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fen.wikipedia.org%2Fwiki%2FSouthern_Boubou&amp;rut=78d2">The <b>southern</b> <b>boubou</b> is a bushshrike.</a>
  </div></div>
<div class="result results_links results_links_deep web-result ">
  <h2 class="result__title"><a rel="nofollow" class="result__a" href="https://ebird.org/species/soubou1">eBird</a></h2>
</div>
</div></body></html>"#;

#[test]
fn duckduckgo_results_unwrap_redirects() {
    let out = parse_search_results(DDG, Engine::DuckDuckGo, 5);
    assert_eq!(out.len(), 2);
    assert_eq!(
        out[0],
        "• Southern boubou - Wikipedia\n  https://en.wikipedia.org/wiki/Southern_Boubou\n  The southern boubou is a bushshrike."
    );
    assert_eq!(out[1], "• eBird\n  https://ebird.org/species/soubou1\n  ");
    assert!(parse_search_results(DDG, Engine::Bing, 5).is_empty());
}

/// Trimmed from a live `search.brave.com/search?q=jevai.net` page (2026-10-28): one hit with
/// a nested site-link cluster, one plain hit, one non-web block.
const BRAVE: &str = r#"<html><body><div id="results">
<div class="snippet svelte-jmfu5f" data-pos="0" data-type="web" data-keynav="true"><div class="result-body"><div class="result-content">
<a href="https://jevai.net/" target="_self" class="svelte-14r20fy l1"><div class="site-name-wrapper"><div class="site-name-content"><div class="desktop-small-semibold">Jev AI</div> <cite class="snippet-url">jevai.net</cite></div></div>
<div class="title search-snippet-title line-clamp-1" title="Jev AI — Decisions at machine speed">Jev AI — Decisions at machine speed</div></a>
<div class="generic-snippet"><div class="content desktop-default-regular"><!--[-1--><!--]--> <!---->Email us hello@<strong>jevai.net</strong> · Live chat Open in the bottom-right · Free 14-day trial.<!----></div></div>
</div></div>
<div class="result-cluster"><div class="snippet"><a href="https://jevai.net/features/" class="title">Features</a> <div class="description">Define outputs, provide context.</div></div></div>
</div>
<div class="snippet" data-pos="1" data-type="web"><a href="https://en.wikipedia.org/wiki/Jev_(AI_model)" class="l1"><div class="title search-snippet-title">Jev (AI model) - Wikipedia</div></a>
<div class="generic-snippet"><div class="content">3 days ago - Jev is a proprietary artificial intelligence model developed by TypeSafe AI.</div></div></div>
<div class="snippet" data-type="cluster"><a href="https://example.org/x" class="l1"><div class="title">Not a web hit</div></a></div>
</div></body></html>"#;

#[test]
fn brave_results_parse_titles_urls_and_snippets() {
    let out = parse_search_results(BRAVE, Engine::Brave, 5);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(
        out[0],
        "• Jev AI — Decisions at machine speed\n  https://jevai.net/\n  Email us hello@jevai.net · Live chat Open in the bottom-right · Free 14-day trial."
    );
    assert_eq!(
        out[1],
        "• Jev (AI model) - Wikipedia\n  https://en.wikipedia.org/wiki/Jev_(AI_model)\n  3 days ago - Jev is a proprietary artificial intelligence model developed by TypeSafe AI."
    );
    assert_eq!(parse_search_results(BRAVE, Engine::Brave, 1).len(), 1);
    assert!(parse_search_results(BRAVE, Engine::Bing, 5).is_empty());
}

#[test]
fn default_backends_brave_first_and_map_to_engines() {
    assert_eq!(DEFAULT_BACKENDS.len(), 3);
    assert_eq!(Engine::for_backend(DEFAULT_BACKENDS[0]), Engine::Brave);
    assert_eq!(Engine::for_backend(DEFAULT_BACKENDS[1]), Engine::DuckDuckGo);
    assert_eq!(Engine::for_backend(DEFAULT_BACKENDS[2]), Engine::Bing);
    assert!(DEFAULT_BACKENDS[0].contains("search.brave.com"));
    assert_eq!(
        Engine::for_backend("https://example.org/s?q={query}"),
        Engine::Bing
    );
}

#[test]
fn relevance_floor_tokens_and_matching() {
    assert_eq!(query_tokens("jevai.net"), vec!["jevai"]);
    assert_eq!(
        query_tokens("What is the capital of France?"),
        vec!["capital", "france"]
    );
    assert!(query_tokens("the of").is_empty());
    let t = query_tokens("jevai.net");
    assert!(is_relevant(
        &t,
        "• Jev AI\n  https://jevai.net/\n  Email us hello@jevai.net"
    ));
    assert!(!is_relevant(
        &t,
        "• Gmail\n  https://mail.google.com/\n  Gmail is email on the internet"
    ));
    // Five-letter prefixes cover plurals and verb forms, both ways round.
    assert!(is_relevant(
        &query_tokens("decision models"),
        "• Decisions at machine speed\n  u\n  "
    ));
    assert!(is_relevant(
        &query_tokens("decisions"),
        "• a decision model\n  u\n  "
    ));
    // Short tokens match whole words only.
    let t = query_tokens("ai");
    assert!(is_relevant(&t, "• Jev AI\n  u\n  "));
    assert!(!is_relevant(&t, "• Aim high\n  u\n  "));
    // No tokens, no floor.
    assert!(is_relevant(&[], "anything"));
}

#[test]
fn direct_url_recognises_site_shaped_queries() {
    assert_eq!(
        direct_url("jevai.net").as_deref(),
        Some("https://jevai.net")
    );
    assert_eq!(
        direct_url(" typesafe.ai/blog ").as_deref(),
        Some("https://typesafe.ai/blog")
    );
    assert_eq!(
        direct_url("https://example.org/x?y=1").as_deref(),
        Some("https://example.org/x?y=1")
    );
    assert_eq!(
        direct_url("127.0.0.1:8080").as_deref(),
        Some("http://127.0.0.1:8080")
    );
    assert_eq!(direct_url("localhost:8080"), None);
    for q in [
        "open weights system-one models",
        "python 3.12",
        "e.g.",
        "",
        "jevai",
        "a.b.c.d.e.",
        "what is jevai.net",
    ] {
        assert_eq!(direct_url(q), None, "{q:?}");
    }
}

#[test]
fn extract_text_drops_chrome_and_scripts() {
    let html = r#"<html><head><title>T</title><style>p{}</style><script>var x=1;</script></head>
<body><header>Site</header><nav>menu</nav><main><h1>Hello</h1><p>World &amp; <b>friends</b>.</p></main>
<footer>foot</footer><noscript>js off</noscript></body></html>"#;
    let t = extract_text(html);
    assert_eq!(t, "T Hello World & friends .");
}

// ---- web_fetch against a loopback server ----

fn serve_once(body: &'static str) -> String {
    serve_once_at(body, "page")
}

/// One-shot loopback server; `path` lets a backend URL carry an engine name for
/// `Engine::for_backend` (`…/duckduckgo`).
fn serve_once_at(body: &'static str, path: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes());
        }
    });
    format!("http://{addr}/{path}")
}

/// What Bing returned for `jevai.net` on 2026-10-28: well-formed organic results about
/// something else entirely, with no "no results" notice in the markup.
const BING_FILLER: &str = r#"<html><body><ol id="b_results">
<li class="b_algo"><h2><a href="https://techcommunity.microsoft.com/forms-quiz">"Locking" Forms Quiz to prevent cheating | Microsoft Community Hub</a></h2>
  <div class="b_caption"><p>How to lock a Microsoft Forms quiz.</p></div></li>
<li class="b_algo"><h2><a href="https://mail.google.com/">Gmail</a></h2>
  <div class="b_caption"><p>Gmail is email that is intuitive, efficient, and useful.</p></div></li>
</ol></body></html>"#;

/// The DuckDuckGo HTML endpoint's bot check (HTTP 202 in the wild, 2026-10-28).
const DDG_CHALLENGE: &str = r#"<html><body><div class="anomaly-modal__mask"><div class="anomaly-modal__modal">
<div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>
<div class="anomaly-modal__description">Please complete the following challenge to confirm this search was made by a human.</div>
</div></div></body></html>"#;

#[test]
fn irrelevant_results_fall_through_to_the_next_backend() {
    let d = home();
    let bing = serve_once_at(BING_FILLER, "bing");
    let ddg = serve_once_at(DDG, "duckduckgo");
    let web = Web::with_backends(d.path(), vec![bing.clone(), ddg.clone()]);
    let out = web.search("southern boubou", 5);
    // The Wikipedia hit passes the floor; eBird (bare title, unrelated URL) does not.
    assert_eq!(
        out,
        "• Southern boubou - Wikipedia\n  https://en.wikipedia.org/wiki/Southern_Boubou\n  The southern boubou is a bushshrike."
    );
    let key_of = |b: &str| {
        cache_key(&[
            "search",
            b.split("//").last().expect("host"),
            "southern boubou",
            "5",
        ])
    };
    // The filler set was not cached; the accepted set was.
    assert!(cache_get(&web.cache_dir, &key_of(&bing), CACHE_TTL).is_none());
    assert!(cache_get(&web.cache_dir, &key_of(&ddg), CACHE_TTL).is_some());
}

#[test]
fn all_irrelevant_results_give_a_no_results_note_and_are_not_cached() {
    let d = home();
    let bing = serve_once_at(BING_FILLER, "bing");
    let web = Web::with_backends(d.path(), vec![bing.clone()]);
    let out = web.search("boubouzzz", 5);
    assert!(
        out.starts_with("[web_search: no relevant results for \"boubouzzz\" ("),
        "{out}"
    );
    assert!(
        out.contains("2 results, none mentioning the query"),
        "{out}"
    );
    assert!(out.contains("web_fetch the site directly"));
    let host = bing.split("//").last().expect("host");
    let key = cache_key(&["search", host, "boubouzzz", "5"]);
    assert!(cache_get(&web.cache_dir, &key, CACHE_TTL).is_none());
}

#[test]
fn duckduckgo_challenge_is_reported_not_counted_as_empty() {
    let d = home();
    let ddg = serve_once_at(DDG_CHALLENGE, "duckduckgo");
    let web = Web::with_backends(d.path(), vec![ddg]);
    let out = web.search("boubou", 5);
    assert!(out.starts_with("[web_search: no relevant results"), "{out}");
    assert!(
        out.contains("served a bot challenge instead of results"),
        "{out}"
    );
}

/// An engine's results about a site talk about the site, not about its domain label.
const BING_ABOUT_SITE: &str = r#"<html><body><ol id="b_results">
<li class="b_algo"><h2><a href="https://example.org/review">Boubou review: a local agent for small models</a></h2>
  <div class="b_caption"><p>Hands-on with the desktop assistant.</p></div></li>
<li class="b_algo"><h2><a href="https://mail.google.com/">Gmail</a></h2>
  <div class="b_caption"><p>Gmail is email that is intuitive, efficient, and useful.</p></div></li>
</ol></body></html>"#;

#[test]
fn fetched_site_title_widens_the_relevance_floor() {
    let d = home();
    let url = serve_once_at(
        "<html><body><h1>Boubou home</h1><p>A local agent.</p></body></html>",
        "",
    );
    let hostport = url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let bing = serve_once_at(BING_ABOUT_SITE, "bing");
    let web = Web::with_backends(d.path(), vec![bing]);
    let out = web.search(&hostport, 5);
    // "Boubou review…" shares no word with "127.0.0.1:PORT" but does with the page title;
    // Gmail shares neither and is dropped.
    assert!(out.starts_with("• http://"), "{out}");
    assert!(
        out.contains("\n• Boubou review: a local agent for small models\n"),
        "{out}"
    );
    assert!(!out.contains("Gmail"), "{out}");
}

#[test]
fn site_shaped_query_is_fetched_directly_before_the_engines() {
    let d = home();
    let url = serve_once_at(
        "<html><body><h1>Boubou home</h1><p>A local agent.</p></body></html>",
        "",
    );
    let hostport = url.trim_start_matches("http://").trim_end_matches('/');
    let web = Web::with_backends(d.path(), vec![]);
    let out = web.search(hostport, 5);
    assert!(
        out.starts_with(&format!(
            "• http://{hostport} (fetched directly)\n  http://{hostport}\n  Boubou home A local agent."
        )),
        "{out}"
    );
    assert!(
        out.ends_with("[web_search: no engine results (no search backends configured)]"),
        "{out}"
    );
}

#[test]
fn web_fetch_stores_full_text_and_returns_summary() {
    let d = home();
    let url =
        serve_once("<html><body><script>x()</script><h1>Title</h1><p>Body text</p></body></html>");
    let web = Web::with_backends(&d.path().join("home"), vec![]);
    let out = web.fetch(&url, Some(d.path()));
    assert!(out.starts_with("Title Body text"));
    assert!(out.contains("[full text saved: sources/fetch_"));
    let key = cache_key(&["fetch", &url]);
    let saved = std::fs::read_to_string(d.path().join("sources").join(format!("fetch_{key}.txt")))
        .expect("source file");
    assert_eq!(saved, format!("URL: {url}\n\nTitle Body text"));
    // Second call is served from the cache (the server accepted only one connection).
    assert_eq!(web.fetch(&url, Some(d.path())), out);
}

#[test]
fn web_fetch_error_is_a_marked_tool_error() {
    let d = home();
    let web = Web::with_backends(d.path(), vec![]);
    let out = web.fetch("http://127.0.0.1:9/x", Some(d.path()));
    assert!(out.starts_with("[web_fetch error:"));
}

#[test]
fn web_search_reports_last_error_when_all_backends_fail() {
    let d = home();
    let web = Web::with_backends(
        d.path(),
        vec![
            "http://127.0.0.1:9/a".to_string(),
            "http://127.0.0.1:9/b?x=1".to_string(),
        ],
    );
    let out = web.search("anything", 5);
    assert!(out.starts_with("[web_search error:"));
    let multi = web.search_multi(&["q1".to_string(), "q2".to_string()], 2);
    assert!(multi.starts_with("### Query: q1\n[web_search error:"));
    assert!(multi.contains("\n\n### Query: q2\n"));
    assert_eq!(
        web.search_multi(&[], 2),
        "[web_search_multi: a queries array is required]"
    );
}

#[test]
fn web_search_serves_cached_results() {
    let d = home();
    let web = Web::with_backends(d.path(), vec!["http://127.0.0.1:9/s".to_string()]);
    let key = cache_key(&["search", "127.0.0.1:9/s", "rust", "5"]);
    cache_set(&web.cache_dir, &key, &json!("• cached\n  u\n  s"));
    assert_eq!(web.search("rust", 5), "• cached\n  u\n  s");
}

// ---- memory ----

#[test]
fn memory_store_and_recall() {
    let d = home();
    assert_eq!(memory_recall(d.path(), "x", 3), "(no memory)");
    assert_eq!(
        memory_store(d.path(), "user likes Rust and static linking"),
        "[saved 1 facts]"
    );
    assert_eq!(
        memory_store(d.path(), "the project is called Boubou"),
        "[saved 2 facts]"
    );
    assert_eq!(memory_store(d.path(), "unrelated note"), "[saved 3 facts]");
    let mem = load_memory(d.path());
    assert_eq!(mem.len(), 3);
    assert!(mem[0]["ts"].as_str().is_some_and(|t| t.len() == 16));
    let r = memory_recall(d.path(), "What does the user like? rust", 3);
    assert!(r.starts_with('['));
    assert!(r.contains("user likes Rust"));
    assert!(!r.contains("unrelated"));
    // No overlap → the last three facts, oldest first, bare text.
    let r = memory_recall(d.path(), "zzz", 3);
    assert_eq!(
        r,
        "user likes Rust and static linking\nthe project is called Boubou\nunrelated note"
    );
}

#[test]
fn memory_tools_run_through_the_executor() {
    let d = home();
    let mut e = ex(d.path());
    assert_eq!(
        e.run_tool("memory_store", &json!({"text": "fact one"})),
        "[saved 1 facts]"
    );
    let r = e.run_tool("memory_recall", &json!({"query": "fact"}));
    assert!(r.contains("fact one"));
    assert!(e.agent_home.join("memory.json").exists());
}

// ---- batch_tools (TestBatchTools) ----

#[test]
fn batch_serial_file_ops_no_race() {
    let d = home();
    let mut e = ex(d.path());
    let out = e.run_tool(
        "batch_tools",
        &json!({"calls": [
            {"tool": "create_file", "args": {"path": "a.txt", "content": "AAA"}},
            {"tool": "read_file", "args": {"path": "a.txt"}}
        ]}),
    );
    assert!(out.contains("AAA"));
    assert!(out.starts_with("### [1] create_file\n"));
    assert!(out.contains("\n\n### [2] read_file\n"));
}

#[test]
fn batch_nested_blocked() {
    let d = home();
    let mut e = ex(d.path());
    let out = e.run_tool(
        "batch_tools",
        &json!({"calls": [{"tool": "batch_tools", "args": {}}]}),
    );
    assert!(out.contains("nested batch_tools"));
}

#[test]
fn batch_dangerous_still_blocked() {
    let d = home();
    let mut e = ex(d.path());
    let out = e.run_tool(
        "batch_tools",
        &json!({"calls": [{"tool": "run_bash", "args": {"command": "rm -rf /x"}}]}),
    );
    assert!(out.contains("dangerous operation"));
}

#[test]
fn batch_parallel_search_blocks_are_labelled_in_order() {
    let d = home();
    let mut e = ex(d.path());
    let out = e.run_tool(
        "batch_tools",
        &json!({"calls": [
            {"tool": "web_search_multi", "args": {"queries": ["x"], "max_results": 1}},
            {"tool": "web_search", "args": {"query": "y"}},
            {"tool": "list_dir", "args": {"path": "."}}
        ]}),
    );
    let i1 = out.find("### [1] web_search_multi").expect("block 1");
    let i2 = out.find("### [2] web_search").expect("block 2");
    let i3 = out.find("### [3] list_dir").expect("block 3");
    assert!(i1 < i2 && i2 < i3);
    assert!(out.contains("[web_search error:"));
}

#[test]
fn batch_network_calls_run_concurrently() {
    // Two fetches against a server that answers each request after a delay: concurrent
    // execution finishes well under twice the delay.
    let d = home();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for _ in 0..2 {
            if let Ok((mut sock, _)) = listener.accept() {
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf);
                    std::thread::sleep(Duration::from_millis(600));
                    let body = "<p>slow</p>";
                    let _ = sock.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    );
                });
            }
        }
    });
    let mut e = ex(d.path());
    let t0 = std::time::Instant::now();
    let out = e.run_tool(
        "batch_tools",
        &json!({"calls": [
            {"tool": "web_fetch", "args": {"url": format!("http://{addr}/one")}},
            {"tool": "web_fetch", "args": {"url": format!("http://{addr}/two")}}
        ]}),
    );
    let el = t0.elapsed();
    assert_eq!(out.matches("slow").count(), 2, "{out}");
    assert!(el < Duration::from_millis(1100), "took {el:?}");
}
