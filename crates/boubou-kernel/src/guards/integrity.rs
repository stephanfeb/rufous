//! Integrity gate: a finish must not claim what this turn's own evidence contradicts.
//!
//! Boubou's departure from Mingbird's finish ladder, driven by the 2026-10-28 probes
//! (`bench/probes.py`) and the LRAB WF-04 cell:
//! - a finish said "check.py printed OK" with no check.py run in the turn, or with the last run
//!   exited non-zero, and the pre-delivery self-check let it through;
//! - a finish said "Task complete" while a file the task named (fix_summary.md) was never
//!   written; the artefact gate only looks at files the *summary* names;
//! - an honest "the download failed, dataset.csv could not be produced" was rejected six times
//!   because the file name appeared in the summary and the plan.
//!
//! Lexical checks only: LLM judges do not detect false success (Advani, arXiv:2606.09863).

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::artifacts::{self, missing_files_in_text};

/// Words that mark a clause as reporting a failure rather than claiming a result.
pub const FAILURE_WORDS: [&str; 22] = [
    "fail",
    "could not",
    "couldn't",
    "cannot",
    "can't",
    "unable",
    "not possible",
    "did not",
    "didn't",
    "missing",
    "unavailable",
    "unreachable",
    "error",
    "incorrect",
    "not completed",
    "incomplete",
    "blocked",
    "refused",
    "aborted",
    "gave up",
    "not produced",
    "not created",
];

/// Files a task text may name that are never deliverables.
const NOT_DELIVERABLE: [&str; 3] = ["task_input.txt", "plan.md", "todo.json"];

static VERIFIED_CLAIM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
          printed\s+ok\b | prints\s+ok\b | output(?:s|ted)?\s+ok\b | returned\s+ok\b
        | \btests?\s+(?:are\s+|is\s+|all\s+|now\s+)*(?:pass(?:ed|es|ing)?|green|succeed(?:ed|s)?)\b
        | \ball\s+(?:\d+\s+)?tests?\s+(?:pass|passed|green)\b
        | \b\d+\s+passed\b
        | \bcheck(?:s|ed)?\s+(?:passed|passes|ok|succeeded)\b
        | \bexit\s+code\s+0\b | \bexited\s+(?:with\s+)?0\b
        | \bran\s+successfully\b | \bexecuted\s+successfully\b
        | \bverified\s+(?:by\s+running|with|via|using)\b",
    )
    .expect("regex")
});

/// A file name that reads as a test or check script (program or interpreter argument).
static TEST_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:test[_.-]?[\w-]*|[\w-]*[_.-]tests?|tests?|check[\w-]*|[\w-]*[_.-]check|verify[\w-]*|validate[\w-]*|lint[\w-]*|run[_-]?tests?[\w-]*)(?:\.(?:py|js|ts|mjs|cjs|rb|pl|sh|bash|zsh|dart|php|ps1|bat|cmd|exe))?$",
    )
    .expect("regex")
});

/// Programs that verify by being run at all.
const VERIFIER_PROGRAMS: &[&str] = &[
    "pytest",
    "py.test",
    "nosetests",
    "nose2",
    "tox",
    "nox",
    "jest",
    "vitest",
    "mocha",
    "ava",
    "rspec",
    "phpunit",
    "ctest",
    "gotestsum",
    "flake8",
    "pylint",
    "mypy",
    "pyright",
    "eslint",
    "shellcheck",
];
/// Tools that verify under one of these subcommands (`cargo build` is not a check).
const VERIFIER_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("cargo", &["test", "check", "clippy", "nextest"]),
    ("go", &["test", "vet"]),
    ("npm", &["test", "t"]),
    ("yarn", &["test"]),
    ("pnpm", &["test"]),
    ("bun", &["test"]),
    ("deno", &["test", "lint", "check"]),
    ("make", &["test", "check", "lint", "verify", "validate"]),
    ("dart", &["test", "analyze"]),
    ("flutter", &["test", "analyze"]),
    ("mvn", &["test", "verify"]),
    ("gradle", &["test", "check"]),
    ("gradlew", &["test", "check"]),
    ("dotnet", &["test"]),
    ("swift", &["test"]),
    ("mix", &["test"]),
    ("rake", &["test", "spec"]),
    ("ruff", &["check"]),
];
/// Package managers whose named script verifies: `npm run lint`, `yarn test:unit`.
const SCRIPT_RUNNERS: &[&str] = &["npm", "yarn", "pnpm", "bun"];
const VERIFY_SCRIPT_PREFIXES: &[&str] = &[
    "test",
    "lint",
    "check",
    "typecheck",
    "verify",
    "validate",
    "ci",
];
/// Interpreters: the script or module they run decides.
const INTERPRETERS: &[&str] = &[
    "python", "python3", "python2", "py", "node", "ruby", "perl", "sh", "bash", "zsh", "dart",
    "php", "deno", "bun",
];
const PY_VERIFY_MODULES: &[&str] = &[
    "pytest", "unittest", "nose", "nose2", "doctest", "mypy", "flake8", "pylint", "ruff", "tox",
    "pyright",
];
/// Words that run the program after them, plus their option words.
const RUN_WRAPPERS: &[&str] = &[
    "sudo", "env", "time", "timeout", "nice", "nohup", "npx", "bunx", "xvfb-run",
];
const RUN_PREFIX_PAIRS: &[(&str, &str)] = &[
    ("uv", "run"),
    ("poetry", "run"),
    ("pipenv", "run"),
    ("pdm", "run"),
    ("hatch", "run"),
    ("bundle", "exec"),
];

static EXIT_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[exit (-?\d+)\]").expect("regex"));

/// What kind of action a record is: a shell command, or a file-writing tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Shell,
    FileWrite,
}

/// One action this turn that could have changed the workdir or verified it: a `run_bash`
/// with its exit code, or a file-writing tool call. `exit` is `None` when the action never
/// ran (safety gate, duplicate block, argument error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRecord {
    pub kind: ActionKind,
    pub command: String,
    pub exit: Option<i32>,
}

impl CommandRecord {
    pub fn from_result(command: &str, result: &str) -> Self {
        Self {
            kind: ActionKind::Shell,
            command: command.to_string(),
            exit: exit_code_of(result),
        }
    }

    /// A `create_file` / `edit_file` / `append_file` / `delete_file` call. `landed` is the
    /// executor's word ([`crate::exec::ToolStatus::Ok`]): until 2026-10-31 (bead
    /// boubou-bj4) this read four result openings and missed the scope gate's and the
    /// blocked-path ones, so a refused write counted as a landed one here.
    pub fn file_write(tool: &str, path: &str, landed: bool) -> Self {
        Self {
            kind: ActionKind::FileWrite,
            command: format!("{tool} {path}"),
            exit: landed.then_some(0),
        }
    }

    pub fn succeeded(&self) -> bool {
        self.exit == Some(0)
    }

    pub fn is_verification(&self) -> bool {
        self.kind == ActionKind::Shell && is_verification_command(&self.command)
    }

    /// Could this action have changed a file? File writes that ran, and shell commands that
    /// ran and are not plainly read-only (`cat`, `ls`, `grep` … with no redirect).
    pub fn mutates(&self) -> bool {
        match self.kind {
            ActionKind::FileWrite => self.exit.is_some(),
            ActionKind::Shell => self.exit.is_some() && !is_read_only_command(&self.command),
        }
    }
}

const READ_ONLY_CMDS: &[&str] = &[
    "cat", "ls", "head", "tail", "grep", "rg", "wc", "find", "echo", "printf", "pwd", "stat",
    "diff", "which", "type", "file", "du", "df", "tree", "sort", "uniq", "cut", "md5sum", "shasum",
    // Filters and navigation a reading session uses; `sed -i`, `sort -o` and `awk -i` are
    // caught below, a redirect or a command substitution above.
    "sed", "awk", "gawk", "jq", "cd", "less", "more", "nl", "tr", "column", "comm", "cmp",
    "basename", "dirname", "realpath", "readlink", "date", "whoami", "hostname", "uname", "id",
    "test", "[", "true", "false", "xxd", "od", "hexdump", "strings", "env", "printenv",
];
/// Read-only subcommands of tools that also mutate (`bd done`, `git commit` are not here).
const READ_ONLY_SUBCOMMANDS: [(&str, &[&str]); 2] = [
    (
        "bd",
        &[
            "show", "list", "ready", "blocked", "stats", "search", "memories", "stale", "orphans",
            "dep", "doctor",
        ],
    ),
    (
        "git",
        &[
            "status",
            "log",
            "diff",
            "show",
            "branch",
            "blame",
            "ls-files",
            "rev-parse",
            "describe",
        ],
    ),
];

/// A command that only reads: every `;`/`&&`/`|`-separated part starts with a read-only
/// program (or a read-only subcommand of `bd` / `git`, or `xargs` / `env` feeding one),
/// nothing is redirected to a file and nothing runs inside `$(…)` or backticks (`echo
/// $(rm -rf build)` is a delete). Redirecting stderr to /dev/null or to stdout is not a write.
pub fn is_read_only_command(command: &str) -> bool {
    let c = command
        .replace("2>/dev/null", "")
        .replace("2> /dev/null", "")
        .replace("2>&1", "")
        .replace(">/dev/null", "")
        .replace("> /dev/null", "");
    let c = c.trim();
    if c.is_empty() || c.contains('>') || c.contains("$(") || c.contains('`') {
        return false;
    }
    c.split(['\n', ';', '|', '&'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .all(read_only_part)
}

fn read_only_part(part: &str) -> bool {
    let p = part.strip_prefix("sudo ").unwrap_or(part);
    let mut words = p.split_whitespace();
    let prog = words.next().unwrap_or("");
    let prog = prog.rsplit('/').next().unwrap_or(prog);
    if prog == "xargs" {
        // `xargs grep -l x`: the program it feeds decides (flags such as -0 skipped).
        let rest: Vec<&str> = words.skip_while(|w| w.starts_with('-')).collect();
        return !rest.is_empty() && read_only_part(&rest.join(" "));
    }
    if prog == "env" {
        // `env` alone (or piped) lists variables; `env FOO=1 cmd` runs cmd, which decides.
        let rest: Vec<&str> = words
            .skip_while(|w| w.starts_with('-') || w.contains('='))
            .collect();
        return rest.is_empty() || read_only_part(&rest.join(" "));
    }
    if READ_ONLY_CMDS.contains(&prog) {
        let args: Vec<&str> = words.collect();
        return match prog {
            // `find … -delete` / `-exec cmd` act; everything else find does is a read.
            "find" => !args
                .iter()
                .any(|w| matches!(*w, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir")),
            // `sed -i`, `-i.bak`, `-ni`, `--in-place` rewrite the file in place.
            "sed" => !args.iter().any(|w| {
                w.starts_with("--in-place")
                    || (w.starts_with('-') && !w.starts_with("--") && w[1..].contains('i'))
            }),
            "sort" => !args.iter().any(|w| *w == "-o" || w.starts_with("--output")),
            "awk" | "gawk" => !args
                .iter()
                .any(|w| w.starts_with("-i") || w.starts_with("--in-place")),
            _ => true,
        };
    }
    if let Some((_, subs)) = READ_ONLY_SUBCOMMANDS.iter().find(|(t, _)| *t == prog) {
        // Global options precede the subcommand; `-C <dir>` and `-c <key=val>` take a value.
        let mut sub = "";
        while let Some(w) = words.next() {
            if w == "-C" || w == "-c" {
                words.next();
            } else if !w.starts_with('-') {
                sub = w;
                break;
            }
        }
        if !subs.contains(&sub) {
            return false;
        }
        return !(prog == "git" && sub == "branch") || git_branch_lists(words);
    }
    false
}

/// `git branch` lists unless it is asked to create, delete, move, copy or retarget a branch:
/// a bare name creates, `-d`/`-D`/`-m`/`-M`/`-c`/`-C`/`-u`/`-f` act. Listing flags that take
/// a value (`--merged main`, `--contains x`) keep their argument, and a name after `--list`
/// is a pattern.
fn git_branch_lists<'a>(args: impl Iterator<Item = &'a str>) -> bool {
    const VALUE_FLAGS: [&str; 6] = [
        "--contains",
        "--no-contains",
        "--merged",
        "--no-merged",
        "--points-at",
        "--sort",
    ];
    const LIST_FLAGS: [&str; 11] = [
        "-a",
        "-r",
        "-v",
        "-vv",
        "--all",
        "--remotes",
        "--verbose",
        "--show-current",
        "--color",
        "--no-color",
        "--no-abbrev",
    ];
    let mut listing = false;
    let mut expect_value = false;
    for w in args {
        if expect_value {
            expect_value = false;
            continue;
        }
        if VALUE_FLAGS.contains(&w) {
            expect_value = true;
            continue;
        }
        if w == "--list" || w == "-l" {
            listing = true;
            continue;
        }
        if LIST_FLAGS.contains(&w)
            || VALUE_FLAGS.iter().any(|f| w.starts_with(&format!("{f}=")))
            || w.starts_with("--format=")
            || w.starts_with("--color=")
        {
            continue;
        }
        if w.starts_with('-') || !listing {
            return false;
        }
    }
    true
}

/// The exit code a `run_bash` result reports (`[exit N]` first), else `None`.
pub fn exit_code_of(result: &str) -> Option<i32> {
    EXIT_CODE
        .captures(result.trim_start())
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

/// Does the command look like it verifies something (tests, checks, lint, `python check.py`)?
pub fn is_verification_command(command: &str) -> bool {
    command.split(['\n', ';', '&', '|']).any(segment_verifies)
}

/// May the harness re-execute this verification command at finish? Every segment must run
/// a check or only read, and nothing may be redirected to a file: `pytest && git commit`
/// would commit twice, `pytest > out.txt` would write. Stderr to stdout or to /dev/null is
/// not a write.
pub fn rerunnable_verification(command: &str) -> bool {
    let stripped = command
        .replace("2>&1", "")
        .replace("2>/dev/null", "")
        .replace(">/dev/null", "");
    if stripped.contains('>') {
        return false;
    }
    stripped
        .split(['\n', ';', '&', '|'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .all(|s| segment_verifies(s) || is_read_only_command(s))
}

/// Does one shell segment *run* a check? The program in program position (after
/// `VAR=x`, `sudo`, `timeout 5`, `uv run`, …) must be a test runner, a tool under a verifying
/// subcommand, an interpreter given a test-named script or module, or a test-named script
/// executed directly. A word alone proves nothing: the previous rule, `\b(test|check|…)`
/// with no trailing boundary, took `cat test_parse.py` for a passed test and
/// `git checkout -b fix` (exit 128) for a failed one (review 2026-10-29).
fn segment_verifies(seg: &str) -> bool {
    let words: Vec<&str> = seg
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    let mut i = 0;
    while i < words.len() && words[i].contains('=') && !words[i].starts_with('-') {
        i += 1;
    }
    loop {
        let Some(w) = words.get(i) else {
            return false;
        };
        let b = base_of(w);
        if RUN_WRAPPERS.contains(&b.as_str()) {
            i += 1;
            while let Some(a) = words.get(i) {
                if a.starts_with('-')
                    || a.starts_with(|c: char| c.is_ascii_digit())
                    || a.contains('=')
                {
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        if let Some(a) = words.get(i + 1) {
            if RUN_PREFIX_PAIRS.contains(&(b.as_str(), *a)) {
                i += 2;
                continue;
            }
        }
        break;
    }
    let Some(prog) = words.get(i) else {
        return false;
    };
    let base = base_of(prog);
    let args = &words[i + 1..];
    if VERIFIER_PROGRAMS.contains(&base.as_str()) {
        return true;
    }
    if let Some((_, subs)) = VERIFIER_SUBCOMMANDS.iter().find(|(p, _)| *p == base) {
        let sub = args
            .iter()
            .find(|a| !a.starts_with('-'))
            .copied()
            .unwrap_or("");
        if subs.contains(&sub) {
            return true;
        }
        if SCRIPT_RUNNERS.contains(&base.as_str()) {
            let script = if sub == "run" || sub == "run-script" {
                args.iter()
                    .filter(|a| !a.starts_with('-'))
                    .nth(1)
                    .copied()
                    .unwrap_or("")
            } else {
                sub
            };
            let script = script.to_lowercase();
            return VERIFY_SCRIPT_PREFIXES.iter().any(|p| script.starts_with(p));
        }
    }
    if base == "tsc" {
        return args.iter().any(|a| a.eq_ignore_ascii_case("--noemit"));
    }
    if INTERPRETERS.contains(&base.as_str()) || base.starts_with("python") {
        if let Some(k) = args.iter().position(|a| *a == "-m") {
            return args
                .get(k + 1)
                .is_some_and(|m| PY_VERIFY_MODULES.contains(&m.to_lowercase().as_str()));
        }
        if args.iter().any(|a| *a == "-c" || *a == "-e") {
            return false;
        }
        let script = args
            .iter()
            .find(|a| !a.starts_with('-'))
            .copied()
            .unwrap_or("");
        return path_is_test_like(script);
    }
    path_is_test_like(prog)
}

/// Lower-cased file name of a program, script word or path.
fn base_of(w: &str) -> String {
    let w = w.trim_end_matches('/');
    w.rsplit(['/', '\\']).next().unwrap_or(w).to_lowercase()
}

/// `check.py`, `test_x.py`, `run_tests.sh`, or any script under a `tests/` directory.
fn path_is_test_like(p: &str) -> bool {
    if p.is_empty() {
        return false;
    }
    if TEST_FILE.is_match(&base_of(p)) {
        return true;
    }
    let norm = p.replace('\\', "/");
    norm.split('/').rev().skip(1).any(|d| {
        matches!(
            d.to_lowercase().as_str(),
            "test" | "tests" | "spec" | "specs" | "__tests__"
        )
    })
}

/// Does the summary claim a verified outcome ("check.py printed OK", "all tests pass", …)?
pub fn claims_verified_outcome(summary: &str) -> bool {
    VERIFIED_CLAIM.is_match(summary)
}

static CLAUSE_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[.!?](?:\s+|$)|[;\n]").expect("regex"));
static LIST_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|\s)(\d{1,2})\.\s").expect("regex"));

/// Sentences/clauses; a full stop splits only before whitespace so `dataset.csv` stays whole,
/// and a numbered-list marker ("Steps failed: 1. Run fetch.py, 2. …") is not a sentence end.
fn clauses(text: &str) -> impl Iterator<Item = String> {
    let flat = LIST_MARKER.replace_all(text, "${1}${2}) ").into_owned();
    CLAUSE_END
        .split(&flat)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
}

fn has_failure_word(clause_lower: &str) -> bool {
    FAILURE_WORDS.iter().any(|w| clause_lower.contains(w))
}

/// Words of a reply clause on either side of a mention that an admission must fall within
/// (bead boubou-e9i): "currency conversion with error handling" sixteen words after
/// `tasks.md` is not an admission about it.
const ADMISSION_WINDOW: usize = 8;

/// Verbs that make a clause naming the file a claim of having done it.
static DONE_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(wrote|written|created|added|appended|updated|edited|ticked|marked|saved|produced|inserted|removed|fixed|replaced|done)\b",
    )
    .expect("regex")
});

/// The directory the instruction gave for `base` (`/tmp` for `/tmp/boubou_convo_perm.txt`,
/// `docs/` for `docs/report.md`), `None` when the name is bare. A rooted directory loses
/// its trailing slash so "the /tmp directory" mentions it; a relative one keeps it so
/// "the docs folder" does not.
pub fn dir_in_text(task_low: &str, base_low: &str) -> Option<String> {
    let is_path_char = |c: char| c.is_ascii_alphanumeric() || "_./~-".contains(c);
    for (pos, _) in task_low.match_indices(base_low) {
        let head = &task_low[..pos];
        let from = head
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_path_char(*c))
            .last()
            .map(|(i, _)| i)
            .unwrap_or(pos);
        let dir = &head[from..];
        if !dir.ends_with('/') || dir == "/" || dir == "./" {
            continue;
        }
        return Some(if dir.starts_with(['/', '~']) {
            dir.trim_end_matches('/').to_string()
        } else {
            dir.to_string()
        });
    }
    None
}

/// Byte spans of the whitespace-separated words of `s`.
fn word_spans(s: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(b)) => {
                spans.push((b, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(b) = start {
        spans.push((b, s.len()));
    }
    spans
}

/// Is there a failure word within [`ADMISSION_WINDOW`] words of the mention at byte `at`,
/// not counting the mentioning word itself (`error.log` is a name, not an admission)?
fn failure_near(low: &str, at: usize) -> bool {
    let spans = word_spans(low);
    let Some(idx) = spans.iter().rposition(|(b, _)| *b <= at) else {
        return false;
    };
    let lo = idx.saturating_sub(ADMISSION_WINDOW);
    let hi = (idx + ADMISSION_WINDOW).min(spans.len().saturating_sub(1));
    let around: Vec<&str> = (lo..=hi)
        .filter(|i| *i != idx)
        .map(|i| &low[spans[i].0..spans[i].1])
        .collect();
    has_failure_word(&around.join(" "))
}

/// Does `text` report failure about `name`? With an empty `name`, any clause with a failure
/// word will do. Otherwise a clause mentions the file by its name, by its stem as a whole
/// word ("the dataset download failed" for dataset.csv; stems under four characters are
/// too common), or by the directory the instruction gave for it (`dir`, "cannot write
/// files to the /tmp directory"). A mentioning clause admits when a failure word is within
/// [`ADMISSION_WINDOW`] words of the mention; it claims when it has none but carries a
/// done-word ("Done, tasks.md updated"); an offer or a conditional ("I can, however, write
/// it to … such as X") is neither. The reply admits when some clause admits and none claims
/// (bead boubou-e9i: "Done. I've broken the spec into tasks in tasks.md … with error
/// handling" used to admit, and "I cannot write to the /tmp directory. I can, however,
/// write it … such as …/boubou_convo_perm.txt" used to claim).
pub fn admits_failure_about_in(text: &str, name: &str, dir: Option<&str>) -> bool {
    let name = name.to_lowercase();
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(&name);
    let stem_re = (stem.len() >= 4 && stem != name)
        .then(|| Regex::new(&format!(r"\b{}\b", regex::escape(stem))).ok())
        .flatten();
    let dir = dir.map(str::to_lowercase).filter(|d| d.len() >= 3);
    let mut admitted = false;
    for c in clauses(text) {
        let low = c.to_lowercase();
        if name.is_empty() {
            if has_failure_word(&low) {
                return true;
            }
            continue;
        }
        let mut mentions: Vec<usize> = low.match_indices(name.as_str()).map(|(i, _)| i).collect();
        if let Some(re) = &stem_re {
            mentions.extend(re.find_iter(&low).map(|m| m.start()));
        }
        if let Some(d) = &dir {
            mentions.extend(low.match_indices(d.as_str()).map(|(i, _)| i));
        }
        if mentions.is_empty() {
            continue;
        }
        if mentions.iter().any(|&at| failure_near(&low, at)) {
            admitted = true;
        } else if DONE_WORD.is_match(&low) {
            return false;
        }
    }
    admitted
}

/// [`admits_failure_about_in`] for a bare name: the finish gates' summaries name files
/// without a directory of the instruction's to go by.
pub fn admits_failure_about(text: &str, name: &str) -> bool {
    admits_failure_about_in(text, name, None)
}

/// Words that introduce a file the task hands the agent: something to read, convert or edit.
const INPUT_CUES: &[&str] = &[
    "from",
    "in",
    "read",
    "reads",
    "reading",
    "parse",
    "parses",
    "parsing",
    "load",
    "loads",
    "loading",
    "using",
    "use",
    "uses",
    "given",
    "input",
    "inputs",
    "open",
    "opens",
    "existing",
    "based on",
    "according to",
    "convert",
    "converts",
    "process",
    "processes",
    "analyse",
    "analyze",
    "summarise",
    "summarize",
    "clean",
    "merge",
    "reconcile",
    "check",
    "fix",
    "update",
    "edit",
    "review",
    "refactor",
    "rename",
    "move",
    "copy",
    "delete",
    "run",
    "contents of",
    "content of",
    "data in",
    "the file",
];
/// Words that introduce a file the task asks for. Anywhere in the window they win, so
/// `write the totals in result.txt` is a deliverable despite the `in`.
const OUTPUT_CUES: &[&str] = &[
    "to",
    "into",
    "as",
    "create",
    "creates",
    "write",
    "writes",
    "save",
    "saves",
    "produce",
    "produces",
    "generate",
    "generates",
    "output",
    "outputs",
    "named",
    "called",
    "deliver",
    "export",
    "exports",
    "store",
    "result in",
    "results in",
    "report in",
    "new file",
];

/// The six words before position `pos`, padded with spaces for whole-cue matching.
fn window_before(task_low: &str, pos: usize) -> String {
    let words: Vec<&str> = task_low[..pos]
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| !w.is_empty())
        .collect();
    format!(" {} ", words[words.len().saturating_sub(6)..].join(" "))
}

/// Verbs that make the file they act on something the turn must write (bead boubou-78s).
static MUTATION_VERB: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(tick|mark|add|append|update|edit|change|modify|write|rewrite|insert|remove|delete|rename|fix|record|create|fill|replace|extend|correct|adjust|put|cross off|check off)(s|ed|ing)?\b",
    )
    .expect("regex")
});

/// Does the instruction ask for `file_low` to be written or changed? An output cue in the
/// six words before a mention ("to tasks.md", "into tasks.md"), or a mutation verb anywhere
/// in the instruction ("Tick the first task in tasks.md"). A question about the file
/// ("how many headings does report.md have") is neither.
pub fn names_as_write_target(task_low: &str, file_low: &str) -> bool {
    let mut seen = false;
    for (pos, _) in task_low.match_indices(file_low) {
        seen = true;
        let window = window_before(task_low, pos);
        if OUTPUT_CUES
            .iter()
            .any(|c| window.contains(&format!(" {c} ")))
        {
            return true;
        }
    }
    seen && MUTATION_VERB.is_match(task_low)
}

/// The paths of this turn's landed writes: file tools that landed, and the write targets
/// of shell commands that exited 0.
fn landed_write_paths(cmds: &[CommandRecord]) -> Vec<String> {
    cmds.iter()
        .filter(|c| c.succeeded())
        .flat_map(|c| match c.kind {
            ActionKind::FileWrite => c
                .command
                .split_once(' ')
                .map(|(_, path)| vec![path.to_string()])
                .unwrap_or_default(),
            ActionKind::Shell => crate::safety::write_targets(&c.command)
                .into_iter()
                .map(|(_, t)| t)
                .collect(),
        })
        .collect()
}

/// `path` relative to `workdir`, lexically normalised (`./openspec/../tasks.md` →
/// `tasks.md`); an absolute path is resolved through the file system (a landed write
/// exists) and made relative. `None` when it leaves the workdir.
fn relative_in(workdir: &Path, path: &str) -> Option<PathBuf> {
    let given = path.replace('\\', "/");
    let given = Path::new(&given);
    let rel = if given.is_absolute() {
        let real = given.canonicalize().ok()?;
        let base = workdir.canonicalize().ok()?;
        real.strip_prefix(&base).ok()?.to_path_buf()
    } else {
        given.to_path_buf()
    };
    let mut out = PathBuf::new();
    for c in rel.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(s) => out.push(s),
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// A file the instruction asks to write that no landed write of this turn reached
/// ([`task_named_unwritten`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unwritten {
    /// The name as the instruction gave it.
    pub name: String,
    /// Where this turn's landed writes of that name went instead, when the name is an
    /// existing file in the workdir (bead boubou-di6). Empty when nothing of the name was
    /// written at all.
    pub elsewhere: Vec<String>,
}

impl Unwritten {
    /// The hold note's line: the bare name, or where it resolves and where the write went.
    pub fn line(&self) -> String {
        if self.elsewhere.is_empty() {
            self.name.clone()
        } else {
            format!(
                "{} is ./{}; the write went to {}",
                self.name,
                self.name,
                self.elsewhere.join(", ")
            )
        }
    }
}

/// Gate 3c (bead boubou-78s): files the instruction asks to write or change
/// ([`names_as_write_target`]) that no landed write of this turn reached, unless the reply
/// reports failure about them. The plan tool is not a file: "the tasks into tasks.md" put
/// into `todo` and reported done is what this catches.
///
/// A bare name resolves the way the tools resolve it, against the workdir: when
/// `<workdir>/<name>` is a file, the write has to reach that file (bead boubou-di6:
/// qwen3.5:4b, told "tasks in tasks.md" with `./tasks.md` present, created
/// `openspec/tasks.md` beside the other path in the sentence and the basename rule let it
/// through). When no such file exists, a write of that name in any directory counts.
pub fn task_named_unwritten(
    task: &str,
    reply: &str,
    cmds: &[CommandRecord],
    workdir: &Path,
) -> Vec<Unwritten> {
    let task_low = task.to_lowercase();
    // A refusal phrased without the path ("cannot write files outside the workspace",
    // qwen3.5:4b, bead boubou-18l) admits failure about every named file: the hold is
    // for claims and silence. A reply that names some file keeps the per-file rule.
    let general_failure =
        artifacts::files_in_text(reply).is_empty() && admits_failure_about(reply, "");
    let written = landed_write_paths(cmds);
    artifacts::files_in_text(task)
        .into_iter()
        .filter_map(|f| {
            let low = f.to_lowercase();
            let base = base_of(&low);
            let dir = dir_in_text(&task_low, &base);
            if NOT_DELIVERABLE.contains(&base.as_str())
                || !names_as_write_target(&task_low, &low)
                || general_failure
                || admits_failure_about_in(reply, &base, dir.as_deref())
            {
                return None;
            }
            let same_name: Vec<&String> = written.iter().filter(|p| base_of(p) == base).collect();
            if workdir.join(&f).is_file() {
                let reached = same_name.iter().any(|p| {
                    relative_in(workdir, p)
                        .is_some_and(|r| r.to_string_lossy().to_lowercase() == low)
                });
                if reached {
                    return None;
                }
                let mut elsewhere: Vec<String> = same_name.iter().map(|p| p.to_string()).collect();
                elsewhere.sort();
                elsewhere.dedup();
                return Some(Unwritten { name: f, elsewhere });
            }
            same_name.is_empty().then(|| Unwritten {
                name: f,
                elsewhere: Vec::new(),
            })
        })
        .collect()
}

/// Does every mention of `file_low` in `task_low` read as an input? The six words before a
/// mention decide: an output cue among them makes it a deliverable, otherwise an input cue
/// makes it an input. No cue at all keeps the old reading (expected to exist at the end).
pub fn named_as_input(task_low: &str, file_low: &str) -> bool {
    let mut seen = false;
    for (pos, _) in task_low.match_indices(file_low) {
        seen = true;
        let window = window_before(task_low, pos);
        if OUTPUT_CUES
            .iter()
            .any(|c| window.contains(&format!(" {c} ")))
        {
            return false;
        }
        if !INPUT_CUES
            .iter()
            .any(|c| window.contains(&format!(" {c} ")))
        {
            return false;
        }
    }
    seen
}

/// Files the task text names (with an artefact extension) that are not in the workdir and
/// that the summary does not report as failed. Plan files, the prompt file and the task's
/// inputs ([`named_as_input`]) never count: a missing input is reported, not produced.
pub fn task_named_missing(task: &str, summary: &str, workdir: &Path) -> Vec<String> {
    let task_low = task.to_lowercase();
    missing_files_in_text(task, workdir)
        .into_iter()
        .filter(|f| {
            let base = f.rsplit(['/', '\\']).next().unwrap_or(f).to_lowercase();
            !NOT_DELIVERABLE.contains(&base.as_str())
                && !admits_failure_about(summary, &base)
                && !named_as_input(&task_low, &f.to_lowercase())
        })
        .collect()
}

/// A summary-vs-evidence contradiction about command results, as (log line, message to the
/// model), or `None` when the summary is consistent with what ran.
pub fn claim_contradiction(summary: &str, cmds: &[CommandRecord]) -> Option<(String, String)> {
    let last_verif = cmds.iter().rev().find(|c| c.is_verification());
    if claims_verified_outcome(summary) {
        // The state at finish time is what the claim is about: a check that passed earlier
        // and failed on the last run (the model broke the result afterwards, probes_integrity
        // t08) does not back "printed OK".
        let backed = last_verif.is_some_and(CommandRecord::succeeded);
        if backed {
            // A pass is evidence only for the state it saw: commands or file writes after
            // it may have changed the result (probes_integrity2 t05: check.py OK, then a
            // failing `awk … > result.txt` emptied the file, then "printed OK").
            let after = cmds
                .iter()
                .rposition(CommandRecord::is_verification)
                .map(|i| &cmds[i + 1..])
                .unwrap_or(&[]);
            let changed: Vec<&CommandRecord> = after.iter().filter(|c| c.mutates()).collect();
            if let (Some(v), Some(last)) = (last_verif, changed.last()) {
                let n = changed.len();
                return Some((
                    format!(
                        "claim check: `{}` passed but {n} action(s) that can change files ran after it (last: `{}`); finish rejected",
                        head(&v.command),
                        head(&last.command)
                    ),
                    format!(
                        "⚠️ Your finish was rejected: `{}` passed, but {n} action(s) that can change files ran after it (the last was `{}`), so the verified state may be gone. Re-run the verification now with run_bash and finish with what it actually printed; if it no longer passes, say so in the summary.",
                        head(&v.command),
                        head(&last.command)
                    ),
                ));
            }
            return None;
        }
        // Not backed: no verification ran, or the last one failed / never ran.
        let evidence = match last_verif {
            Some(c) => match c.exit {
                Some(n) => format!(
                    "the last verification command `{}` exited {n}",
                    head(&c.command)
                ),
                None => format!(
                    "the last verification command `{}` never ran (it was refused or malformed)",
                    head(&c.command)
                ),
            },
            None => "no verification command (tests, check script, lint) ran in this turn at all"
                .to_string(),
        };
        return Some((
                format!("claim check: summary claims a verified outcome but {evidence}; finish rejected"),
                format!(
                    "⚠️ Your finish was rejected: the summary claims a verified result, but {evidence}. Two options: (1) run the verification now with run_bash and finish with what it actually printed; (2) if it cannot pass, say so plainly in the summary (what failed and why) and finish. Never report a result you did not observe."
                ),
            ));
    }
    if let Some(c) = last_verif {
        if !c.succeeded() && !admits_failure_about(summary, "") {
            let how = match c.exit {
                Some(n) => format!("exited {n}"),
                None => "was refused and never ran".to_string(),
            };
            return Some((
                format!("claim check: last verification command `{}` {how} but the summary reports success; finish rejected", head(&c.command)),
                format!(
                    "⚠️ Your finish was rejected: your last verification command `{}` {how}, yet the summary reports success. Either fix the problem and rerun it until it passes, or state the failure in the summary (which command, what it printed), then finish.",
                    head(&c.command)
                ),
            ));
        }
    }
    None
}

/// Did a verification command pass with nothing that can change files run after it? Then
/// the deliverable is verified and neither plan bookkeeping (unticked todo items) nor the
/// pre-delivery self-check should stand between the model and the finish.
pub fn verified_at_end(cmds: &[CommandRecord]) -> bool {
    let Some(i) = cmds.iter().rposition(CommandRecord::is_verification) else {
        return false;
    };
    cmds[i].succeeded() && !cmds[i + 1..].iter().any(CommandRecord::mutates)
}

fn head(s: &str) -> String {
    let s = s.lines().next().unwrap_or("").trim();
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(60).collect::<String>())
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(cmd: &str, res: &str) -> CommandRecord {
        CommandRecord::from_result(cmd, res)
    }

    #[test]
    fn exit_codes_parse_from_result_heads() {
        assert_eq!(exit_code_of("[exit 0]\nSTDOUT:\nOK"), Some(0));
        assert_eq!(exit_code_of("[exit 127]\nSTDOUT:\n"), Some(127));
        assert_eq!(exit_code_of("[safety gate: refused]"), None);
        assert_eq!(
            exit_code_of("[tool error: run_bash missing argument]"),
            None
        );
    }

    #[test]
    fn verified_claims_and_verification_commands_are_recognised() {
        for s in [
            "check.py printed OK",
            "All 6 tests pass.",
            "tests are now green",
            "6 passed in 0.1s",
            "Verified by running pytest",
            "The script ran successfully",
        ] {
            assert!(claims_verified_outcome(s), "{s}");
        }
        for s in [
            "Wrote report.md and summary.md",
            "check.py printed FAIL",
            "Task complete.",
        ] {
            assert!(!claims_verified_outcome(s), "{s}");
        }
        assert!(is_verification_command("python check.py"));
        assert!(is_verification_command("python -m pytest -q"));
        assert!(!is_verification_command("python fetch.py"));
        assert!(!is_verification_command("ls -la"));
    }

    #[test]
    fn failure_admission_is_per_clause() {
        let s = "Download of dataset.csv failed due to a connection error. Wrote summary.md.";
        assert!(admits_failure_about(s, "dataset.csv"));
        assert!(!admits_failure_about(s, "summary.md"));
        assert!(
            !admits_failure_about(s, "other.md"),
            "unmentioned file is not excused"
        );
        assert!(admits_failure_about(s, ""));
        assert!(!admits_failure_about(
            "Created dataset.csv and summary.md.",
            "dataset.csv"
        ));
        // the stem counts as a mention (probes_integrity loop t03: "the dataset download
        // failed" was refused for not naming dataset.csv)
        assert!(admits_failure_about(
            "Could not complete the task as the dataset download failed (503).",
            "dataset.csv"
        ));
        assert!(!admits_failure_about(
            "Wrote the dataset to disk.",
            "dataset.csv"
        ));
        // probes_corrector2 t02: a numbered list is one clause, not three sentences
        assert!(admits_failure_about(
            "dataset.csv: could not be produced (503). Steps failed: 1. Run python fetch.py to download dataset.csv, 2. Count the rows in dataset.csv, 3. Write summary.md.",
            "dataset.csv"
        ));
        assert!(
            !admits_failure_about("The run failed.", "a.py"),
            "stems under 4 chars are ignored"
        );
        assert!(!admits_failure_about(
            "Successfully simulated the download of dataset.csv",
            ""
        ));
    }

    // Bead boubou-e9i: the two live replies of 4b_di6_v1 that the clause rule misread.
    #[test]
    fn an_admission_is_near_the_mention_and_a_claim_needs_a_done_word() {
        let qwen = "Done. I've broken the spec into 6 small tasks in `tasks.md`, each focusing on a single concern: CLI parsing, CSV validation with line numbers, currency conversion with error handling, monthly grouping, markdown output generation, and year filtering.";
        assert!(
            !admits_failure_about(qwen, "tasks.md"),
            "error handling, far away"
        );
        assert!(admits_failure_about(
            "tasks.md: error, not written.",
            "tasks.md"
        ));
        let gemma = "I cannot write files to the `/tmp` directory due to security restrictions. I can, however, write \"hello\" to a file within the current working directory, such as `/Users/x/work/boubou_convo_perm.txt`.";
        assert!(
            !admits_failure_about(gemma, "boubou_convo_perm.txt"),
            "bare: the directory is unknown"
        );
        assert!(admits_failure_about_in(
            gemma,
            "boubou_convo_perm.txt",
            Some("/tmp")
        ));
        // A claim beside an admission is still a claim.
        assert!(!admits_failure_about_in(
            "I cannot write to /tmp. I created boubou_convo_perm.txt in the workspace instead.",
            "boubou_convo_perm.txt",
            Some("/tmp")
        ));
        // The mentioning word is not its own admission.
        assert!(!admits_failure_about(
            "Wrote error.log with the trace.",
            "error.log"
        ));
        assert!(admits_failure_about(
            "Could not write error.log.",
            "error.log"
        ));
        // The instruction's directory.
        let low = |s: &str| s.to_lowercase();
        assert_eq!(
            dir_in_text(
                &low("Write hello to /tmp/boubou_convo_perm.txt."),
                "boubou_convo_perm.txt"
            )
            .as_deref(),
            Some("/tmp")
        );
        assert_eq!(
            dir_in_text(&low("Update docs/report.md."), "report.md").as_deref(),
            Some("docs/")
        );
        assert_eq!(dir_in_text(&low("Update ./report.md."), "report.md"), None);
        assert_eq!(dir_in_text(&low("Update tasks.md."), "tasks.md"), None);
        // Through gate 3c with the live instructions.
        let dir = tempfile::tempdir().expect("tempdir");
        let wd = dir.path();
        let t1 = "Break the spec in openspec/spec.md into small tasks in tasks.md, one `- [ ]` line each.";
        assert_eq!(
            task_named_unwritten(t1, qwen, &[], wd)
                .into_iter()
                .map(|u| u.name)
                .collect::<Vec<_>>(),
            ["tasks.md"]
        );
        let perm = "Write the single word hello to /tmp/boubou_convo_perm.txt.";
        let declined = vec![CommandRecord::file_write(
            "create_file",
            "/tmp/boubou_convo_perm.txt",
            false,
        )];
        assert!(task_named_unwritten(perm, gemma, &declined, wd).is_empty());
    }

    #[test]
    fn claim_without_a_passing_verification_is_rejected() {
        // probes_gatefix t02: "check.py printed Calculation complete." with zero check runs.
        let none: Vec<CommandRecord> = vec![rec("python -c 'print(1)'", "[exit 0]")];
        let (log, msg) = claim_contradiction("check.py printed OK", &none).expect("rejected");
        assert!(log.contains("no verification command"));
        assert!(msg.starts_with("⚠️") && msg.contains("Never report a result you did not observe"));
        // smoke trial: check.py ran but exited 127 every time.
        let failed = vec![rec("python check.py", "[exit 127]\nSTDOUT:\n")];
        let (log, _) = claim_contradiction("check.py printed OK", &failed).expect("rejected");
        assert!(log.contains("exited 127"));
        // refused by the safety gate: never ran.
        let refused = vec![rec("python check.py", "[safety gate: refused]")];
        assert!(claim_contradiction("tests pass", &refused)
            .expect("rejected")
            .0
            .contains("never ran"));
        // backed by a real pass: fine, even if an earlier run failed.
        let ok = vec![
            rec("python check.py", "[exit 1]"),
            rec("python check.py", "[exit 0]\nOK"),
        ];
        assert!(claim_contradiction("check.py printed OK", &ok).is_none());
        // passed earlier, then broken: the last run is what the claim is about.
        let broke = vec![
            rec("python check.py", "[exit 0]\nOK"),
            rec("python check.py", "[exit 1]"),
        ];
        assert!(claim_contradiction("check.py printed OK", &broke)
            .expect("rejected")
            .0
            .contains("exited 1"));
    }

    #[test]
    fn failed_verification_needs_an_honest_summary() {
        let cmds = vec![rec("python -m pytest -q", "[exit 1]\n1 failed")];
        assert!(
            claim_contradiction("Implemented the feature and wrote notes.md.", &cmds).is_some()
        );
        assert!(claim_contradiction(
            "pytest still fails on test_x; notes.md explains why.",
            &cmds
        )
        .is_none());
        // A failing non-verification command (ls on a missing dir) does not trip the gate.
        let cmds = vec![rec("ls missing_dir", "[exit 1]")];
        assert!(claim_contradiction("Wrote report.md.", &cmds).is_none());
        assert!(claim_contradiction("Wrote report.md.", &[]).is_none());
    }

    #[test]
    fn task_named_files_missing_unless_reported_failed() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::write(d.path().join("bug_report.md"), "x").expect("w");
        let task = "Write bug_report.md and fix_summary.md; plan in plan.md; input is data.csv.";
        // data.csv is named as an input: missing, it is reported, never demanded.
        assert_eq!(
            task_named_missing(task, "Task complete.", d.path()),
            vec!["fix_summary.md".to_string()]
        );
        let honest = "fix_summary.md could not be written: the fix is incomplete. data.csv was missing from the directory.";
        assert!(task_named_missing(task, honest, d.path()).is_empty());
    }

    #[test]
    fn verified_at_end_means_a_passing_check_with_no_file_changes_after_it() {
        assert!(verified_at_end(&[rec("python check.py", "[exit 0]\nOK")]));
        // read-only commands after the check keep it current …
        assert!(verified_at_end(&[
            rec("python check.py", "[exit 0]"),
            rec("cat result.txt | head -1", "[exit 0]")
        ]));
        // … anything that can write does not.
        assert!(!verified_at_end(&[
            rec("python check.py", "[exit 0]"),
            rec(
                "awk '{s+=$3} END {print s}' data.csv > result.txt",
                "[exit 2]"
            )
        ]));
        assert!(!verified_at_end(&[
            rec("python check.py", "[exit 0]"),
            CommandRecord::file_write("create_file", "result.txt", true)
        ]));
        // a refused write never ran, nor did a missed edit or one already in the file
        assert!(verified_at_end(&[
            rec("python check.py", "[exit 0]"),
            CommandRecord::file_write("delete_file", "/etc/x", false),
            CommandRecord::file_write("edit_file", "a.py", false),
            CommandRecord::file_write("edit_file", "a.py", false)
        ]));
        assert!(!verified_at_end(&[rec("python check.py", "[exit 1]")]));
        assert!(!verified_at_end(&[]));
    }

    #[test]
    fn a_pass_followed_by_file_changes_is_stale_evidence() {
        // probes_integrity2 t05: check.py OK at turn 21, failing awk redirects into
        // result.txt afterwards, finish says "printed OK" over an empty file.
        let cmds = vec![
            rec("python check.py", "[exit 0]\nOK"),
            rec(
                "awk -F, '$2==\"paid\"{s+=$3} END{print s}' data.csv > result.txt",
                "[exit 2]",
            ),
        ];
        let (log, msg) = claim_contradiction("check.py printed OK", &cmds).expect("stale");
        assert!(log.contains("passed but 1 action(s)"), "{log}");
        assert!(msg.contains("Re-run the verification"), "{msg}");
        // a file tool after the check is a change too
        let cmds = vec![
            rec("python check.py", "[exit 0]\nOK"),
            CommandRecord::file_write("edit_file", "result.txt", true),
        ];
        assert!(claim_contradiction("check.py printed OK", &cmds).is_some());
        // reading after the check is fine
        let cmds = vec![
            rec("python check.py", "[exit 0]\nOK"),
            rec("cat result.txt", "[exit 0]\n1234.56"),
        ];
        assert!(claim_contradiction("check.py printed OK", &cmds).is_none());
    }

    #[test]
    fn read_only_commands_are_recognised() {
        for c in [
            "cat result.txt",
            "ls -la",
            "grep -n paid data.csv | head",
            "sudo ls",
            "wc -l data.csv; cat result.txt",
            "find openspec -name \"*.md\" 2>/dev/null | sort",
            "find openspec -type f 2>/dev/null | xargs grep -l \"bitswap\"",
            "ls lib 2>&1",
            "bd show --current",
            "bd list",
            "git status --short && git log --oneline -3",
            // filters and navigation (the old list bounced an honest finish on these)
            "sed -n '1,40p' lib/main.dart",
            "sed -e 's/a/b/' f | head",
            "jq .name package.json",
            "awk '{print}' f",
            "awk -F, '{print $2}' data.csv | sort | uniq -c",
            "cd src && cat f",
            "tr -d '\\r' < f | wc -l",
            "env",
            "env | grep -i path",
            "git branch",
            "git branch -a -v",
            "git branch --list 'feat/*'",
            "git branch --merged main",
            "git -C sub branch --show-current",
        ] {
            assert!(is_read_only_command(c), "{c}");
        }
        for c in [
            "cat a > b",
            "python check.py",
            "rm x",
            "echo hi > f",
            "ls && python x.py",
            "bd done 9h3",
            "bd update x --status closed",
            "git commit -m x",
            "mkdir -p lib/content",
            "find . -name x -delete",
            "xargs rm",
            "",
            // writes dressed as reads (the old list counted every one of these as a read)
            "git branch -D feature/x",
            "git branch -m old new",
            "git branch newbranch",
            "sort -o out.txt in.txt",
            "echo $(rm -rf build)",
            "echo `rm -rf build`",
            "sed -i 's/a/b/' f",
            "sed -i.bak s/a/b/ f",
            "sed -ni p f",
            "awk -i inplace '{print}' f",
            "env FOO=1 python x.py",
        ] {
            assert!(!is_read_only_command(c), "{c}");
        }
        // file tool names never look like verification commands
        assert!(!CommandRecord::file_write("edit_file", "test_x.py", true).is_verification());
    }

    #[test]
    fn inputs_the_task_names_are_not_deliverables() {
        let d = tempfile::tempdir().expect("tmp");
        let m = |task: &str| task_named_missing(task, "", d.path());
        let none: Vec<String> = Vec::new();
        assert_eq!(
            m("Read data.csv and write the totals to result.txt"),
            vec!["result.txt"]
        );
        assert_eq!(
            m("Convert data.csv to a markdown table in table.md"),
            vec!["table.md"]
        );
        assert_eq!(
            m("Create report.md from the notes in notes.txt"),
            vec!["report.md"]
        );
        assert_eq!(m("Fix the bug in parser.py and run the tests."), none);
        assert_eq!(
            m("Run python fetch.py and save its output as out.json"),
            vec!["out.json"]
        );
        assert_eq!(m("Produce summary.md"), vec!["summary.md"]);
        // No cue keeps the old reading: a named file is expected to exist at the end.
        assert_eq!(m("summary.md with three bullet points"), vec!["summary.md"]);
    }

    // Gate 3c (bead boubou-78s): the tracker script's four turns, the longform count
    // question, a declined write reported as such, and the shell as a write.
    #[test]
    fn a_named_write_target_without_a_landed_write_is_unwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wd = dir.path();
        let names = |u: Vec<Unwritten>| u.into_iter().map(|u| u.name).collect::<Vec<_>>();
        let none: Vec<CommandRecord> = Vec::new();
        let t1 = "Break the spec in openspec/spec.md into small tasks in tasks.md, one `- [ ]` line each, smallest useful slice first. Do not implement anything yet.";
        assert_eq!(
            names(task_named_unwritten(
                t1,
                "Done, tasks.md updated.",
                &none,
                wd
            )),
            ["tasks.md"]
        );
        let t3 = "Tick the first task in tasks.md as done. Do not start the second one.";
        assert_eq!(
            names(task_named_unwritten(t3, "Ticked.", &none, wd)),
            ["tasks.md"]
        );
        let t4 = "Add the next three tasks you can think of for feature 5 (the --year flag) to tasks.md.";
        assert_eq!(
            names(task_named_unwritten(t4, "Added.", &none, wd)),
            ["tasks.md"]
        );
        // Nothing to write: a question about the file, and a turn naming no file.
        let q =
            "How many '## ' section headings does report.md have now? Reply with the number only.";
        assert!(task_named_unwritten(q, "7", &none, wd).is_empty());
        let t2 = "Which task should we do first, and why? Just tell me; do not change any file.";
        assert!(task_named_unwritten(t2, "The first.", &none, wd).is_empty());
        // The reply reports failure about the file: not a claim.
        let perm = "Write the single word hello to /tmp/boubou_convo_perm.txt.";
        assert_eq!(
            names(task_named_unwritten(
                perm,
                "Please try again if you wish.",
                &none,
                wd
            )),
            ["boubou_convo_perm.txt"]
        );
        assert!(task_named_unwritten(
            perm,
            "I was unable to create /tmp/boubou_convo_perm.txt.",
            &none,
            wd
        )
        .is_empty());
        // Bead boubou-18l: a failure report that names no file admits it for every file;
        // one that names another file does not.
        assert!(task_named_unwritten(
            perm,
            "Responded directly to user's request - cannot write files outside workspace path due to security restrictions",
            &none
        , wd)
        .is_empty());
        assert_eq!(
            names(task_named_unwritten(
                perm,
                "I could not read notes.txt, so nothing was done.",
                &none,
                wd
            )),
            ["boubou_convo_perm.txt"]
        );
        // A landed file-tool write, a refused one, and a shell redirect.
        let landed = vec![CommandRecord::file_write("append_file", "tasks.md", true)];
        assert!(task_named_unwritten(t3, "Ticked.", &landed, wd).is_empty());
        let refused = vec![CommandRecord::file_write("append_file", "tasks.md", false)];
        assert_eq!(
            names(task_named_unwritten(t3, "Ticked.", &refused, wd)),
            ["tasks.md"]
        );
        let shell = vec![CommandRecord::from_result(
            "echo '- [x] one' >> tasks.md",
            "[exit 0]\nSTDOUT:\n",
        )];
        assert!(task_named_unwritten(t3, "Ticked.", &shell, wd).is_empty());
        let shell_failed = vec![CommandRecord::from_result(
            "echo '- [x] one' >> tasks.md",
            "[exit 1]\nSTDERR:\nno\n",
        )];
        assert_eq!(
            names(task_named_unwritten(t3, "Ticked.", &shell_failed, wd)),
            ["tasks.md"]
        );
        // The plan file itself is never a deliverable.
        assert!(
            task_named_unwritten("Update plan.md with the next step.", "Done.", &none, wd)
                .is_empty()
        );
    }

    // Bead boubou-di6: a bare name that is a file in the workdir is that file. qwen3.5:4b
    // (4b_scope_v1 tracker r2), told "tasks in tasks.md" with ./tasks.md present, created
    // openspec/tasks.md beside the other path in the sentence.
    #[test]
    fn a_bare_name_that_is_a_file_in_the_workdir_must_be_the_file_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wd = dir.path();
        std::fs::write(wd.join("tasks.md"), "# Tasks\n").expect("write");
        std::fs::create_dir(wd.join("openspec")).expect("mkdir");
        std::fs::write(wd.join("openspec/tasks.md"), "- [ ] 1.\n").expect("write");
        let t1 = "Break the spec in openspec/spec.md into small tasks in tasks.md, one `- [ ]` line each, smallest useful slice first.";
        let beside = vec![
            CommandRecord::from_result("mkdir -p openspec", "[exit 0]\nSTDOUT:\n"),
            CommandRecord::file_write("create_file", "openspec/tasks.md", true),
            CommandRecord::file_write("edit_file", "openspec/tasks.md", true),
        ];
        let u = task_named_unwritten(t1, "Created tasks.md with six tasks.", &beside, wd);
        assert_eq!(
            u,
            [Unwritten {
                name: "tasks.md".into(),
                elsewhere: vec!["openspec/tasks.md".into()],
            }]
        );
        assert_eq!(
            u[0].line(),
            "tasks.md is ./tasks.md; the write went to openspec/tasks.md"
        );
        let shell = vec![CommandRecord::from_result(
            "echo '- [ ] one' >> openspec/tasks.md",
            "[exit 0]\nSTDOUT:\n",
        )];
        assert_eq!(
            task_named_unwritten(t1, "Done.", &shell, wd)[0].elsewhere,
            ["openspec/tasks.md"]
        );
        // The file itself, by any spelling.
        let abs = wd.join("tasks.md").to_string_lossy().into_owned();
        for p in [
            "tasks.md",
            "./tasks.md",
            "openspec/../tasks.md",
            abs.as_str(),
        ] {
            let w = vec![CommandRecord::file_write("edit_file", p, true)];
            assert!(task_named_unwritten(t1, "Done.", &w, wd).is_empty(), "{p}");
        }
        // Nothing of the name written at all: the plain line, as before.
        let none: Vec<CommandRecord> = Vec::new();
        assert_eq!(
            task_named_unwritten(t1, "Done.", &none, wd)[0].line(),
            "tasks.md"
        );
        // No file of that name in the workdir: the model chooses where it goes.
        let empty = tempfile::tempdir().expect("tempdir");
        assert!(task_named_unwritten(t1, "Done.", &beside, empty.path()).is_empty());
    }
}
