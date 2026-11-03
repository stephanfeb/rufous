//! System prompts (`TASK_BASE`, `CHAT_SYSTEM`, `system_prompt()`) and the platform block
//! (`platform_profile.prompt_block`). The structure is Mingbird's; the text is rufous's
//! own English. Dynamic lines are added at run time so the factory prefill stays
//! byte-stable.
//!
//! `TASK_BASE` is the kernel's task prefill, the one the unattended harness runs. A product
//! supplies its own base through its turn policy (`Prompt::Task(base)`); [`system_prompt`]
//! builds the same way around either, so a product edits its text, not the assembly.

use std::path::Path;

use crate::text::read_text;

/// The kernel's task prefill. `{PLATFORM_LINE}` and `{USER_HOME}` are filled by
/// [`fill_base`]; a product's base may use the same placeholders.
pub const TASK_BASE: &str = r#"You are an autonomous AI agent in a workspace directory.
JUDGE INTENT FIRST - decide what the user wants before deciding how to act:
- Small talk, greetings, introducing yourself, general knowledge questions (e.g. "hello", "who are you", "what can you do", "what's the weather like"):
  ANSWER IN PLAIN TEXT. Never call a tool, never create a todo, never create a file or run a command. Then call finish.
- Real work (writing code, creating or editing files, analysing data, researching, running commands, fixing bugs):
  first call todo(action=create) to make a numbered plan, then execute it step by step with tools, updating the todo as you go.
RULES:
- Do not treat "hello / what do you do" as a task to demonstrate tools on. Just answer.
- Actually DO the work with tools when it's a real task (write code, run it, fix errors until verified). Never just describe.
- Need web, memory or MCP tools? Call enable_tools first; they then become available.
- {PLATFORM_LINE}
- Home directory: {USER_HOME}. To save outside the workspace (e.g. the desktop at {USER_HOME}/Desktop) use the real absolute path; the out-of-workspace confirmation will let it through. Never invent directories.
- Be concise in text; put large content in tool arguments. Call finish only when answered or fully verified done."#;

const PLATFORM_LINE_WINDOWS: &str = "Work only in the current directory (Windows). Never cd to absolute paths like /workspace — commands already run here; use relative paths.";
const PLATFORM_LINE_POSIX: &str = "Work only in the current directory. Never cd outside it and never invent paths like C:\\ — commands already run here; use relative paths.";

/// `CHAT_SYSTEM`: the Q&A prefill. Same shape as Mingbird's self-introduction so the
/// behaviour benchmarks carry over; the wording is rufous's.
pub const CHAT_SYSTEM: &str = "You are rufous, a local AI assistant, chatting with the user.
- Answer directly, naturally and concisely, like a person in conversation.
- For ordinary small talk or simple questions, just answer: one sentence or a few, never an essay.
- Use tools only when you genuinely need to look something up: read a file, search, or run a command that reports state (git status, ls, the project's own CLI); otherwise answer in plain text.
- Stop when you have answered. No repetition, no showing off, no filler like \"how can I help you today\".";

/// The kernel's task prefill with the home directory and platform line filled in.
pub fn system_base() -> String {
    fill_base(TASK_BASE)
}

/// A task prefill with its `{PLATFORM_LINE}` and `{USER_HOME}` placeholders filled in.
pub fn fill_base(base: &str) -> String {
    let home = dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "~".to_string());
    let line = if cfg!(windows) {
        PLATFORM_LINE_WINDOWS
    } else {
        PLATFORM_LINE_POSIX
    };
    base.replace("{PLATFORM_LINE}", line)
        .replace("{USER_HOME}", &home)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::Windows => "Windows",
            Platform::MacOs => "macOS",
            Platform::Linux => "Linux",
        }
    }

    pub fn shell(self) -> &'static str {
        match self {
            Platform::Windows => "cmd.exe",
            _ => "/bin/sh",
        }
    }

    pub fn path_sep(self) -> &'static str {
        match self {
            Platform::Windows => "\\",
            _ => "/",
        }
    }

    fn command_map(self) -> &'static str {
        match self {
            Platform::Windows => "Command map: list dir=dir / show file=type / search text=findstr / copy=copy; to look around prefer list_dir, read_file and search_files",
            _ => "To look around use list_dir, read_file and search_files rather than ls, cat and grep (their output is bounded and says when it is cut)",
        }
    }
}

/// `platform_profile.prompt_block`. `full=false` is the one-line Q&A variant.
pub fn platform_block(full: bool, unattended: bool) -> String {
    let p = Platform::current();
    if !full {
        return format!("Platform: {}, path separator {}", p.label(), p.path_sep());
    }
    let guard = if unattended {
        "- Commands run outside a sandbox. Recursive deletes, uninstalls and system-environment changes are refused outright in unattended mode; do not retry them, use a safe path inside the working directory instead."
    } else {
        "- Commands run outside a sandbox. Recursive deletes, uninstalls and system-environment changes prompt the user for confirmation. That is normal protection, not a fault; carry on with the work."
    };
    format!(
        "Platform: {} · command shell={} (supports && chaining)\n- {}; path separator {}, quote paths containing spaces\n{}",
        p.label(),
        p.shell(),
        p.command_map(),
        p.path_sep(),
        guard
    )
}

/// `_CHILD_SYSTEM`: the minimal prompt of a dispatched child (local cost of a short
/// small-model session; the parent's prefill is untouched).
pub const CHILD_SYSTEM: &str = "You are a sub-agent doing one small mechanical file task.
- Work ONLY inside your current directory. Never touch paths outside it.
- Use create_file to write the result, list_dir/read_file to inspect, todo to plan.
- No commands, no network, no deleting, no editing other files.
- When the output file exists and is correct, call finish(summary=files you created).";

/// The dynamic working-directory line appended to the task and child prompts.
fn workdir_line(wd: &Path) -> String {
    format!(
        "\nCurrent working directory (all commands and relative paths run under it): {}",
        wd.display()
    )
}

/// The child prompt plus the dynamic workdir line. No platform block: a child has no
/// command tools, so the shell notes would only cost tokens.
pub fn child_system(workdir: Option<&Path>) -> String {
    let mut s = CHILD_SYSTEM.to_string();
    if let Some(wd) = workdir {
        s.push_str(&workdir_line(wd));
    }
    s
}

/// Per-workdir instructions: `<workdir>/SYSTEM.md` is appended under the base prompt for
/// task and chat turns alike; when its first line is [`REPLACE_MARK`] it replaces the base
/// instead. A folder can therefore carry its own persona and rules. `<workdir>/AGENTS.md`,
/// the file the other harnesses read, is appended after it (bead boubou-bk9.5), capped at
/// [`PROJECT_FILE_CHARS`].
pub const PROJECT_SYSTEM_FILE: &str = "SYSTEM.md";
pub const PROJECT_AGENTS_FILE: &str = "AGENTS.md";
pub const REPLACE_MARK: &str = "<!-- rufous: replace -->";
/// [`REPLACE_MARK`] under the name before rufous; a SYSTEM.md written then still replaces.
pub const REPLACE_MARK_OLD: &str = "<!-- boubou: replace -->";
/// Upper bound on the `AGENTS.md` text placed in a prompt.
pub const PROJECT_FILE_CHARS: usize = 6000;

/// `<workdir>/AGENTS.md`, trimmed and capped; `None` when absent or blank.
pub fn project_agents(workdir: Option<&Path>) -> Option<String> {
    let text = read_text(&workdir?.join(PROJECT_AGENTS_FILE)).ok()?;
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if crate::text::clen(t) > PROJECT_FILE_CHARS {
        return Some(format!(
            "{}\n[AGENTS.md truncated at {PROJECT_FILE_CHARS} characters]",
            crate::text::head(t, PROJECT_FILE_CHARS)
        ));
    }
    Some(t.to_string())
}

/// What the model is told about where it runs, beyond the working directory and the
/// platform: the date, the git state and the local tools found on PATH. Probed once per
/// run ([`Environment::probe`]); one line of the task prompt ([`Environment::line`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// `YYYY-MM-DD`, local time.
    pub date: String,
    /// `None`: not a git repository. `Some(branch)`; an empty branch is a detached head
    /// or a worktree whose `.git` is a file.
    pub git: Option<String>,
    /// The entries of [`PROBED_TOOLS`] found on PATH, in that order.
    pub tools: Vec<String>,
}

/// The local tools the environment line looks for, in the order it names them.
pub const PROBED_TOOLS: &[&str] = &[
    "git", "rg", "fd", "jq", "python3", "uv", "node", "npm", "cargo", "go", "dart", "flutter",
    "docker", "make", "gh", "bd",
];

impl Environment {
    pub fn probe(workdir: &Path) -> Self {
        Self {
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            git: git_state(workdir),
            tools: PROBED_TOOLS
                .iter()
                .filter(|t| on_path(t))
                .map(|t| t.to_string())
                .collect(),
        }
    }

    /// The prompt line, after the workdir line.
    pub fn line(&self) -> String {
        let git = match &self.git {
            Some(b) if !b.is_empty() => format!("a git repository on branch {b}"),
            Some(_) => "a git repository".to_string(),
            None => "not a git repository".to_string(),
        };
        let tools = if self.tools.is_empty() {
            "none of the common ones found".to_string()
        } else {
            self.tools.join(", ")
        };
        format!(
            "\nDate: {}. The working directory is {git}. Local tools on PATH: {tools}.",
            self.date
        )
    }
}

/// The branch from `.git/HEAD`, read without a subprocess.
fn git_state(workdir: &Path) -> Option<String> {
    let g = workdir.join(".git");
    if g.is_file() {
        return Some(String::new());
    }
    if !g.is_dir() {
        return None;
    }
    let head = std::fs::read_to_string(g.join("HEAD")).unwrap_or_default();
    Some(
        head.trim()
            .strip_prefix("ref: refs/heads/")
            .unwrap_or("")
            .to_string(),
    )
}

/// Whether `name` is an executable file on PATH.
pub fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: &[&str] = if cfg!(windows) { &[".exe", ""] } else { &[""] };
    std::env::split_paths(&path).any(|dir| {
        exts.iter()
            .any(|ext| dir.join(format!("{name}{ext}")).is_file())
    })
}

/// `(text, replace)` from `<workdir>/SYSTEM.md`; `None` when absent or blank.
pub fn project_system(workdir: Option<&Path>) -> Option<(String, bool)> {
    let text = read_text(&workdir?.join(PROJECT_SYSTEM_FILE)).ok()?;
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    match t
        .strip_prefix(REPLACE_MARK)
        .or_else(|| t.strip_prefix(REPLACE_MARK_OLD))
    {
        Some(rest) => Some((rest.trim().to_string(), true)),
        None => Some((t.to_string(), false)),
    }
}

fn push_project(base: &mut String, project: Option<&(String, bool)>) {
    if let Some((t, _)) = project {
        base.push_str(
            "\n\nPROJECT INSTRUCTIONS (SYSTEM.md in the working directory; follow them):\n",
        );
        base.push_str(t);
    }
}

fn push_agents(base: &mut String, agents: Option<&str>) {
    if let Some(t) = agents {
        base.push_str(
            "\n\nPROJECT INSTRUCTIONS (AGENTS.md in the working directory; follow them):\n",
        );
        base.push_str(t);
    }
}

const SESSION_CONTEXT_HEAD: &str = "\n\nSESSION CONTEXT (from the project's hooks; follow it):\n";
const SESSION_CONTEXT_END: &str = "\nEND SESSION CONTEXT";

/// The `SessionStart` context a built task prompt carries, as it was given. A continued
/// session's rebuild takes it from the previous system message rather than firing the hooks
/// again, so the prompt keeps its text and the server its prefix cache (bead boubou-xwi).
/// `None` when there is no block, or for a prompt built before the block had its end line.
pub fn session_context(system: &str) -> Option<String> {
    let start = system.find(SESSION_CONTEXT_HEAD)? + SESSION_CONTEXT_HEAD.len();
    let len = system[start..].find(SESSION_CONTEXT_END)?;
    Some(system[start..start + len].to_string())
}

/// Options for [`system_prompt`].
#[derive(Debug, Clone, Default)]
pub struct PromptOptions<'a> {
    /// The task prefill to build on, placeholders unfilled; `None` is the kernel's
    /// [`TASK_BASE`]. A product's turn policy carries its own.
    pub base: Option<&'a str>,
    /// Absolute workdir, injected as the dynamic "current working directory" line.
    pub workdir: Option<&'a Path>,
    /// `AGENT_SYSTEM_FILE`: replaces the static prefill when it exists.
    pub system_file: Option<&'a Path>,
    pub unattended: bool,
    /// Skills in effect for this task (`name`, body), prefilled after the base prompt.
    pub skills: Vec<(String, String)>,
    /// The SKILLS AVAILABLE block ([`crate::skills::SkillSet::catalogue`], bead boubou-15s),
    /// after the skills in effect. Task prompt only.
    pub skill_catalogue: Option<String>,
    /// The model profile's prompt note (bead 65d.15), after the skills and before the
    /// workdir line. Only the task prompt carries it; the Q&A prompt does not.
    pub profile_note: Option<&'a str>,
    /// What the project's `SessionStart` hooks printed ([`crate::hooks`]), after the
    /// project instructions and before the skills. Task prompt only.
    pub hook_context: Option<String>,
    /// The environment line after the workdir line (bead boubou-bk9.5); `None` under the
    /// `env_block` ablation. Task prompt only.
    pub environment: Option<&'a Environment>,
}

/// `system_prompt()`: static prefill + project instructions + skills in effect + dynamic
/// workdir line + platform block. Base precedence: the explicit override file, then a
/// replacing `SYSTEM.md`, then the built-in prompt; an appending `SYSTEM.md` (or a replacing
/// one that lost to the override) follows the base.
pub fn system_prompt(opts: &PromptOptions<'_>) -> String {
    let project = project_system(opts.workdir);
    let override_text = match opts.system_file {
        Some(p) if p.exists() => read_text(p).ok(),
        _ => None,
    };
    let (mut base, project_used) = match (override_text, &project) {
        (Some(t), _) => (t, false),
        (None, Some((t, true))) => (t.clone(), true),
        _ => (fill_base(opts.base.unwrap_or(TASK_BASE)), false),
    };
    if !project_used {
        push_project(&mut base, project.as_ref());
    }
    push_agents(&mut base, project_agents(opts.workdir).as_deref());
    if let Some(ctx) = opts
        .hook_context
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        base.push_str(SESSION_CONTEXT_HEAD);
        base.push_str(ctx);
        base.push_str(SESSION_CONTEXT_END);
    }
    for (name, body) in &opts.skills {
        base.push_str(&format!(
            "\n\nSKILL IN EFFECT FOR THIS TASK (follow it):\n### SKILL: {name}\n{}",
            body.trim_end()
        ));
    }
    if let Some(c) = &opts.skill_catalogue {
        base.push_str("\n\n");
        base.push_str(c);
    }
    if let Some(note) = opts.profile_note.map(str::trim).filter(|n| !n.is_empty()) {
        base.push_str(&format!(
            "\n\nMODEL PROFILE NOTE (applies to this task):\n{note}"
        ));
    }
    if let Some(wd) = opts.workdir {
        base.push_str(&workdir_line(wd));
    }
    if let Some(env) = opts.environment {
        base.push_str(&env.line());
    }
    base.push('\n');
    base.push_str(&platform_block(true, opts.unattended));
    base
}

/// The Q&A system prompt for this workdir (`CHAT_SYSTEM` + project instructions + workdir +
/// one-line platform).
pub fn chat_system(workdir: Option<&Path>) -> String {
    let project = project_system(workdir);
    let replaced = matches!(&project, Some((_, true)));
    let mut s = match &project {
        Some((t, true)) => t.clone(),
        _ => CHAT_SYSTEM.to_string(),
    };
    if !replaced {
        push_project(&mut s, project.as_ref());
    }
    push_agents(&mut s, project_agents(workdir).as_deref());
    if let Some(wd) = workdir {
        s.push_str(&format!("\nCurrent working directory: {}", wd.display()));
    }
    s.push('\n');
    s.push_str(&platform_block(false, false));
    s
}
