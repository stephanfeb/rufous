//! The tool face: which tool schemas the model sees on a given turn.
//!
//! Mingbird keeps this in module globals (`_active_tools`, `_disabled_tools`) that
//! `tools_for_categories` rebuilds every turn from category routing plus whatever the model
//! asked for through `enable_tools`. Disabled tools are filtered at assembly *and* refused at
//! call time, otherwise the per-turn rebuild would quietly resurrect them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use serde_json::Value;

use crate::mcp::{HostFace, CAT_CODE, CAT_FILES, CAT_MCP, CAT_MEMORY, CAT_WEB};
use crate::tools::{advanced_tools, core_tools, CORE_TOOL_NAMES};

/// Tools every task gets regardless of category.
pub const BASE_TOOLS: [&str; 6] = [
    "read_file",
    "list_dir",
    "todo",
    "skills",
    "enable_tools",
    "finish",
];

/// Category → tools. The `files` and `code` entries are always offered (see
/// `tools_for_categories`): nothing else can unlock `run_bash` or `create_file` later, and
/// whether a task needs to compute or write something is not a property of its domain. The
/// `web`, `memory` and `mcp` entries, and MCP servers tagged with any category, follow the
/// routing. (`run_python` dropped in rufous.)
pub const CATEGORY_TOOLS: [(&str, &[&str]); 5] = [
    (
        CAT_FILES,
        &[
            "create_file",
            "edit_file",
            "append_file",
            "delete_file",
            "search_files",
        ],
    ),
    (CAT_CODE, &["run_bash"]),
    (
        CAT_WEB,
        &["web_search", "web_fetch", "web_search_multi", "batch_tools"],
    ),
    (CAT_MEMORY, &["memory_store", "memory_recall"]),
    (CAT_MCP, &["mcp_call"]),
];

/// Keyword routing. Matched case-insensitively as substrings of the task text, so a
/// keyword also matches its inflections (`debug` hits `debugging`). The code list is
/// programming vocabulary only: everyday words that used to be on it (`fix`, `test`, `error`,
/// `calculate`, `run `, `function`) labelled ordinary tasks as coding work, and the harness
/// should not lean towards any one domain. `program ` keeps the UK `programme` out.
const CAT_KEYWORDS: [(&str, &[&str]); 5] = [
    (
        CAT_CODE,
        &[
            "code",
            "coding",
            "program ",
            "programs",
            "programming",
            "script",
            "bash",
            "shell",
            "terminal",
            "command line",
            "python",
            "rust",
            "javascript",
            "typescript",
            "java",
            "sql",
            "compile",
            "refactor",
            "debug",
            "pytest",
            "unit test",
            "traceback",
            "exception",
            "stack trace",
            "repository",
            "git ",
        ],
    ),
    (
        CAT_FILES,
        &[
            "create",
            "make ",
            "generate",
            "document",
            "file",
            "folder",
            "directory",
            "tidy",
            "organis",
            "organiz",
            "rename",
            "delete",
            "remove",
            "edit",
            "write",
            "save",
            "report",
            "markdown",
            "csv",
            "json",
        ],
    ),
    (
        CAT_WEB,
        &[
            "search", "look up", "research", "web", "internet", "online", "website", "page", "url",
            "http", "news", "weather", "download", "fetch", "latest",
        ],
    ),
    (
        CAT_MEMORY,
        &[
            "remember",
            "recall",
            "memory",
            "memorise",
            "memorize",
            "archive",
            "earlier",
            "last time",
            "previously",
        ],
    ),
    (
        CAT_MCP,
        &[
            "mcp",
            "external tool",
            "tool server",
            "plugin",
            "server",
            "api",
            "database",
        ],
    ),
];

/// Tools that reach the network; hidden in offline mode.
pub const ONLINE_TOOLS: [&str; 4] = ["web_search", "web_fetch", "web_search_multi", "batch_tools"];
/// The question route's tools (`_chat_tool_defs`): the read-only set plus `run_bash`, because
/// a question about the state of a repo, a backlog or a build is a command by nature
/// (`git status`, `bd ready`, `flutter pub get`), and a model with no shell on a question turn
/// invents one: web_search used as a shell, 8 times in one turn (bead boubou-o5a,
/// 2026-10-29). The safety gate still vets every command; the file mutators stay blocked
/// ([`QA_BLOCKED`]).
pub const CHAT_TOOLS: [&str; 5] = [
    "read_file",
    "list_dir",
    "web_search",
    "web_fetch",
    "run_bash",
];
/// `_RESEARCH_ALL`: research tools covered by the fingerprint-level streak guard.
pub const RESEARCH_ALL: [&str; 3] = ["web_search", "web_fetch", "web_search_multi"];
/// `_PRODUCTIVE`: tools that count as making progress (todo/research counters reset).
pub const PRODUCTIVE: [&str; 5] = [
    "create_file",
    "edit_file",
    "append_file",
    "finish",
    "run_bash",
];
/// `_MUTATE`: the tools that change the workspace (bookkeeping: what counts as a mutation).
pub const MUTATE: [&str; 5] = [
    "create_file",
    "edit_file",
    "append_file",
    "delete_file",
    "run_bash",
];
/// The tools the Q&A guard blocks on a question turn: the file mutators. `run_bash` is not
/// among them (see [`CHAT_TOOLS`]); the safety gate is its guard.
pub const QA_BLOCKED: [&str; 4] = ["create_file", "edit_file", "append_file", "delete_file"];
/// `_PRODUCING_PREFIX`: an MCP tool whose name starts with one of these did real work.
pub const PRODUCING_PREFIX: [&str; 12] = [
    "create_",
    "write_",
    "render_",
    "export_",
    "update_",
    "save_",
    "generate_",
    "build_",
    "insert_",
    "convert_",
    "import_",
    "add_",
];

/// `_is_mcp_producing`: `server.tool` whose tool part looks like it writes something.
pub fn is_mcp_producing(name: &str) -> bool {
    let tool = name.split_once('.').map(|(_, t)| t).unwrap_or("");
    PRODUCING_PREFIX.iter().any(|p| tool.starts_with(p))
}

/// `route_categories`: task text → categories. Code implies files; nothing matched means
/// the wide default (files + code + web) — better too many tools than too few.
pub fn route_categories(text: &str) -> BTreeSet<String> {
    let low = text.to_lowercase();
    let mut cats = BTreeSet::new();
    for (cat, kws) in CAT_KEYWORDS {
        if kws.iter().any(|k| low.contains(k)) {
            cats.insert(cat.to_string());
        }
    }
    if cats.contains(CAT_CODE) {
        cats.insert(CAT_FILES.to_string());
    }
    if cats.is_empty() {
        for c in [CAT_FILES, CAT_CODE, CAT_WEB] {
            cats.insert(c.to_string());
        }
    }
    cats
}

/// A session's categories: the union of [`route_categories`] over its instructions, each
/// routed as it would be alone (default included). A follow-up can add a category but never
/// drop one, so the tool set sent, which templates render ahead of the history, stays the
/// same from turn to turn and the server keeps its prefix cache (bead boubou-g11).
pub fn session_categories<S: AsRef<str>>(instructions: &[S]) -> BTreeSet<String> {
    instructions
        .iter()
        .flat_map(|t| route_categories(t.as_ref()))
        .collect()
}

/// The prompt line for a session with host tools (docs/host-tools.md). A small model follows
/// the text it is given, not the state behind it, so the text names the tools; `None` when
/// the session has none.
pub fn host_note(host: &HostFace) -> Option<String> {
    if host.is_empty() {
        return None;
    }
    let names = host
        .names
        .iter()
        .zip(&host.titles)
        .map(|(n, t)| format!("{n} ({t})"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(if host.inlined {
        format!(
            "HOST TOOLS: the app you are running in gave you its own tools: {names}. They are in your tool list: call them directly (no enable_tools) whenever the user asks for one of the app's automations, by its name or in their own words, or asks what one returned. Report what the tool returned."
        )
    } else {
        format!(
            "HOST TOOLS: the app you are running in has its own tools: {names}. To run one, call mcp_call with server and tool (the parts before and after the dot) and its arguments, whenever the user asks for one of the app's automations, by its name or in their own words, or asks what one returned."
        )
    })
}

/// Lower-case words joined by single spaces: `Say_Hello!` and `say hello` read the same.
fn words(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The host tools an instruction names (bead boubou-8c4, docs/host-tool-mentions.md):
/// `(qualified name, display name)` for each whose display name of two words or more, or
/// whose qualified name, occurs in `text` on word boundaries, ignoring case and punctuation.
/// A one-word display name ("backup", "hello") is an everyday word, never a mention alone.
pub fn host_mentions<'a>(host: &'a HostFace, text: &str) -> Vec<(&'a str, &'a str)> {
    let hay = format!(" {} ", words(text));
    host.names
        .iter()
        .zip(&host.titles)
        .filter(|(name, title)| {
            let t = words(title);
            (t.contains(' ') && hay.contains(&format!(" {t} ")))
                || hay.contains(&format!(" {} ", words(name)))
        })
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect()
}

/// The mention note (bead boubou-8c4, arm A, kept on the 2026-11-05 bench): the named
/// automation is a tool the model can call now. There is no hold behind it: arm B turned
/// "Don't run Weekly Report" into a run 5/6 on qwen3.5:4b.
pub fn host_mention_note(mentions: &[(&str, &str)]) -> String {
    let named = mentions
        .iter()
        .map(|(n, t)| format!("\"{t}\" is the app's automation {n}"))
        .collect::<Vec<_>>()
        .join("; ");
    format!("Hint: {named}. If the user wants it run or wants its result, call it; it runs now.")
}

fn schema_map() -> &'static BTreeMap<String, Value> {
    static MAP: LazyLock<BTreeMap<String, Value>> = LazyLock::new(|| {
        core_tools()
            .into_iter()
            .chain(advanced_tools())
            .filter_map(|t| {
                let name = t["function"]["name"].as_str()?.to_string();
                Some((name, t))
            })
            .collect()
    });
    &MAP
}

/// Schema of one built-in tool by name.
pub fn schema_of(name: &str) -> Option<&'static Value> {
    schema_map().get(name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFace {
    /// `_active_tools`, in enable order (core tools first).
    pub active: Vec<String>,
    /// `_disabled_tools`: refused at call time and never re-assembled.
    pub disabled: BTreeSet<String>,
    /// Names the model asked for through `enable_tools` (`allowed_extra`).
    pub extra: BTreeSet<String>,
    /// Offline mode hides the online tools from every schema list.
    pub offline: bool,
}

impl Default for ToolFace {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolFace {
    pub fn new() -> Self {
        Self {
            active: CORE_TOOL_NAMES.iter().map(|s| s.to_string()).collect(),
            disabled: BTreeSet::new(),
            extra: BTreeSet::new(),
            offline: false,
        }
    }

    pub fn is_active(&self, name: &str) -> bool {
        self.active.iter().any(|n| n == name)
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.contains(name)
    }

    /// Child mode (`install_child_mode`): only `allowed` stays; every other built-in tool is
    /// disabled at assembly and refused at call time.
    pub fn narrow_to(&mut self, allowed: &[String]) {
        let keep = |n: &str| allowed.iter().any(|a| a == n);
        self.disabled = crate::tools::CORE_TOOL_NAMES
            .iter()
            .chain(crate::tools::ADVANCED_TOOL_NAMES.iter())
            .filter(|n| !keep(n))
            .map(|n| n.to_string())
            .collect();
        self.active.retain(|n| keep(n));
        self.extra.clear();
    }

    /// Persistently disable a tool (anti-loop escalation).
    pub fn disable(&mut self, name: &str) {
        self.disabled.insert(name.to_string());
        self.active.retain(|n| n != name);
    }

    /// `enable_tools` by the model: request it for the next assembly and, as Mingbird
    /// promises in its ban message, unlock a disabled tool.
    pub fn enable(&mut self, name: &str) {
        self.extra.insert(name.to_string());
        self.disabled.remove(name);
    }

    fn filter_offline(&self, defs: Vec<Value>) -> Vec<Value> {
        if !self.offline {
            return defs;
        }
        defs.into_iter()
            .filter(|t| {
                let n = t["function"]["name"].as_str().unwrap_or("");
                !ONLINE_TOOLS.contains(&n)
            })
            .collect()
    }

    /// `_chat_tool_defs`: the question route's tools ([`CHAT_TOOLS`]). Whatever is offered here is also
    /// activated: the call-time gate refuses any tool missing from `active`, and a question
    /// turn that listed web_search but refused the call was how the model came to tell the
    /// user it "cannot browse" (web UI session of 2026-10-28).
    pub fn chat_tool_defs(&mut self) -> Vec<Value> {
        for n in CHAT_TOOLS {
            if !self.disabled.contains(n) && !self.is_active(n) {
                self.active.push(n.to_string());
            }
        }
        let defs = CHAT_TOOLS
            .iter()
            .filter(|n| !self.disabled.contains(**n))
            .filter_map(|n| schema_of(n).cloned())
            .collect();
        self.filter_offline(defs)
    }

    /// `tools_for_categories`: base tools + the routed categories + `extra`, minus
    /// disabled. Newly needed tools are appended to `active`; the result keeps active order.
    pub fn tools_for_categories(&mut self, cats: &BTreeSet<String>) -> Vec<Value> {
        let mut names: BTreeSet<String> = BASE_TOOLS.iter().map(|s| s.to_string()).collect();
        for (cat, tools) in CATEGORY_TOOLS {
            if cats.contains(cat) || cat == CAT_FILES || cat == CAT_CODE {
                names.extend(tools.iter().map(|s| s.to_string()));
            }
        }
        names.extend(self.extra.iter().cloned());
        for d in &self.disabled {
            names.remove(d);
        }
        for n in &names {
            if schema_of(n).is_some() && !self.is_active(n) {
                self.active.push(n.clone());
            }
        }
        let defs = self
            .active
            .iter()
            .filter(|n| names.contains(*n))
            .filter_map(|n| schema_of(n).cloned())
            .collect();
        self.filter_offline(defs)
    }

    /// Host tools past the cap: `mcp_call` on every route, activated so the call-time gate
    /// lets it through; nothing when `defs` already carry it or it is disabled.
    pub fn host_fallback(&mut self, defs: &[Value]) -> Vec<Value> {
        const NAME: &str = "mcp_call";
        if self.disabled.contains(NAME) || defs.iter().any(|d| d["function"]["name"] == NAME) {
            return Vec::new();
        }
        if !self.is_active(NAME) {
            self.active.push(NAME.to_string());
        }
        schema_of(NAME).cloned().into_iter().collect()
    }

    /// `active_tool_defs`: everything currently active (the flat-prefill fallback). A
    /// disabled tool is refused at call time, so it is not advertised here either: the
    /// model is never shown a tool the loop will then punish it for calling.
    pub fn active_defs(&self) -> Vec<Value> {
        let defs = self
            .active
            .iter()
            .filter(|n| !self.disabled.contains(*n))
            .filter_map(|n| schema_of(n).cloned())
            .collect();
        self.filter_offline(defs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(defs: &[Value]) -> Vec<String> {
        defs.iter()
            .filter_map(|d| d["function"]["name"].as_str().map(String::from))
            .collect()
    }

    #[test]
    fn host_mentions_need_the_whole_display_name_or_the_qualified_name() {
        let host = HostFace {
            names: vec![
                "choreo.backup".into(),
                "choreo.say_hello".into(),
                "choreo.weekly_report".into(),
            ],
            titles: vec!["backup".into(), "Say Hello".into(), "Weekly Report".into()],
            defs: Vec::new(),
            inlined: true,
        };
        let hit = |t: &str| -> Vec<String> {
            host_mentions(&host, t)
                .iter()
                .map(|(n, _)| n.to_string())
                .collect()
        };
        assert_eq!(
            hit("What did Say Hello return for Ada?"),
            vec!["choreo.say_hello"]
        );
        assert_eq!(
            hit("run choreo.weekly_report please"),
            vec!["choreo.weekly_report"]
        );
        assert_eq!(hit("Has weekly-report run?"), vec!["choreo.weekly_report"]);
        assert!(hit("Hello! Say hi to the team").is_empty());
        assert!(
            hit("make a backup of my notes").is_empty(),
            "one-word title"
        );
        assert!(hit("say hellos").is_empty(), "word boundary");
    }

    #[test]
    fn a_disabled_tool_is_advertised_on_no_path() {
        let mut face = ToolFace::new();
        let cats: BTreeSet<String> = [CAT_FILES.to_string()].into_iter().collect();
        assert!(names(&face.tools_for_categories(&cats)).contains(&"edit_file".to_string()));
        assert!(names(&face.active_defs()).contains(&"edit_file".to_string()));
        face.disable("edit_file");
        assert!(!names(&face.tools_for_categories(&cats)).contains(&"edit_file".to_string()));
        assert!(!names(&face.active_defs()).contains(&"edit_file".to_string()));
        assert!(face.is_disabled("edit_file"));
    }
}
