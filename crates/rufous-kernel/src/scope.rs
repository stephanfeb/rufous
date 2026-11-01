//! The scope gate (bead boubou-6x3): what the user's instruction is *for*, and whether a
//! producing call leaves it.
//!
//! In a live editor session (acp_1031_160446, 2026-10-31) the instruction was to break the
//! specs into beads; after sixty tracker commands the model ran `bd ready`, claimed the one
//! item it saw and wrote `lib/src/key_pair_manager.dart`. Nothing in the harness represented
//! what the turn was for, so nothing could notice. The pull was the tracker's own workflow
//! text in the prompt. The earlier case (bead boubou-he2) was a one-line follow-up that
//! turned into implementing the next plan item.
//!
//! The gate derives a [`Class`] for each task turn from the user's text, heuristically
//! (nouns and verbs; the System One decider is the later home for the classification, in
//! shadow first as the question-or-task decision was). When the class is [`Class::Tracker`]
//! or [`Class::Docs`] and a call would write a source file or run a build, test or package
//! command, the call goes through the safety gate's ask ring with the action "outside the
//! instruction": the CLI asks, an editor shows its permission dialog, unattended denies.
//! Allow clears the gate for the rest of the turn; Deny returns [`deny_text`] to the model,
//! which quotes the instruction and asks for a report. Claiming a bead is not the drift,
//! the file write is, so tracker commands never fire. A code instruction is [`Class::Code`]
//! and never asks; anything unclear is [`Class::Mixed`] and never asks either. Nothing is
//! added to the prompt: the scope is harness state.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::text::head;
use crate::tools::ToolInvocation;

/// `config.json` `scope_gate`: `"on"` asks (default), `"shadow"` only logs what it would
/// have asked, `"off"` does nothing. The `scope_gate` ablation switch is `off` for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeGateMode {
    Off,
    Shadow,
    #[default]
    On,
}

/// What the instruction asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Issues, beads, tickets, epics: tracker work.
    Tracker,
    /// Specs, designs, documentation, plans, notes.
    Docs,
    /// Implementation: the gate never fires.
    Code,
    /// Unclear, a question, or code signals beside tracker or docs ones: never fires.
    Mixed,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Tracker => "tracker",
            Class::Docs => "docs",
            Class::Code => "code",
            Class::Mixed => "mixed",
        }
    }

    /// The classes a producing call can leave.
    pub fn guarded(self) -> bool {
        matches!(self, Class::Tracker | Class::Docs)
    }
}

/// The scope of one task turn: its class and the instruction it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub class: Class,
    pub instruction: String,
}

impl Scope {
    pub fn of(instruction: &str) -> Self {
        Self {
            class: classify(instruction),
            instruction: instruction.to_string(),
        }
    }

    /// One log line: `class=tracker guarded (break the specs into beads…)`.
    pub fn describe(&self) -> String {
        format!(
            "class={} {} ({})",
            self.class.name(),
            if self.class.guarded() {
                "guarded"
            } else {
                "open"
            },
            head(self.instruction.trim(), 80).replace('\n', " ")
        )
    }
}

static WORD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-z][a-z0-9_+#.-]*").expect("word regex"));
/// A path with a source extension anywhere in the text (`lib/x.dart`, `parser.py`).
static SOURCE_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b[\w./-]+\.(rs|py|dart|js|jsx|ts|tsx|mjs|cjs|go|java|kt|kts|swift|c|cc|cpp|cxx|h|hpp|cs|rb|php|sh|bash|zsh|sql|scala|ex|exs|erl|hs|lua|pl|r|m|mm|vue|svelte)\b",
    )
    .expect("source path regex")
});

/// Tracker words: the instruction is about the issue tracker.
const TRACKER: &[&str] = &[
    "bead",
    "beads",
    "bd",
    "issue",
    "issues",
    "ticket",
    "tickets",
    "epic",
    "epics",
    "backlog",
    "milestone",
    "milestones",
    "story",
    "stories",
    "subtask",
    "subtasks",
    "triage",
    "close",
    "closed",
    "reprioritise",
    "reprioritize",
    "unblock",
    "blocker",
    "blockers",
];
/// Docs words: the instruction is about writing or revising a document.
const DOCS: &[&str] = &[
    "spec",
    "specs",
    "specification",
    "specifications",
    "openspec",
    "design",
    "designs",
    "document",
    "documents",
    "documentation",
    "docs",
    "readme",
    "write-up",
    "writeup",
    "notes",
    "proposal",
    "proposals",
    "adr",
    "rfc",
    "plan",
    "roadmap",
    "outline",
    "summary",
    "summarise",
    "summarize",
    "changelog",
    "guide",
    "architecture",
];
/// Code words: the instruction wants implementation; the gate stands down.
const CODE: &[&str] = &[
    "implement",
    "implementation",
    "fix",
    "fixes",
    "bug",
    "bugs",
    "debug",
    "refactor",
    "compile",
    "compiles",
    "build",
    "builds",
    "test",
    "tests",
    "unit",
    "function",
    "functions",
    "method",
    "methods",
    "class",
    "classes",
    "struct",
    "module",
    "modules",
    "endpoint",
    "endpoints",
    "api",
    "library",
    "dependency",
    "dependencies",
    "package",
    "packages",
    "install",
    "code",
    "coding",
    "script",
    "scripts",
    "program",
    "lint",
    "typecheck",
    "migrate",
    "migration",
    "wire",
    "integrate",
    "integration",
    "crash",
    "crashes",
    "error",
    "errors",
    "exception",
    "stack",
    "regression",
    "widget",
    "component",
    "service",
    "parser",
    "handler",
    "schema",
    "query",
    "cli",
    "binary",
    "crate",
    "crates",
    "src",
    "lib",
];

fn words(text: &str) -> Vec<String> {
    let low = text.to_lowercase();
    WORD_RE
        .find_iter(&low)
        .map(|m| m.as_str().trim_end_matches('.').to_string())
        .collect()
}

/// A negation within this many words before a code word cancels it ("do not implement
/// anything yet", bead boubou-gqw). Apostrophes split words, so "don't" is `don`, `t`.
const NEGATION_WINDOW: usize = 3;
const NEGATORS: &[&str] = &[
    "not", "don", "dont", "doesn", "won", "didn", "never", "without", "no",
];
/// A snake_case identifier (`strip_accents`, `slug_path`) names code; a file name with an
/// extension (`boubou_convo_perm.txt`) does not.
static IDENTIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9]*(?:_[a-z0-9]+)+$").expect("identifier regex"));
/// A tasks file: `tasks.md`, `todo.md`, `backlog.md`.
const TASKS_FILES: &[&str] = &["tasks.md", "todo.md", "backlog.md", "todo.txt", "tasks.txt"];

/// A code word that no negation precedes within [`NEGATION_WINDOW`] words.
fn code_word_at(ws: &[String], i: usize) -> bool {
    if !CODE.contains(&ws[i].as_str()) {
        return false;
    }
    let from = i.saturating_sub(NEGATION_WINDOW);
    !ws[from..i].iter().any(|w| NEGATORS.contains(&w.as_str()))
}

/// The class of an instruction (bead boubou-6x3; the rules of bead boubou-gqw are each
/// justified by a line of `bench/convo/scope_labels.jsonl`). Code signals, a code word not
/// negated within three words before it, a snake_case identifier or a source path, win or
/// neutralise: with any present the class is [`Class::Code`] (no tracker or docs signal) or
/// [`Class::Mixed`] (both kinds), so a tracker or docs instruction that mentions a bug or a
/// file is left alone. Without them tracker signals (a tracker word; `task`/`tasks` when a
/// tasks file, a `.md` file or another tracker word is named) make [`Class::Tracker`], docs
/// signals (a docs word, a `.md` file) [`Class::Docs`] (tracker wins when both appear:
/// "break the specs into beads" is tracker work), and neither is [`Class::Mixed`].
pub fn classify(text: &str) -> Class {
    let ws = words(text);
    let has = |list: &[&str]| ws.iter().any(|w| list.contains(&w.as_str()));
    let md_file = ws.iter().any(|w| w.ends_with(".md") && w.len() > 3);
    let tasks_file = ws.iter().any(|w| TASKS_FILES.contains(&w.as_str()));
    let identifier = ws.iter().any(|w| IDENTIFIER_RE.is_match(w));
    let code =
        (0..ws.len()).any(|i| code_word_at(&ws, i)) || identifier || SOURCE_PATH_RE.is_match(text);
    let tracker_word = has(TRACKER);
    let task_word = ws.iter().any(|w| w == "task" || w == "tasks");
    let tracker = tracker_word || (task_word && (tasks_file || md_file));
    let docs = has(DOCS) || md_file;
    match (code, tracker || docs) {
        (true, true) => Class::Mixed,
        (true, false) => Class::Code,
        (false, true) => {
            if tracker {
                Class::Tracker
            } else {
                Class::Docs
            }
        }
        (false, false) => Class::Mixed,
    }
}

/// Source extensions a write counts as code (documents, data and config files do not).
pub const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "dart", "js", "jsx", "ts", "tsx", "mjs", "cjs", "go", "java", "kt", "kts", "swift",
    "c", "cc", "cpp", "cxx", "h", "hpp", "cs", "rb", "php", "sh", "bash", "zsh", "sql", "scala",
    "ex", "exs", "erl", "hs", "lua", "pl", "r", "m", "mm", "vue", "svelte",
];

/// Build, test and package tools: running one is implementation work.
pub const BUILD_TOOLS: &[&str] = &[
    "cargo",
    "rustc",
    "npm",
    "npx",
    "yarn",
    "pnpm",
    "bun",
    "deno",
    "node",
    "tsc",
    "pip",
    "pip3",
    "poetry",
    "uv",
    "pytest",
    "flutter",
    "dart",
    "go",
    "make",
    "cmake",
    "ninja",
    "gradle",
    "gradlew",
    "mvn",
    "dotnet",
    "swift",
    "xcodebuild",
    "gcc",
    "g++",
    "clang",
    "clang++",
    "javac",
    "mix",
    "stack",
    "cabal",
    "zig",
];

/// Whether `path` names a source file.
pub fn is_source_path(path: &str) -> bool {
    std::path::Path::new(path.trim())
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            let e = e.to_lowercase();
            SOURCE_EXTENSIONS.contains(&e.as_str())
        })
}

/// The first word of each simple command in a shell line (`cd x && cargo test | tee` gives
/// `cd`, `cargo`, `tee`), lowercased, path prefix dropped (`./gradlew` → `gradlew`).
fn command_heads(command: &str) -> Vec<String> {
    command
        .split(['\n', ';', '|', '&'])
        .filter_map(|seg| {
            seg.trim()
                .trim_start_matches('(')
                .split_whitespace()
                .find(|w| !w.contains('='))
                .map(|w| w.rsplit('/').next().unwrap_or(w).to_lowercase())
        })
        .collect()
}

/// Whether a shell command is implementation work: a build, test or package tool, or a
/// redirect into a source file (`cat > lib/x.dart`, `>> main.py`).
pub fn is_build_or_source_write(command: &str) -> bool {
    let heads = command_heads(command);
    if heads.iter().any(|h| {
        BUILD_TOOLS.contains(&h.as_str())
            || (h == "python" || h == "python3") && command.contains("-m pytest")
    }) {
        return true;
    }
    static REDIRECT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r">\s*>?\s*([\w./-]+)").expect("redirect regex"));
    REDIRECT_RE
        .captures_iter(command)
        .any(|c| is_source_path(&c[1]))
}

/// Why a call leaves `scope`, if it does: "writes code" or "runs a build or test command".
pub fn out_of_scope(scope: &Scope, inv: &ToolInvocation) -> Option<&'static str> {
    if !scope.class.guarded() {
        return None;
    }
    match inv {
        ToolInvocation::CreateFile { path, .. }
        | ToolInvocation::EditFile { path, .. }
        | ToolInvocation::AppendFile { path, .. }
            if is_source_path(path) =>
        {
            Some("writes code")
        }
        ToolInvocation::RunBash { command } if is_build_or_source_write(command) => {
            Some("runs a build or test command")
        }
        _ => None,
    }
}

/// What the model reads when the user, or unattended mode, denies the call.
pub fn deny_text(scope: &Scope, why: &str) -> String {
    format!(
        "[scope gate: the instruction was: \"{}\"; this call {why}, which it did not ask for. Report what is done and stop; the user will say what comes next.]",
        head(scope.instruction.trim(), 120).replace('\n', " ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(t: &str) -> Class {
        classify(t)
    }

    #[test]
    fn the_two_live_instructions_are_tracker_work() {
        assert_eq!(
            class("Let's now proceed to break down our specs into beads. The progression should be incremental, small slices and follow engineering best-practices. DO NOT e.g. try to map the spec documents one-to-one with a bead. Each spec will likely have several beads under it's epic."),
            Class::Tracker
        );
        assert_eq!(
            class("I think we already did that one. It can be closed."),
            Class::Tracker
        );
    }

    /// Every line of `bench/convo/scope_labels.jsonl` (the rig's instructions and the two
    /// live ones, labelled by hand) classes as labelled; a rule change that moves one must
    /// change its label and say why.
    #[test]
    fn every_labelled_instruction_classes_as_labelled() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bench/convo/scope_labels.jsonl");
        let text = std::fs::read_to_string(&path).expect("labels");
        let mut seen = 0;
        let mut wrong = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value = serde_json::from_str(line).expect("label line");
            let say = v["say"].as_str().expect("say");
            let want = v["class"].as_str().expect("class");
            seen += 1;
            let got = classify(say).name();
            if got != want {
                wrong.push(format!(
                    "{}: got {got}, labelled {want}: {}",
                    v["id"], v["why"]
                ));
            }
        }
        assert!(seen >= 25, "{seen} labels");
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn negation_identifiers_and_named_files_decide_the_edge_cases() {
        // A negated code word is no code signal; an unnegated one further away still is.
        assert_eq!(
            class("Do not implement anything yet; list the beads"),
            Class::Tracker
        );
        assert_eq!(class("Don't fix it, file an issue"), Class::Tracker);
        assert_eq!(class("Fix it, and do not forget the issue"), Class::Mixed);
        // snake_case names code; a file name with an extension does not.
        assert_eq!(class("Rename strip_accents everywhere"), Class::Code);
        assert_eq!(
            class("Write hello to /tmp/boubou_convo_perm.txt"),
            Class::Mixed
        );
        // task/tasks is tracker only beside a tasks file, a .md file or a tracker word.
        assert_eq!(class("Tick the first task in tasks.md"), Class::Tracker);
        assert_eq!(class("Which task first? Just tell me"), Class::Mixed);
        assert_eq!(class("Add three tasks to the backlog"), Class::Tracker);
        // A .md file is a docs signal; 'feature' is not a code word.
        assert_eq!(class("Create report.md with the title line"), Class::Docs);
        assert_eq!(class("Add tasks for feature 5 to tasks.md"), Class::Tracker);
        assert_eq!(class("Implement feature 5"), Class::Code);
    }

    #[test]
    fn docs_code_and_mixed_read_as_expected() {
        assert_eq!(
            class("Write the architecture spec for the sync layer in openspec/"),
            Class::Docs
        );
        assert_eq!(class("Fix the bug in the parser"), Class::Code);
        assert_eq!(
            class("Implement KeyPairManager in lib/src/key_pair_manager.dart"),
            Class::Code
        );
        assert_eq!(class("Create an issue for the login bug"), Class::Mixed);
        assert_eq!(
            class("Update the README after you fix the build"),
            Class::Mixed
        );
        assert_eq!(class("hello, what can you do?"), Class::Mixed);
        assert_eq!(
            class("Add a unit test for slugify in tests/test_slug.py"),
            Class::Code
        );
    }

    #[test]
    fn source_paths_and_build_commands_are_recognised() {
        assert!(is_source_path("lib/src/key_pair_manager.dart"));
        assert!(is_source_path("/abs/path/main.RS"));
        assert!(!is_source_path("openspec/architecture.md"));
        assert!(!is_source_path("config.json"));
        assert!(!is_source_path("notes"));
        assert!(is_build_or_source_write("cd /p && flutter pub get"));
        assert!(is_build_or_source_write("cargo test -p rufous-kernel"));
        assert!(is_build_or_source_write("python -m pytest -q"));
        assert!(is_build_or_source_write("./gradlew assembleDebug"));
        assert!(is_build_or_source_write(
            "cat > lib/content/store.dart << 'EOF'\nclass X {}\nEOF"
        ));
        assert!(!is_build_or_source_write(
            "bd create --title=\"Setup host\" --description=\"...\""
        ));
        assert!(!is_build_or_source_write("bd update overshare-ar0 --claim"));
        assert!(!is_build_or_source_write("mkdir -p lib/content"));
        assert!(!is_build_or_source_write(
            "cat openspec/architecture.md | grep -E '^## '"
        ));
        assert!(!is_build_or_source_write("echo notes > plan.md"));
        assert!(!is_build_or_source_write("git status"));
    }

    #[test]
    fn only_guarded_scopes_see_drift_and_only_on_code() {
        let tracker = Scope::of("break the specs into beads");
        let dart = ToolInvocation::CreateFile {
            path: "lib/src/key_pair_manager.dart".into(),
            content: String::new(),
            replace: false,
        };
        assert_eq!(out_of_scope(&tracker, &dart), Some("writes code"));
        let bd = ToolInvocation::RunBash {
            command: "bd create --title=\"x\"".into(),
        };
        assert_eq!(out_of_scope(&tracker, &bd), None);
        let md = ToolInvocation::CreateFile {
            path: "openspec/specs/a.md".into(),
            content: String::new(),
            replace: false,
        };
        assert_eq!(out_of_scope(&tracker, &md), None);
        let build = ToolInvocation::RunBash {
            command: "flutter pub get".into(),
        };
        assert_eq!(
            out_of_scope(&tracker, &build),
            Some("runs a build or test command")
        );
        let code = Scope::of("Implement the key pair manager");
        assert_eq!(out_of_scope(&code, &dart), None);
        let mixed = Scope::of("Create an issue for the login bug");
        assert_eq!(out_of_scope(&mixed, &dart), None);
        assert!(deny_text(&tracker, "writes code").starts_with(
            "[scope gate: the instruction was: \"break the specs into beads\"; this call writes code, which it did not ask for. Report what is done and stop"
        ));
    }

    /// Every bench task prompt (`bench/lrab_ext/tasks/*/*.json`) must be left alone: a task
    /// read as tracker or docs work may name no source file among its deliverables, so the
    /// gate cannot ask (or, unattended, refuse) on a cell's own work.
    #[test]
    fn the_bench_tasks_never_arm_the_gate_against_their_own_deliverables() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/lrab_ext/tasks");
        let mut seen = 0;
        let mut problems = Vec::new();
        for tier in std::fs::read_dir(&root).expect("bench tasks") {
            let tier = tier.expect("entry").path();
            if !tier.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(&tier).expect("tier") {
                let f = f.expect("entry").path();
                if f.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let v: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&f).expect("read"))
                        .expect("json");
                let prompt = v["prompt"].as_str().unwrap_or_default();
                seen += 1;
                let c = classify(prompt);
                if c.guarded() {
                    let names: Vec<&str> = v["final_artifacts"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                        .unwrap_or_default();
                    if names.iter().any(|n| is_source_path(n)) || SOURCE_PATH_RE.is_match(prompt) {
                        problems.push(format!(
                            "{}: {} with source deliverables {names:?}",
                            f.display(),
                            c.name()
                        ));
                    }
                }
            }
        }
        assert!(
            seen >= 10,
            "found only {seen} tasks under {}",
            root.display()
        );
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
