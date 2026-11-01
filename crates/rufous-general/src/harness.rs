//! What makes the general-purpose harness its own: the `general` turn policy, the task prompt it runs
//! and the skills built into it (`skills/*.md` in this crate). The kernel's
//! [`TurnPolicy::conversation`] supplies the turn semantics; this module supplies the name,
//! the words and the operating manuals.
//!
//! The web UI (`rufous serve`, `rufous-app`) and `rufous chat` run this policy; the
//! skills are the ones about documents, folders and research rather than code.
//!
//! The prompt is the kernel's task prefill, as a copy this crate owns, less the two sentences
//! that ask for finish: in a conversation text ends the turn, and a finish summary taken as
//! the reply dropped the conversation's standing instructions (bead boubou-twe). Further
//! differences are made here, with the test stating them (`docs/refactor.md`, step 10).

use rufous_kernel::agent::{Prompt, TurnPolicy};
use rufous_kernel::skills::Skill;

/// The task prefill this harness runs. `{PLATFORM_LINE}` and `{USER_HOME}` are filled by
/// the kernel when the prompt is built.
pub const TASK_PROMPT: &str = r#"You are an autonomous AI agent in a workspace directory.
JUDGE INTENT FIRST - decide what the user wants before deciding how to act:
- Small talk, greetings, introducing yourself, general knowledge questions (e.g. "hello", "who are you", "what can you do", "what's the weather like"):
  ANSWER IN PLAIN TEXT. Never call a tool, never create a todo, never create a file or run a command.
- Real work (writing code, creating or editing files, analysing data, researching, running commands, fixing bugs):
  first call todo(action=create) to make a numbered plan, then execute it step by step with tools, updating the todo as you go.
RULES:
- Do not treat "hello / what do you do" as a task to demonstrate tools on. Just answer.
- Actually DO the work with tools when it's a real task (write code, run it, fix errors until verified). Never just describe.
- Need web, memory or MCP tools? Call enable_tools first; they then become available.
- {PLATFORM_LINE}
- Home directory: {USER_HOME}. To save outside the workspace (e.g. the desktop at {USER_HOME}/Desktop) use the real absolute path; the out-of-workspace confirmation will let it through. Never invent directories.
- Be concise in text; put large content in tool arguments."#;

/// The `general` preset: [`TurnPolicy::conversation`] under this harness's name, on its own
/// task prefill.
pub const fn policy() -> TurnPolicy {
    TurnPolicy {
        name: "general",
        prompt: Prompt::Task(TASK_PROMPT),
        ..TurnPolicy::conversation()
    }
}

/// The skills compiled into this harness, one file each under `skills/` in this crate.
const SKILL_FILES: &[&str] = &[
    include_str!("../skills/doc_digest.md"),
    include_str!("../skills/file_organizer.md"),
    include_str!("../skills/markdown_report.md"),
    include_str!("../skills/web_research.md"),
];

/// The built-in skills, parsed. A file that fails to parse is a build defect the crate's
/// tests catch; at run time it is logged and skipped rather than failing the turn.
pub fn skills() -> Vec<Skill> {
    SKILL_FILES
        .iter()
        .filter_map(|text| match Skill::embedded(text) {
            Ok(skill) => Some(skill),
            Err(e) => {
                tracing::warn!("built-in skill unreadable: {e}");
                None
            }
        })
        .collect()
}
