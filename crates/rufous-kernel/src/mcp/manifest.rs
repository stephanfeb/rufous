//! The probed tool manifest (`mcp_manifest`), its memory + disk cache, flattening into
//! tool definitions (`mcp_tool_defs`), and MCP argument path resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{call_tool, probe_server, McpConfig, ProbedTool, ServerConfig};
use crate::egress::{offline_refusal, EgressLog};
use crate::tools::{ADVANCED_TOOL_NAMES, CORE_TOOL_NAMES};

/// `_MCP_TTL`: probes spawn processes, so results live this long.
pub const MANIFEST_TTL: Duration = Duration::from_secs(300);
/// `_MCP_CACHE_FILE` under the agent home.
pub const CACHE_FILE: &str = "mcp_manifest_cache.json";
/// When any probe failed the cache expires after this instead, so the retry comes soon.
const FAILED_TTL: Duration = Duration::from_secs(60);
const MAX_PARALLEL_PROBES: usize = 8;
/// Description length of a routed MCP tool in its flattened definition.
pub const DESC_CAP: usize = 60;
/// Description length of a host tool: a host's descriptions say what an automation does and
/// what it needs, which 60 characters cut off (docs/host-tools.md).
pub const HOST_DESC_CAP: usize = 200;
/// Length of a parameter description kept in a compacted schema. Only host tools are sent
/// them (docs/mcp-schemas.md).
pub const PARAM_DESC_CAP: usize = 120;
/// Host tools inlined as schemas; past this the face carries `mcp_call` and the prompt names
/// the tools instead. `RUFOUS_HOST_TOOL_CAP` overrides it for the bench that sets it.
pub const HOST_TOOL_CAP: usize = 15;

/// The host tools of a session ([`McpRegistry::host_face`]): their qualified names, sorted,
/// and their schemas when they fit under the cap. Fixed for the session, so the tool list
/// stays the same from turn to turn (bead boubou-g11).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostFace {
    pub names: Vec<String>,
    /// Display names, one per entry of `names`: the MCP title, else the tool part with `_`
    /// read as a space (docs/host-tool-mentions.md).
    pub titles: Vec<String>,
    /// The flattened definitions; empty when `inlined` is false.
    pub defs: Vec<Value>,
    pub inlined: bool,
}

impl HostFace {
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }
}

/// The cap in effect: `RUFOUS_HOST_TOOL_CAP` when set to a number, else [`HOST_TOOL_CAP`].
pub fn host_tool_cap() -> usize {
    std::env::var("RUFOUS_HOST_TOOL_CAP")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(HOST_TOOL_CAP)
}

/// One flattened `server.tool` definition, its description on one line and cut to `cap`.
/// Parameter descriptions stay only for a host tool (`param_desc`, docs/mcp-schemas.md).
fn flat_def(server: &str, t: &ManifestTool, cap: usize, param_desc: bool) -> Value {
    let mut desc: String = t.desc.replace('\n', " ");
    if desc.chars().count() > cap {
        desc = format!("{}…", desc.chars().take(cap).collect::<String>());
    }
    if desc.is_empty() {
        desc = format!("{server} tool");
    }
    let mut params = t.schema.clone();
    if !param_desc {
        if let Some(props) = params.get_mut("properties").and_then(Value::as_object_mut) {
            for v in props.values_mut() {
                if let Some(o) = v.as_object_mut() {
                    o.remove("description");
                }
            }
        }
    }
    json!({"type": "function", "function": {
        "name": super::qualified_name(server, &t.name),
        "description": desc,
        "parameters": params,
    }})
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub desc: String,
    #[serde(default = "empty_schema")]
    pub schema: Value,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default = "auto")]
    pub expose: String,
}

fn auto() -> String {
    "auto".to_string()
}

fn empty_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ServerManifest {
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub tools: Vec<ManifestTool>,
}

/// `server → {categories, tools}`.
pub type Manifest = BTreeMap<String, ServerManifest>;

/// `_compact_schema`: per parameter its type, its `enum` and its description (one line, cut
/// to [`PARAM_DESC_CAP`]); `required` as the server sent it, a missing list meaning none.
/// With more than eight parameters only the required ones stay, when some are named
/// (prefill stays small). docs/mcp-schemas.md.
pub fn compact_schema(js: &Value) -> Value {
    let Some(obj) = js.as_object() else {
        return json!({"type": "object", "properties": {}, "required": []});
    };
    let mut props: Vec<(String, Value)> = Vec::new();
    if let Some(p) = obj.get("properties").and_then(Value::as_object) {
        for (k, v) in p {
            let Some(t) = v.get("type").filter(|t| !t.is_null()) else {
                continue;
            };
            let mut e = serde_json::Map::new();
            e.insert("type".into(), t.clone());
            if let Some(vals) = v.get("enum").and_then(Value::as_array) {
                if !vals.is_empty()
                    && vals
                        .iter()
                        .all(|x| x.is_string() || x.is_number() || x.is_boolean())
                {
                    e.insert("enum".into(), Value::Array(vals.clone()));
                }
            }
            if let Some(d) = v.get("description").and_then(Value::as_str) {
                let d = d.split_whitespace().collect::<Vec<_>>().join(" ");
                if !d.is_empty() {
                    let d = if d.chars().count() > PARAM_DESC_CAP {
                        format!("{}…", d.chars().take(PARAM_DESC_CAP).collect::<String>())
                    } else {
                        d
                    };
                    e.insert("description".into(), Value::String(d));
                }
            }
            props.push((k.clone(), Value::Object(e)));
        }
    }
    let req: Vec<String> = obj
        .get("required")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if props.len() > 8 && !req.is_empty() {
        props.retain(|(k, _)| req.contains(k));
    }
    let mut m = serde_json::Map::new();
    for (k, v) in props {
        m.insert(k, v);
    }
    json!({"type": "object", "properties": m, "required": req})
}

/// How the registry discovers a server's tools. The real one spawns/connects via `rmcp`;
/// tests script it.
pub trait Prober: Send + Sync {
    /// `None` = probe failed or timed out (distinct from `Some(vec![])` = no tools).
    fn probe(&self, cfg: &ServerConfig) -> Option<Vec<ProbedTool>>;
}

#[derive(Debug, Default)]
struct RmcpProber;

impl Prober for RmcpProber {
    fn probe(&self, cfg: &ServerConfig) -> Option<Vec<ProbedTool>> {
        probe_server(cfg, super::PROBE_TIMEOUT)
    }
}

/// A server's entry in the cache: the fingerprint it was probed under, and when.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Stamp {
    fp: String,
    ts: f64,
}

/// `<home>/mcp_manifest_cache.json`: Mingbird's `{ts, data}` plus a stamp per server
/// (docs/mcp-cache.md). A file without stamps reads as all stale.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DiskCache {
    ts: f64,
    data: Manifest,
    #[serde(default)]
    servers: BTreeMap<String, Stamp>,
}

/// Entries of servers no registry has probed for this long leave the cache file.
const PRUNE_AFTER: f64 = 86_400.0;

/// What a server's tools depend on that rufous can see: its configuration and, for a stdio
/// server, the size and modification time of its command binary. Files in `args` are left
/// out: bizframe's `--app` file changes with every edit (docs/mcp-cache.md).
fn fingerprint(cfg: &ServerConfig) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(cfg).unwrap_or_default().hash(&mut h);
    if let Some(meta) = cfg
        .command
        .as_deref()
        .and_then(command_path)
        .and_then(|p| std::fs::metadata(p).ok())
    {
        meta.len().hash(&mut h);
        if let Ok(t) = meta.modified() {
            t.hash(&mut h);
        }
    }
    format!("{:016x}", h.finish())
}

/// The binary a command names: the path itself when it has a separator, else the first
/// match on `PATH`.
fn command_path(cmd: &str) -> Option<PathBuf> {
    let p = Path::new(cmd);
    if cmd.is_empty() {
        return None;
    }
    if p.components().count() > 1 {
        return Some(p.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(cmd))
        .find(|c| c.is_file())
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Configured servers plus the cached manifest. Shared by the loop (tool definitions)
/// and the executor (calls); cheap to clone behind an `Arc`.
pub struct McpRegistry {
    pub servers: McpConfig,
    cache_file: PathBuf,
    mem: Mutex<Option<DiskCache>>,
    prober: Box<dyn Prober>,
    /// The `url` servers called or probed, for the egress report (docs/egress.md).
    pub egress: EgressLog,
    /// Offline: a `url` server on another machine is neither probed nor called.
    pub offline: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for McpRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRegistry")
            .field("servers", &self.servers.keys().collect::<Vec<_>>())
            .field("cache_file", &self.cache_file)
            .finish()
    }
}

impl McpRegistry {
    pub fn new(agent_home: &Path, servers: McpConfig) -> Self {
        Self::with_prober(agent_home, servers, Box::new(RmcpProber))
    }

    fn is_offline(&self) -> bool {
        self.offline.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn with_prober(agent_home: &Path, servers: McpConfig, prober: Box<dyn Prober>) -> Self {
        Self {
            servers,
            cache_file: agent_home.join(CACHE_FILE),
            mem: Mutex::new(None),
            prober,
            egress: EgressLog::default(),
            offline: Default::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// `is_mcp_tool`: `server.tool` with a configured server.
    pub fn is_mcp_tool(&self, name: &str) -> bool {
        super::split_qualified(name).is_some_and(|(s, _)| self.servers.contains_key(s))
    }

    /// Seconds-since-epoch of the oldest configured server's cache entry (tests check the
    /// shortened TTL).
    pub fn cache_ts(&self) -> Option<f64> {
        let g = self.mem.lock().ok()?;
        let c = g.as_ref()?;
        self.servers
            .keys()
            .filter_map(|n| c.servers.get(n).map(|s| s.ts))
            .reduce(f64::min)
    }

    fn load_disk(&self) -> Option<DiskCache> {
        let raw = std::fs::read_to_string(&self.cache_file).ok()?;
        let mut d: DiskCache = serde_json::from_str(&raw).ok()?;
        // A cache written with differently cased or aliased category names still routes.
        for m in d.data.values_mut() {
            m.categories = super::normalise_categories(&m.categories);
            for t in &mut m.tools {
                t.categories = super::normalise_categories(&t.categories);
            }
        }
        Some(d)
    }

    fn save_disk(&self, cache: &DiskCache) {
        if let Some(dir) = self.cache_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string(cache) {
            let _ = std::fs::write(&self.cache_file, s);
        }
    }

    /// `mcp_manifest(force)`: the configured servers' tools. A server whose cache stamp
    /// matches its fingerprint and is younger than the TTL is served from memory or disk;
    /// the others are probed in parallel (all of them with `force`). A failed probe keeps
    /// that server's previous tools (stale-while-revalidate) and is retried after
    /// [`FAILED_TTL`]. Only configured servers are returned (docs/mcp-cache.md).
    pub fn manifest(&self, force: bool) -> Manifest {
        let now = now_secs();
        let ttl = MANIFEST_TTL.as_secs_f64();
        let cand: Vec<(&String, &ServerConfig, String)> = self
            .servers
            .iter()
            .filter(|(_, c)| c.is_runnable())
            .map(|(n, c)| (n, c, fingerprint(c)))
            .collect();
        let fresh = |c: &DiskCache, name: &str, fp: &str| {
            c.data.contains_key(name)
                && c.servers
                    .get(name)
                    .is_some_and(|s| s.fp == fp && now - s.ts < ttl)
        };
        let serve = |c: &DiskCache| -> Manifest {
            cand.iter()
                .filter_map(|(n, _, _)| c.data.get(*n).map(|m| ((*n).clone(), m.clone())))
                .collect()
        };
        let mem = self.mem.lock().ok().and_then(|g| g.clone());
        if !force {
            if let Some(c) = &mem {
                if cand.iter().all(|(n, _, fp)| fresh(c, n, fp)) {
                    return serve(c);
                }
            }
        }
        // The disk may be newer than memory: another process probes into the same file.
        let disk = self.load_disk();
        if !force {
            if let Some(c) = &disk {
                if cand.iter().all(|(n, _, fp)| fresh(c, n, fp)) {
                    if let Ok(mut g) = self.mem.lock() {
                        *g = Some(c.clone());
                    }
                    return serve(c);
                }
            }
        }
        let mut cache = disk.or(mem).unwrap_or_default();
        let stale: Vec<&(&String, &ServerConfig, String)> = cand
            .iter()
            .filter(|(n, _, fp)| force || !fresh(&cache, n, fp))
            .collect();
        let mut raws: Vec<Option<Vec<ProbedTool>>> = Vec::with_capacity(stale.len());
        for chunk in stale.chunks(MAX_PARALLEL_PROBES) {
            let outs: Vec<Option<Vec<ProbedTool>>> = std::thread::scope(|s| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|(name, cfg, _)| {
                        // Offline, a server on another machine is not contacted: it counts as
                        // a failed probe, so its earlier tools are kept for an online session.
                        let skip = self.is_offline() && cfg.is_remote();
                        if let (Some(url), false) = (&cfg.url, skip) {
                            self.egress.record(url, &format!("mcp:{name}"));
                        }
                        let cfg: &ServerConfig = cfg;
                        s.spawn(move || if skip { None } else { self.prober.probe(cfg) })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap_or(None))
                    .collect()
            });
            raws.extend(outs);
        }
        for ((name, cfg, fp), raw) in stale.into_iter().zip(raws) {
            let (tools, ts): (Vec<ProbedTool>, f64) = match raw {
                Some(t) => (t, now),
                None => (
                    cache
                        .data
                        .get(*name)
                        .map(|m| {
                            m.tools
                                .iter()
                                .map(|t| ProbedTool {
                                    name: t.name.clone(),
                                    title: t.title.clone(),
                                    desc: t.desc.clone(),
                                    schema: t.schema.clone(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    now - ttl + FAILED_TTL.as_secs_f64(),
                ),
            };
            let tools = tools
                .into_iter()
                .map(|t| ManifestTool {
                    categories: cfg.categories_for(&t.name),
                    expose: cfg.expose_for(&t.name),
                    schema: if t.schema.is_object() {
                        t.schema
                    } else {
                        empty_schema()
                    },
                    name: t.name,
                    title: t.title,
                    desc: t.desc,
                })
                .collect();
            cache.data.insert(
                (*name).clone(),
                ServerManifest {
                    categories: cfg.server_categories(),
                    tools,
                },
            );
            cache
                .servers
                .insert((*name).clone(), Stamp { fp: fp.clone(), ts });
        }
        // Other registries' servers stay for them, up to a day; unstamped ones (a file in
        // the old shape) go unless configured here.
        let kept = |n: &String, c: &BTreeMap<String, Stamp>| {
            self.servers.contains_key(n) || c.get(n).is_some_and(|s| now - s.ts < PRUNE_AFTER)
        };
        let stamps = cache.servers.clone();
        cache.data.retain(|n, _| kept(n, &stamps));
        cache.servers.retain(|n, _| kept(n, &stamps));
        // Mingbird reads the top-level `ts`: the oldest stamp, so it probes when any is due.
        cache.ts = cache
            .servers
            .values()
            .map(|s| s.ts)
            .reduce(f64::min)
            .unwrap_or(now);
        if let Ok(mut g) = self.mem.lock() {
            *g = Some(cache.clone());
        }
        self.save_disk(&cache);
        serve(&cache)
    }

    /// `mcp_tool_defs(cats, extra)`: flattened `server.tool` definitions for the routed
    /// categories. `auto` tools ride with their category; `on-demand`/`disabled` only when
    /// `extra` names them; tools duplicating a built-in are skipped unless named; in
    /// offline mode every `url` server is skipped.
    pub fn tool_defs(
        &self,
        cats: &BTreeSet<String>,
        extra: &BTreeSet<String>,
        offline: bool,
    ) -> Vec<Value> {
        if self.servers.is_empty() {
            return Vec::new();
        }
        let manifest = self.manifest(false);
        let mut defs = Vec::new();
        for (server, info) in &manifest {
            if offline
                && self
                    .servers
                    .get(server)
                    .is_some_and(ServerConfig::is_remote)
            {
                continue;
            }
            // A host server's tools are assembled by `host_face`, every turn.
            if self.servers.get(server).is_some_and(|c| c.host) {
                continue;
            }
            for t in &info.tools {
                let named = extra.contains(&super::qualified_name(server, &t.name));
                if !named {
                    if !t.categories.iter().any(|c| cats.contains(c)) {
                        continue;
                    }
                    if t.expose != "auto" {
                        continue;
                    }
                    if is_builtin_name(&t.name) {
                        continue;
                    }
                    if t.categories.iter().any(|c| c == super::CAT_FILES)
                        && FILE_DUP.iter().any(|(mcp, builtin)| {
                            *mcp == t.name.as_str() && is_builtin_name(builtin)
                        })
                    {
                        continue;
                    }
                }
                defs.push(flat_def(server, t, DESC_CAP, false));
            }
        }
        defs
    }

    /// The session's host tools: every tool of every [`ServerConfig::host`] server, with no
    /// category, `expose` or built-in-duplicate filter (the host chose them), sorted by
    /// qualified name. Offline mode drops `url` servers here as in [`Self::tool_defs`].
    pub fn host_face(&self, offline: bool, cap: usize) -> HostFace {
        let hosts: Vec<&String> = self
            .servers
            .iter()
            .filter(|(_, c)| c.host && !(offline && c.is_remote()))
            .map(|(n, _)| n)
            .collect();
        if hosts.is_empty() {
            return HostFace::default();
        }
        let manifest = self.manifest(false);
        let mut tools: Vec<(String, String, Value)> = hosts
            .iter()
            .filter_map(|s| manifest.get(*s).map(|m| (*s, m)))
            .flat_map(|(s, m)| {
                m.tools.iter().map(|t| {
                    let title = t
                        .title
                        .clone()
                        .unwrap_or_else(|| t.name.replace(['_', '-'], " "));
                    (
                        super::qualified_name(s, &t.name),
                        title,
                        flat_def(s, t, HOST_DESC_CAP, true),
                    )
                })
            })
            .collect();
        tools.sort_by(|a, b| a.0.cmp(&b.0));
        let inlined = tools.len() <= cap;
        let mut face = HostFace {
            inlined,
            ..HostFace::default()
        };
        for (name, title, def) in tools {
            face.names.push(name);
            face.titles.push(title);
            if inlined {
                face.defs.push(def);
            }
        }
        face
    }

    /// Expected parameter names of a tool, from the manifest (for argument-error hints).
    pub fn param_names(&self, server: &str, tool: &str) -> Option<Vec<String>> {
        let m = self.manifest(false);
        let t = m.get(server)?.tools.iter().find(|t| t.name == tool)?;
        Some(
            t.schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default(),
        )
    }

    /// `mcp_call(server, tool, args)` as the dispatcher runs it: relative path arguments
    /// resolved against `workdir`, the sandbox hint appended when an MCP file server
    /// refuses a path.
    pub fn call(
        &self,
        server: &str,
        tool: &str,
        args: &serde_json::Map<String, Value>,
        workdir: &Path,
    ) -> String {
        let Some(cfg) = self.servers.get(server) else {
            return format!(
                "[MCP server '{server}' not configured. Edit {}: {{\"{server}\":{{\"command\":\"...\",\"args\":[...]}}}}]",
                self.cache_file
                    .with_file_name("mcp.json")
                    .display()
            );
        };
        if self.is_offline() && cfg.is_remote() {
            return offline_refusal(&format!("{server}.{tool}"));
        }
        let args = resolve_mcp_paths(args, workdir);
        if let Some(url) = &cfg.url {
            self.egress.record(url, &format!("mcp:{server}"));
        }
        let mut res = call_tool(cfg, tool, args, super::CALL_TIMEOUT);
        if res.starts_with("[mcp_call error:") && !res.contains("timed out") {
            if let Some(names) = self.param_names(server, tool) {
                let names = if names.is_empty() {
                    "(no parameters)".to_string()
                } else {
                    names.join(", ")
                };
                res = format!("{res} The tool {tool} expects the parameters: {names}");
            }
        }
        let low = res.to_lowercase();
        if [
            "access denied",
            "outside allowed",
            "not in allowed",
            "outside the allowed",
            "permission denied",
        ]
        .iter()
        .any(|k| low.contains(k))
        {
            res.push_str(
                "\n[hint: this is the MCP file server's own sandbox directory. To read or write \
                 files in the agent's working directory use the built-in \
                 create_file/read_file/edit_file/list_dir; the MCP file tools only work \
                 inside the root directory they were configured with.]",
            );
        }
        res
    }
}

fn is_builtin_name(name: &str) -> bool {
    CORE_TOOL_NAMES.contains(&name) || ADVANCED_TOOL_NAMES.contains(&name)
}

/// `_FILE_DUP`: MCP file verbs the built-ins already cover (the built-ins know the workdir).
const FILE_DUP: [(&str, &str); 12] = [
    ("write_file", "create_file"),
    ("create_file", "create_file"),
    ("read_file", "read_file"),
    ("read_text_file", "read_file"),
    ("list_directory", "list_dir"),
    ("directory_tree", "list_dir"),
    ("list_allowed_directories", "list_dir"),
    ("search_files", "search_files"),
    ("edit_file", "edit_file"),
    ("delete_file", "delete_file"),
    ("move_file", "create_file"),
    ("append_file", "append_file"),
];

const PATH_ARGS: [&str; 17] = [
    "path",
    "filepath",
    "file",
    "dir",
    "folder",
    "filename",
    "directory",
    "output",
    "out",
    "dest",
    "destination",
    "doc",
    "src",
    "source",
    "target",
    "input",
    "file_path",
];

/// `_is_path_key`: an argument name that carries a filesystem path.
pub fn is_path_key(k: &str) -> bool {
    let kl = k.to_lowercase();
    PATH_ARGS.contains(&kl.as_str())
        || [
            "_path",
            "_file",
            "_dir",
            "_folder",
            "_filename",
            "_pathname",
        ]
        .iter()
        .any(|s| kl.ends_with(s))
}

fn is_abs(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && matches!(b.get(2), Some(b'/') | Some(b'\\')))
        || p.starts_with('/')
        || p.starts_with('\\')
        || ["http://", "https://", "file://", "data:"]
            .iter()
            .any(|s| p.starts_with(s))
}

/// `os.path.normpath(os.path.join(workdir, v))`: lexical, no filesystem access.
fn normjoin(workdir: &Path, rel: &str) -> PathBuf {
    let mut out = PathBuf::from(workdir);
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out
}

/// `_resolve_mcp_paths`: relative path-like arguments → absolute under `workdir`; URLs,
/// absolute paths and non-path keys untouched.
pub fn resolve_mcp_paths(
    args: &serde_json::Map<String, Value>,
    workdir: &Path,
) -> serde_json::Map<String, Value> {
    let mut out = args.clone();
    for (k, v) in out.iter_mut() {
        let Some(s) = v.as_str() else { continue };
        if s.is_empty() || !is_path_key(k) {
            continue;
        }
        let t = s.trim().trim_matches(['"', '\'']);
        if t.is_empty() || is_abs(t) {
            continue;
        }
        let tl = t.to_lowercase();
        if t.contains("://")
            || tl.starts_with("mailto:")
            || tl.starts_with("tel:")
            || t.starts_with('#')
        {
            continue;
        }
        *v = Value::String(normjoin(workdir, t).to_string_lossy().into_owned());
    }
    out
}
