//! MCP: config/expose parsing (`tests/test_core.py::TestMcpManifest`), schema compaction,
//! manifest cache with stale fallback and shortened TTL (`test_v200_perf.py`), flattened
//! tool definitions (`mcp_tool_defs`), path resolution, and the `rmcp` client against a
//! fake stdio server in `fixtures/fake_mcp.py` (needs `python3`; skipped when absent).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rufous_kernel::exec::Executor;
use rufous_kernel::mcp::{
    self, compact_schema, is_path_key, probe_server, resolve_mcp_paths, McpConfig, McpRegistry,
    ProbedTool, Prober, ServerConfig, CACHE_FILE, MANIFEST_TTL,
};
use rufous_kernel::safety::SafetyPolicy;
use serde_json::{json, Map, Value};

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn cfg(json: Value) -> ServerConfig {
    serde_json::from_value(json).expect("server config")
}

fn python() -> Option<String> {
    for cand in ["python3", "python"] {
        if std::process::Command::new(cand)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Some(cand.to_string());
        }
    }
    eprintln!("skipping: no python interpreter on PATH");
    None
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn fake_server(py: &str) -> ServerConfig {
    cfg(json!({
        "command": py,
        "args": [fixture("fake_mcp.py").to_string_lossy()],
        "categories": ["web"],
        "tools": {"needs_q": {"expose": "on-demand"}, "save_path": ["files"], "write_file": ["files"]}
    }))
}

// ---- config ----

#[test]
fn expose_parser_matches_mingbird() {
    let c = cfg(json!({"command": "x", "tools": {
        "a": ["web"],
        "b": {"categories": ["files"], "expose": "on-demand"}
    }}));
    assert_eq!(c.expose_for("a"), "auto");
    assert_eq!(c.expose_for("b"), "on-demand");
    assert_eq!(c.categories_for("a"), vec!["web"]);
    assert_eq!(c.categories_for("b"), vec!["files"]);
    assert_eq!(c.categories_for("zzz"), vec!["mcp"]); // nothing declared → mcp
    let d = cfg(json!({"command": "x", "expose": "disabled"}));
    assert_eq!(d.expose_for("c"), "disabled");
}

#[test]
fn server_categories_filter_unknown_tags_and_unknown_keys_are_ignored() {
    let c = cfg(
        // The `network` alias and a capitalised English tag both normalise.
        json!({"command": "x", "categories": ["Network", "Files", "bogus"], "disabled": false, "type": "stdio"}),
    );
    assert_eq!(c.server_categories(), vec!["web", "files"]);
    assert_eq!(c.unknown_categories(), vec!["bogus"]);
    let h = cfg(json!({"url": "https://mcp.example/x", "headers": {"Authorization": "Bearer k"}}));
    assert!(h.is_http() && h.is_runnable());
    assert_eq!(h.server_categories(), vec!["mcp"]);
    assert!(!cfg(json!({"categories": ["mcp"]})).is_runnable());
    assert_eq!(
        mcp::split_qualified("utils.calculate"),
        Some(("utils", "calculate"))
    );
    assert_eq!(mcp::split_qualified("nodot"), None);
    assert_eq!(mcp::split_qualified(".x"), None);
}

#[test]
fn compact_schema_keeps_names_types_enums_descriptions_and_required_as_sent() {
    // docs/mcp-schemas.md (boubou-erw).
    let js = json!({"type": "object", "properties": {
        "kind": {"type": "string", "enum": ["master", "document", "log"],
                 "description": "master: a list of things;\n  document: one per event"},
        "n": {"type": "integer", "description": ""},
        "odd": {"type": "string", "enum": [{"not": "scalar"}]},
        "weird": {"description": "no type"}
    }, "required": ["kind"]});
    let c = compact_schema(&js);
    assert_eq!(
        c["properties"],
        json!({
            "kind": {"type": "string", "enum": ["master", "document", "log"],
                     "description": "master: a list of things; document: one per event"},
            "n": {"type": "integer"},
            "odd": {"type": "string"}
        })
    );
    assert_eq!(c["required"], json!(["kind"]));
    // A long description is cut to the cap.
    let c = compact_schema(
        &json!({"properties": {"a": {"type": "string", "description": "x".repeat(300)}}}),
    );
    let d = c["properties"]["a"]["description"].as_str().unwrap_or("");
    assert_eq!(d.chars().count(), rufous_kernel::mcp::PARAM_DESC_CAP + 1);
    // No `required` → nothing is required (bizframe's `show.name` is optional).
    let c = compact_schema(&json!({"properties": {"a": {"type": "string"}}}));
    assert_eq!(c["required"], json!([]));
    // > 8 properties → only the required ones survive, when some are named
    let mut props = Map::new();
    for i in 0..10 {
        props.insert(format!("p{i}"), json!({"type": "string"}));
    }
    let c = compact_schema(&json!({"properties": props.clone(), "required": ["p1", "p7"]}));
    assert_eq!(c["properties"].as_object().map(|m| m.len()), Some(2));
    let c = compact_schema(&json!({"properties": props}));
    assert_eq!(c["properties"].as_object().map(|m| m.len()), Some(10));
    assert_eq!(
        compact_schema(&json!("nope")),
        json!({"type": "object", "properties": {}, "required": []})
    );
}

#[test]
fn a_host_tool_keeps_parameter_descriptions_a_routed_tool_keeps_only_enums() {
    let d = tempfile::tempdir().expect("tmp");
    let mut servers = McpConfig::new();
    servers.insert(
        "bizframe".into(),
        ServerConfig {
            host: true,
            ..cfg(json!({"url": "http://127.0.0.1:9/mcp"}))
        },
    );
    servers.insert("notes".into(), cfg(json!({"command": "x"})));
    let mut t = probed("set_entity", "Adds an entity");
    t.schema = compact_schema(&json!({"properties": {
        "kind": {"type": "string", "enum": ["master", "document", "log"], "description": "what it records"}
    }, "required": ["kind"]}));
    let reg = McpRegistry::with_prober(
        d.path(),
        servers,
        Box::new(Shared(Scripted::new(Some(vec![t])))),
    );
    let host = reg.host_face(false, 15);
    assert_eq!(
        host.defs[0]["function"]["parameters"]["properties"]["kind"],
        json!({"type": "string", "enum": ["master", "document", "log"], "description": "what it records"})
    );
    let routed = reg.tool_defs(&set(&["mcp"]), &set(&[]), false);
    assert_eq!(routed[0]["function"]["name"], "notes.set_entity");
    assert_eq!(
        routed[0]["function"]["parameters"]["properties"]["kind"],
        json!({"type": "string", "enum": ["master", "document", "log"]})
    );
    assert_eq!(
        routed[0]["function"]["parameters"]["required"],
        json!(["kind"])
    );
}

// ---- manifest cache ----

struct Scripted {
    result: std::sync::Mutex<Option<Vec<ProbedTool>>>,
    calls: AtomicUsize,
}

impl Scripted {
    fn new(result: Option<Vec<ProbedTool>>) -> Arc<Self> {
        Arc::new(Self {
            result: std::sync::Mutex::new(result),
            calls: AtomicUsize::new(0),
        })
    }
}

/// Newtype so the foreign `Prober` trait can be implemented for a shared handle.
struct Shared(Arc<Scripted>);

impl Prober for Shared {
    fn probe(&self, _cfg: &ServerConfig) -> Option<Vec<ProbedTool>> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        self.0.result.lock().expect("lock").clone()
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_secs_f64()
}

fn slow_servers() -> McpConfig {
    let mut m = McpConfig::new();
    m.insert("slow".into(), cfg(json!({"command": "x"})));
    m
}

#[test]
fn manifest_stale_merge_and_short_ttl() {
    let d = tempfile::tempdir().expect("tmp");
    // Expired disk cache that still knows an old tool for `slow`.
    std::fs::write(
        d.path().join(CACHE_FILE),
        json!({"ts": now() - 400.0, "data": {"slow": {"categories": ["mcp"], "tools": [
            {"name": "old_tool", "desc": "", "schema": {}, "categories": ["mcp"], "expose": "auto"}
        ]}}})
        .to_string(),
    )
    .expect("write");
    let failing = Scripted::new(None);
    let reg = McpRegistry::with_prober(d.path(), slow_servers(), Box::new(Shared(failing.clone())));
    let m = reg.manifest(true);
    assert_eq!(m["slow"].tools[0].name, "old_tool");
    assert!(reg.cache_ts().expect("ts") <= now() - 240.0); // failure → retry in 60s
    assert_eq!(failing.calls.load(Ordering::SeqCst), 1);

    let ok = Scripted::new(Some(vec![]));
    let reg = McpRegistry::with_prober(d.path(), slow_servers(), Box::new(Shared(ok.clone())));
    let m2 = reg.manifest(true);
    assert!(m2["slow"].tools.is_empty());
    assert!(reg.cache_ts().expect("ts") >= now() - 5.0);
    // Disk cache now carries the fresh (empty) result in Mingbird's {ts, data} shape.
    let disk: Value =
        serde_json::from_str(&std::fs::read_to_string(d.path().join(CACHE_FILE)).expect("read"))
            .expect("json");
    assert!(disk["ts"].as_f64().is_some());
    assert_eq!(disk["data"]["slow"]["categories"], json!(["mcp"]));
}

/// Records which servers were probed (by their command), and fails those in `fail`.
struct Recorder {
    probed: std::sync::Mutex<Vec<String>>,
    fail: std::sync::Mutex<Vec<String>>,
}

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            probed: std::sync::Mutex::new(Vec::new()),
            fail: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn take(&self) -> Vec<String> {
        let mut v = std::mem::take(&mut *self.probed.lock().expect("lock"));
        v.sort();
        v
    }
}

struct Rec(Arc<Recorder>);

impl Prober for Rec {
    fn probe(&self, cfg: &ServerConfig) -> Option<Vec<ProbedTool>> {
        let cmd = cfg.command.clone().unwrap_or_default();
        let name = Path::new(&cmd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.0.probed.lock().expect("lock").push(name.clone());
        if self.0.fail.lock().expect("lock").contains(&name) {
            return None;
        }
        let arg = cfg.args.first().cloned().unwrap_or_default();
        Some(vec![probed(&format!("{name}_tool{arg}"), "d")])
    }
}

fn bins(dir: &Path) -> (String, String) {
    let a = dir.join("srv_a");
    let b = dir.join("srv_b");
    std::fs::write(&a, "v1").expect("a");
    std::fs::write(&b, "v1").expect("b");
    (a.display().to_string(), b.display().to_string())
}

fn two(a: &str, b: &str) -> McpConfig {
    let mut m = McpConfig::new();
    m.insert("a".into(), cfg(json!({"command": a})));
    m.insert("b".into(), cfg(json!({"command": b})));
    m
}

#[test]
fn a_changed_server_is_probed_again_and_only_it() {
    // docs/mcp-cache.md (boubou-gj0).
    let home = tempfile::tempdir().expect("home");
    let (a, b) = bins(home.path());
    let r = Recorder::new();
    let reg = McpRegistry::with_prober(home.path(), two(&a, &b), Box::new(Rec(r.clone())));
    reg.manifest(false);
    assert_eq!(r.take(), vec!["srv_a", "srv_b"]);
    // A new process with the same servers reads the disk cache.
    let reg = McpRegistry::with_prober(home.path(), two(&a, &b), Box::new(Rec(r.clone())));
    reg.manifest(false);
    assert!(r.take().is_empty());
    // Changed args: that server only.
    let mut servers = two(&a, &b);
    servers.insert("a".into(), cfg(json!({"command": a, "args": ["2"]})));
    let reg = McpRegistry::with_prober(home.path(), servers.clone(), Box::new(Rec(r.clone())));
    let m = reg.manifest(false);
    assert_eq!(r.take(), vec!["srv_a"]);
    assert_eq!(m["a"].tools[0].name, "srv_a_tool2");
    // A rebuilt binary: that server only, also within one registry's lifetime.
    std::fs::write(&b, "version two").expect("rebuild");
    let m = reg.manifest(false);
    assert_eq!(r.take(), vec!["srv_b"]);
    assert_eq!(m.len(), 2);
    reg.manifest(false);
    assert!(r.take().is_empty());
}

#[test]
fn a_server_added_under_a_fresh_cache_is_probed_and_a_removed_one_is_not_served() {
    let home = tempfile::tempdir().expect("home");
    let (a, b) = bins(home.path());
    let r = Recorder::new();
    let mut only_a = McpConfig::new();
    only_a.insert("a".into(), cfg(json!({"command": a})));
    McpRegistry::with_prober(home.path(), only_a.clone(), Box::new(Rec(r.clone()))).manifest(false);
    assert_eq!(r.take(), vec!["srv_a"]);
    // A second session within the TTL adds `b` (as session/new does).
    let m = McpRegistry::with_prober(home.path(), two(&a, &b), Box::new(Rec(r.clone())))
        .manifest(false);
    assert_eq!(r.take(), vec!["srv_b"]);
    assert!(m.contains_key("a") && m.contains_key("b"));
    // A session without `b` is not served its tools, and probes nothing.
    let m = McpRegistry::with_prober(home.path(), only_a, Box::new(Rec(r.clone()))).manifest(false);
    assert!(r.take().is_empty());
    assert_eq!(m.keys().collect::<Vec<_>>(), vec!["a"]);
    // `b` stays in the file for the sessions that have it.
    let disk: Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join(CACHE_FILE)).expect("read"))
            .expect("json");
    assert!(disk["data"]["b"].is_object() && disk["servers"]["b"]["fp"].is_string());
}

#[test]
fn a_changed_server_whose_probe_fails_keeps_its_old_tools_and_retries_soon() {
    let home = tempfile::tempdir().expect("home");
    let (a, b) = bins(home.path());
    let r = Recorder::new();
    McpRegistry::with_prober(home.path(), two(&a, &b), Box::new(Rec(r.clone()))).manifest(false);
    r.take();
    r.fail.lock().expect("lock").push("srv_a".into());
    std::fs::write(&a, "broken rebuild").expect("rebuild");
    let reg = McpRegistry::with_prober(home.path(), two(&a, &b), Box::new(Rec(r.clone())));
    let m = reg.manifest(false);
    assert_eq!(r.take(), vec!["srv_a"]);
    assert_eq!(m["a"].tools[0].name, "srv_a_tool");
    assert!(reg.cache_ts().expect("ts") <= now() - 240.0);
}

#[test]
fn a_cache_file_in_the_old_shape_is_read_as_stale() {
    let home = tempfile::tempdir().expect("home");
    let (a, b) = bins(home.path());
    std::fs::write(
        home.path().join(CACHE_FILE),
        json!({"ts": now(), "data": {"a": {"categories": ["mcp"], "tools": [
            {"name": "old", "desc": "", "schema": {}, "categories": ["mcp"], "expose": "auto"}
        ]}, "gone": {"categories": ["mcp"], "tools": []}}})
        .to_string(),
    )
    .expect("write");
    let r = Recorder::new();
    let m = McpRegistry::with_prober(home.path(), two(&a, &b), Box::new(Rec(r.clone())))
        .manifest(false);
    assert_eq!(r.take(), vec!["srv_a", "srv_b"]);
    assert_eq!(m["a"].tools[0].name, "srv_a_tool");
    let disk: Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join(CACHE_FILE)).expect("read"))
            .expect("json");
    assert!(disk["data"]["gone"].is_null());
}

#[test]
fn manifest_memory_and_disk_caches_skip_probing() {
    let d = tempfile::tempdir().expect("tmp");
    let tool = ProbedTool {
        name: "t".into(),
        title: None,
        desc: "d".into(),
        schema: json!({"type": "object", "properties": {}}),
    };
    let p = Scripted::new(Some(vec![tool]));
    let reg = McpRegistry::with_prober(d.path(), slow_servers(), Box::new(Shared(p.clone())));
    assert_eq!(reg.manifest(false)["slow"].tools.len(), 1);
    assert_eq!(reg.manifest(false)["slow"].tools.len(), 1);
    assert_eq!(p.calls.load(Ordering::SeqCst), 1, "memory cache");
    // A fresh registry (new process) reads the disk cache instead of probing.
    let p2 = Scripted::new(None);
    let reg2 = McpRegistry::with_prober(d.path(), slow_servers(), Box::new(Shared(p2.clone())));
    assert_eq!(reg2.manifest(false)["slow"].tools.len(), 1);
    assert_eq!(p2.calls.load(Ordering::SeqCst), 0, "disk cache");
    assert!(MANIFEST_TTL >= Duration::from_secs(300));
}

// ---- tool defs ----

fn probed(name: &str, desc: &str) -> ProbedTool {
    ProbedTool {
        name: name.into(),
        title: None,
        desc: desc.into(),
        schema: json!({"type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]}),
    }
}

#[test]
fn tool_defs_flatten_by_category_expose_and_builtin_overlap() {
    let d = tempfile::tempdir().expect("tmp");
    let mut servers = McpConfig::new();
    servers.insert(
        "web".into(),
        cfg(json!({"command": "x", "categories": ["web"], "tools": {
            "heavy": {"expose": "on-demand"},
            "off": {"expose": "disabled"},
            "note": ["memory"],
            "write_file": ["files"]
        }})),
    );
    servers.insert(
        "fs".into(),
        cfg(json!({"url": "https://mcp.example/fs", "categories": ["files"]})),
    );
    let p = Scripted::new(Some(vec![
        probed("search", &format!("{}\nline2", "x".repeat(70))),
        probed("heavy", "h"),
        probed("off", "o"),
        probed("note", ""),
        probed("read_file", "same name as a built-in"),
        probed("write_file", "file verb covered by create_file"),
    ]));
    let reg = McpRegistry::with_prober(d.path(), servers, Box::new(Shared(p)));
    let names = |defs: &[Value]| -> Vec<String> {
        defs.iter()
            .map(|d| d["function"]["name"].as_str().unwrap_or("").to_string())
            .collect()
    };
    let defs = reg.tool_defs(&set(&["web"]), &set(&[]), false);
    assert_eq!(names(&defs), vec!["web.search"]); // heavy/off need extra; note is memory; dup names skipped
    let desc = defs[0]["function"]["description"].as_str().unwrap_or("");
    assert_eq!(desc.chars().count(), 61);
    assert!(desc.ends_with('…') && !desc.contains('\n'));
    assert_eq!(defs[0]["function"]["parameters"]["required"], json!(["q"]));

    let defs = reg.tool_defs(&set(&["memory"]), &set(&[]), false);
    let n = names(&defs);
    assert_eq!(n, vec!["web.note"]);
    assert_eq!(defs[0]["function"]["description"], json!("web tool"));

    // extra forces on-demand/disabled/dup tools in regardless of category.
    let defs = reg.tool_defs(
        &set(&["code"]),
        &set(&["web.heavy", "web.off", "web.read_file", "web.write_file"]),
        false,
    );
    assert_eq!(
        names(&defs),
        vec!["web.heavy", "web.off", "web.read_file", "web.write_file"]
    );

    // files category: the scripted prober gives `fs` the same six tools. read_file (same name
    // as a built-in) and write_file (file verb the built-ins cover) are skipped; the rest ride
    // along in probe order. web.write_file is files too but a covered verb → only when named.
    assert_eq!(
        names(&reg.tool_defs(&set(&["files"]), &set(&[]), false)),
        vec!["fs.search", "fs.heavy", "fs.off", "fs.note"]
    );
    assert_eq!(
        names(&reg.tool_defs(&set(&["files"]), &set(&["web.write_file"]), false)),
        vec![
            "fs.search",
            "fs.heavy",
            "fs.off",
            "fs.note",
            "web.write_file"
        ]
    );
    // Offline skips url servers entirely → only web is left, and its file verb is covered.
    assert!(reg.tool_defs(&set(&["files"]), &set(&[]), true).is_empty());
    assert!(
        reg.is_mcp_tool("web.search") && !reg.is_mcp_tool("nope.search") && !reg.is_mcp_tool("web")
    );
}

#[test]
fn empty_registry_yields_nothing_and_names_the_config_file() {
    let d = tempfile::tempdir().expect("tmp");
    let reg = McpRegistry::new(d.path(), McpConfig::new());
    assert!(reg
        .tool_defs(&set(&["web", "mcp"]), &set(&["x.y"]), false)
        .is_empty());
    let out = reg.call("utils", "calculate", &Map::new(), d.path());
    assert!(out.starts_with("[MCP server 'utils' not configured. Edit "));
    assert!(out.contains("mcp.json"));
    assert!(
        !d.path().join(CACHE_FILE).exists(),
        "no servers → no probe, no cache file"
    );
}

// ---- path resolution ----

#[test]
fn resolve_mcp_paths_like_mingbird() {
    let wd = if cfg!(windows) {
        Path::new("C:\\work")
    } else {
        Path::new("/work")
    };
    let args: Map<String, Value> = serde_json::from_value(json!({
        "path": "sub/a.txt",
        "output_file": "'out/b.docx'",
        "url": "sub/not_a_path_key",
        "src": "https://x.example/img.png",
        "target": "/abs/already",
        "input_dir": "../up/../x",
        "doc": "",
        "count": 3,
        "filename": "mailto:someone"
    }))
    .expect("map");
    let out = resolve_mcp_paths(&args, wd);
    let s = |k: &str| out[k].as_str().unwrap_or("").replace('\\', "/");
    assert!(s("path").ends_with("/work/sub/a.txt"), "{}", s("path"));
    assert!(s("output_file").ends_with("/work/out/b.docx"));
    assert_eq!(s("url"), "sub/not_a_path_key");
    assert_eq!(s("src"), "https://x.example/img.png");
    assert_eq!(s("target"), "/abs/already");
    assert!(
        s("input_dir").ends_with("/x") && !s("input_dir").contains(".."),
        "{}",
        s("input_dir")
    );
    assert_eq!(s("doc"), "");
    assert_eq!(out["count"], json!(3));
    assert_eq!(s("filename"), "mailto:someone");
    assert!(is_path_key("docx_path") && is_path_key("FilePath") && !is_path_key("query"));
}

// ---- live rmcp client against the fake server ----

#[test]
fn probe_stdio_returns_compacted_tools() {
    let Some(py) = python() else { return };
    let tools = probe_server(&fake_server(&py), Duration::from_secs(30)).expect("probe ok");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "needs_q", "write_file", "save_path"]);
    assert_eq!(
        tools[0].schema["properties"]["text"]["type"],
        json!("string")
    );
    assert_eq!(tools[0].schema["required"], json!(["text"]));
    assert_eq!(tools[0].desc, "Echo text back\nsecond line");
    assert_eq!(tools[1].desc.len(), 80); // 150-char cap not reached
}

#[test]
fn probe_stdio_timeout_kills_child() {
    let Some(py) = python() else { return };
    let d = tempfile::tempdir().expect("tmp");
    let pidfile = d.path().join("pid.txt");
    let c = cfg(json!({"command": py, "args": [
        fixture("hang_mcp.py").to_string_lossy(), pidfile.to_string_lossy()
    ]}));
    let t0 = std::time::Instant::now();
    let r = probe_server(&c, Duration::from_secs(3));
    assert!(r.is_none());
    assert!(t0.elapsed() < Duration::from_secs(20));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !pidfile.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .expect("pidfile")
        .trim()
        .parse()
        .expect("pid");
    // The child must be gone (or at least a zombie no longer running). Poll briefly.
    let alive = |pid: u32| -> bool {
        if cfg!(windows) {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}")])
                .output();
            out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        } else {
            let out = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output();
            out.is_ok_and(|o| {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                !s.is_empty() && !s.starts_with('Z')
            })
        }
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline && alive(pid) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!alive(pid), "hung MCP child {pid} still running");
}

#[test]
fn executor_calls_flattened_and_mcp_call_tools() {
    let Some(py) = python() else { return };
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path().join("ws");
    std::fs::create_dir_all(&wd).expect("mkdir");
    let mut servers = McpConfig::new();
    servers.insert("fake".into(), fake_server(&py));
    let reg = Arc::new(McpRegistry::new(&d.path().join("home"), servers));
    let mut ex = Executor::with_policy(&wd, SafetyPolicy::default()).with_mcp(reg.clone());

    // flattened name
    assert_eq!(ex.run_tool("fake.echo", &json!({"text": "hi"})), "echo: hi");
    // legacy mcp_call
    assert_eq!(
        ex.run_tool(
            "mcp_call",
            &json!({"server": "fake", "tool": "echo", "args": {"text": "yo"}})
        ),
        "echo: yo"
    );
    // argument error → expected parameter names appended (from the manifest)
    let err = ex.run_tool("fake.needs_q", &json!({}));
    assert!(err.starts_with("[mcp_call error:"), "{err}");
    assert!(
        err.contains("The tool needs_q expects the parameters: q"),
        "{err}"
    );
    // relative path argument resolved against the workdir before the call
    let out = ex.run_tool("fake.save_path", &json!({"path": "docs/a.md"}));
    let want = wd.join("docs").join("a.md").to_string_lossy().into_owned();
    assert_eq!(out, format!("path={want}"));
    // unknown server, unknown tool
    assert!(ex
        .run_tool("nope.echo", &json!({}))
        .starts_with("[MCP server 'nope' not configured."));
    assert!(ex
        .run_tool("fake.missing", &json!({}))
        .starts_with("[mcp_call error:"));
    // the registry served the loop's definitions for the routed category
    let defs = reg.tool_defs(&set(&["web"]), &set(&[]), false);
    let names: Vec<&str> = defs
        .iter()
        .filter_map(|d| d["function"]["name"].as_str())
        .collect();
    assert_eq!(names, vec!["fake.echo"]); // needs_q on-demand, write_file dup, save_path is files
    let defs = reg.tool_defs(&set(&["files"]), &set(&[]), false);
    let names: Vec<&str> = defs
        .iter()
        .filter_map(|d| d["function"]["name"].as_str())
        .collect();
    assert_eq!(names, vec!["fake.save_path"]);
    // batch_tools runs MCP calls on the parallel path, blocks labelled in order
    let out = ex.run_tool(
        "batch_tools",
        &json!({"calls": [
            {"tool": "fake.echo", "args": {"text": "1"}},
            {"tool": "list_dir", "args": {"path": "."}},
            {"tool": "fake.echo", "args": {"text": "2"}}
        ]}),
    );
    assert!(out.starts_with("### [1] fake.echo\necho: 1"), "{out}");
    assert!(out.contains("### [3] fake.echo\necho: 2"), "{out}");
}

// ---- the project's .mcp.json (bead boubou-w3i.3) ----

#[test]
fn both_config_shapes_parse_to_the_same_servers() {
    let shared = r#"{
      "mcpServers": {
        "notes": { "command": "python3", "args": ["notes.py"], "env": { "NOTES_DIR": "/n" } },
        "docs": { "type": "http", "url": "http://127.0.0.1:1/mcp", "headers": { "Authorization": "Bearer x" }, "categories": ["web"] }
      }
    }"#;
    let bare = r#"{
      "notes": { "command": "python3", "args": ["notes.py"], "env": { "NOTES_DIR": "/n" } },
      "docs": { "url": "http://127.0.0.1:1/mcp", "headers": { "Authorization": "Bearer x" }, "categories": ["web"] }
    }"#;
    let a = mcp::parse_config(shared, Path::new(".mcp.json")).expect("shared shape");
    let b = mcp::parse_config(bare, Path::new("mcp.json")).expect("bare shape");
    assert_eq!(a, b);
    assert_eq!(a["notes"].command.as_deref(), Some("python3"));
    assert_eq!(a["notes"].env["NOTES_DIR"], "/n");
    assert!(a["docs"].is_http());
    assert_eq!(a["docs"].server_categories(), vec!["web".to_string()]);
    let err = mcp::parse_config("{ not json", Path::new("x/.mcp.json")).expect_err("bad json");
    assert!(err.to_string().contains(".mcp.json"), "{err}");
}

#[test]
fn the_projects_servers_join_the_homes_and_win_a_name_clash() {
    let d = tempfile::tempdir().expect("tmp");
    let home = d.path().join("home");
    let work = d.path().join("proj");
    std::fs::create_dir_all(&home).expect("mkdir");
    std::fs::create_dir_all(&work).expect("mkdir");
    std::fs::write(
        home.join("mcp.json"),
        r#"{ "a": { "command": "home-a" }, "b": { "command": "home-b" } }"#,
    )
    .expect("w");
    std::fs::write(
        work.join(".mcp.json"),
        r#"{ "mcpServers": { "b": { "command": "proj-b" }, "c": { "command": "proj-c" } } }"#,
    )
    .expect("w");
    let servers = mcp::load_for(&home, &d.path().join("none"), &work);
    assert_eq!(servers.len(), 3);
    assert_eq!(servers["a"].command.as_deref(), Some("home-a"));
    assert_eq!(
        servers["b"].command.as_deref(),
        Some("proj-b"),
        "project wins"
    );
    assert_eq!(servers["c"].command.as_deref(), Some("proj-c"));

    // A project file that does not parse is skipped; the home servers still count.
    std::fs::write(work.join(".mcp.json"), "{ broken").expect("w");
    let servers = mcp::load_for(&home, &d.path().join("none"), &work);
    assert_eq!(servers.len(), 2);

    // No project file: the home alone, as before.
    std::fs::remove_file(work.join(".mcp.json")).expect("rm");
    assert_eq!(mcp::load_for(&home, &d.path().join("none"), &work).len(), 2);
    assert!(mcp::load_project(&work)
        .expect("absent is empty")
        .is_empty());

    // The executor's configured registry sees the project's servers.
    std::fs::write(
        work.join(".mcp.json"),
        r#"{ "mcpServers": { "proj_only_server": { "command": "x" } } }"#,
    )
    .expect("w");
    let e = Executor::with_policy(&work, SafetyPolicy::default()).with_configured_mcp();
    assert!(e.mcp.servers.contains_key("proj_only_server"));
    let mut e = e;
    e.set_mcp_servers(McpConfig::new());
    assert!(e.mcp.servers.is_empty());
}

#[test]
fn host_servers_are_assembled_whole_sorted_and_long_described_not_routed() {
    // docs/host-tools.md: a `session/new` server of a host is on the face every turn, with
    // no category or expose filter, descriptions up to 200 characters; the routed path
    // skips it so nothing is listed twice.
    let d = tempfile::tempdir().expect("tmp");
    let mut servers = McpConfig::new();
    servers.insert(
        "choreo".into(),
        ServerConfig {
            host: true,
            ..cfg(json!({"url": "http://127.0.0.1:9/mcp", "tools": {"weekly": {"expose": "on-demand"}}}))
        },
    );
    servers.insert("notes".into(), cfg(json!({"command": "x"})));
    let p = Scripted::new(Some(vec![
        probed("say_hello", &"Greets someone. ".repeat(20)),
        probed("weekly", "w"),
        probed("read_file", "a built-in's name"),
    ]));
    let reg = McpRegistry::with_prober(d.path(), servers, Box::new(Shared(p)));
    let host = reg.host_face(false, 15);
    assert!(host.inlined);
    assert_eq!(
        host.names,
        vec!["choreo.read_file", "choreo.say_hello", "choreo.weekly"]
    );
    let desc = host.defs[1]["function"]["description"]
        .as_str()
        .unwrap_or("");
    assert_eq!(desc.chars().count(), rufous_kernel::mcp::HOST_DESC_CAP + 1);
    // The routed path lists only the configured server.
    let routed: Vec<String> = reg
        .tool_defs(&set(&["mcp"]), &set(&[]), false)
        .iter()
        .map(|d| d["function"]["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(routed.iter().all(|n| n.starts_with("notes.")), "{routed:?}");
    // Past the cap the names stay, the schemas go.
    let over = reg.host_face(false, 2);
    assert!(!over.inlined && over.defs.is_empty() && over.names.len() == 3);
    // Offline means this machine (docs/egress.md): the host's own loopback server stays, a
    // url host server elsewhere goes.
    assert_eq!(reg.host_face(true, 15).names.len(), 3);
    let mut far = McpConfig::new();
    far.insert(
        "choreo".into(),
        ServerConfig {
            host: true,
            ..cfg(json!({"url": "https://choreo.example.invalid/mcp"}))
        },
    );
    let p = Scripted::new(Some(vec![probed("say_hello", "s")]));
    let reg = McpRegistry::with_prober(d.path(), far, Box::new(Shared(p)));
    assert!(reg.host_face(true, 15).is_empty());
    assert!(!reg.host_face(false, 15).is_empty());
}

#[test]
fn a_host_tool_s_display_name_is_its_title_else_its_name_read_as_words() {
    let d = tempfile::tempdir().expect("tmp");
    let mut servers = McpConfig::new();
    servers.insert(
        "choreo".into(),
        ServerConfig {
            host: true,
            ..cfg(json!({"url": "http://127.0.0.1:9/mcp"}))
        },
    );
    let mut titled = probed("auto_7f3", "Weekly report");
    titled.title = Some("Weekly Report".into());
    let p = Scripted::new(Some(vec![probed("say_hello", "greets"), titled]));
    let reg = McpRegistry::with_prober(d.path(), servers, Box::new(Shared(p)));
    let host = reg.host_face(false, 15);
    assert_eq!(host.names, vec!["choreo.auto_7f3", "choreo.say_hello"]);
    assert_eq!(host.titles, vec!["Weekly Report", "say hello"]);
}
