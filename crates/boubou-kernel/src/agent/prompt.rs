//! System prompts (`TASK_BASE`, `CHAT_SYSTEM`, `system_prompt()`) and the platform block
//! (`platform_profile.prompt_block`). The structure is Mingbird's; the text is Boubou's
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
/// behaviour benchmarks carry over; the wording is Boubou's.
pub const CHAT_SYSTEM: &str = "You are Boubou, a local AI assistant, chatting with the user.
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
            Platform::Windows => "list dir=dir / show file=type / search text=findstr / copy=copy",
            _ => "list dir=ls / show file=cat / search text=grep / copy=cp",
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
        "Platform: {} · command shell={} (supports && chaining)\n- Command map: {}; path separator {}, quote paths containing spaces\n{}",
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
/// instead. A folder can therefore carry its own persona and rules.
pub const PROJECT_SYSTEM_FILE: &str = "SYSTEM.md";
pub const REPLACE_MARK: &str = "<!-- boubou: replace -->";

/// `(text, replace)` from `<workdir>/SYSTEM.md`; `None` when absent or blank.
pub fn project_system(workdir: Option<&Path>) -> Option<(String, bool)> {
    let text = read_text(&workdir?.join(PROJECT_SYSTEM_FILE)).ok()?;
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    match t.strip_prefix(REPLACE_MARK) {
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
    /// The model profile's prompt note (bead 65d.15), after the skills and before the
    /// workdir line. Only the task prompt carries it; the Q&A prompt does not.
    pub profile_note: Option<&'a str>,
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
    for (name, body) in &opts.skills {
        base.push_str(&format!(
            "\n\nSKILL IN EFFECT FOR THIS TASK (follow it):\n### SKILL: {name}\n{}",
            body.trim_end()
        ));
    }
    if let Some(note) = opts.profile_note.map(str::trim).filter(|n| !n.is_empty()) {
        base.push_str(&format!(
            "\n\nMODEL PROFILE NOTE (applies to this task):\n{note}"
        ));
    }
    if let Some(wd) = opts.workdir {
        base.push_str(&workdir_line(wd));
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
    if let Some(wd) = workdir {
        s.push_str(&format!("\nCurrent working directory: {}", wd.display()));
    }
    s.push('\n');
    s.push_str(&platform_block(false, false));
    s
}
