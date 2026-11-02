//! Prompt assembly, bead boubou-bk9.5: the environment line (date, git state, local
//! tools) in the task prompt under the `env_block` ablation, and `AGENTS.md` as a second
//! per-folder instruction file after `SYSTEM.md`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rufous_kernel::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use rufous_kernel::agent::prompt::{
    self, Environment, PromptOptions, PROBED_TOOLS, PROJECT_FILE_CHARS,
};
use rufous_kernel::agent::{ablation, Agent, AgentEvent, RunParams, ToolRunner};
use rufous_kernel::ollama::Message;
use serde_json::Value;

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::create_dir_all(dir).expect("mkdir");
    std::fs::write(dir.join(name), text).expect("write");
}

#[test]
fn the_environment_is_probed_without_a_subprocess() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    let e = Environment::probe(wd);
    assert_eq!(e.date.len(), 10, "{}", e.date);
    assert!(e.date.starts_with("20"));
    assert_eq!(e.git, None);
    assert!(e.tools.iter().all(|t| PROBED_TOOLS.contains(&t.as_str())));
    let line = e.line();
    assert!(
        line.starts_with(&format!(
            "\nDate: {}. The working directory is not a git repository. Local tools on PATH: ",
            e.date
        )),
        "{line}"
    );
    assert!(
        line.len() < 220,
        "one line, not a paragraph: {} chars",
        line.len()
    );

    write(
        &wd.join(".git"),
        "HEAD",
        "ref: refs/heads/feature/env-line\n",
    );
    let e = Environment::probe(wd);
    assert_eq!(e.git.as_deref(), Some("feature/env-line"));
    assert!(
        e.line()
            .contains("is a git repository on branch feature/env-line."),
        "{}",
        e.line()
    );

    write(
        &wd.join(".git"),
        "HEAD",
        "0123456789abcdef0123456789abcdef01234567\n",
    );
    assert_eq!(Environment::probe(wd).git.as_deref(), Some(""));
    assert!(Environment::probe(wd)
        .line()
        .contains("is a git repository. Local tools"));

    let w = d.path().join("worktree");
    std::fs::create_dir_all(&w).expect("mkdir");
    std::fs::write(w.join(".git"), "gitdir: ../.git/worktrees/w\n").expect("write");
    assert_eq!(Environment::probe(&w).git.as_deref(), Some(""));

    let none = Environment {
        date: "2026-10-31".into(),
        git: None,
        tools: vec![],
    };
    assert_eq!(
        none.line(),
        "\nDate: 2026-10-31. The working directory is not a git repository. Local tools on PATH: none of the common ones found."
    );
}

#[test]
fn agents_md_follows_system_md_in_both_prompts_and_is_capped() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    fn opts<'a>(wd: &'a Path, env: Option<&'a Environment>) -> PromptOptions<'a> {
        PromptOptions {
            base: None,
            workdir: Some(wd),
            system_file: None,
            unattended: false,
            skills: vec![],
            skill_catalogue: None,
            profile_note: None,
            hook_context: None,
            environment: env,
        }
    }
    let bare = prompt::system_prompt(&opts(wd, None));
    assert!(!bare.contains("AGENTS.md"), "{bare}");

    write(wd, "SYSTEM.md", "House rule: GBP everywhere.\n");
    write(
        wd,
        "AGENTS.md",
        "# Repo guide\nRun `make check` before you finish.\n",
    );
    let p = prompt::system_prompt(&opts(wd, None));
    let sys_at = p.find("PROJECT INSTRUCTIONS (SYSTEM.md in the working directory; follow them):\nHouse rule: GBP everywhere.").expect("SYSTEM.md block");
    let ag_at = p.find("PROJECT INSTRUCTIONS (AGENTS.md in the working directory; follow them):\n# Repo guide\nRun `make check` before you finish.").expect("AGENTS.md block");
    let wd_at = p.find("Current working directory").expect("workdir line");
    assert!(sys_at < ag_at && ag_at < wd_at, "{p}");
    let c = prompt::chat_system(Some(wd));
    assert!(
        c.contains("GBP everywhere") && c.contains("Run `make check`"),
        "{c}"
    );
    assert!(
        !c.contains("\nDate: "),
        "the Q&A prompt carries no environment line"
    );

    // A blank file is nothing; a long one is cut with a marker.
    write(wd, "AGENTS.md", "  \n");
    assert!(!prompt::system_prompt(&opts(wd, None)).contains("AGENTS.md"));
    write(wd, "AGENTS.md", &"rule\n".repeat(2000));
    let p = prompt::system_prompt(&opts(wd, None));
    assert!(
        p.contains(&format!(
            "[AGENTS.md truncated at {PROJECT_FILE_CHARS} characters]"
        )),
        "{}",
        &p[p.len() - 300..]
    );

    // The environment line sits between the workdir line and the platform block.
    let env = Environment {
        date: "2026-10-31".into(),
        git: Some("main".into()),
        tools: vec!["git".into(), "rg".into()],
    };
    let p = prompt::system_prompt(&opts(wd, Some(&env)));
    let a = p.find("Current working directory").expect("workdir");
    let b = p.find("\nDate: 2026-10-31. The working directory is a git repository on branch main. Local tools on PATH: git, rg.\n").expect("env line");
    let c = p.find("Platform:").expect("platform");
    assert!(a < b && b < c, "{p}");
    let without = prompt::system_prompt(&opts(wd, None));
    eprintln!(
        "[measure] task prompt without environment line: {} chars; with: {} chars (+{})",
        without.len(),
        p.len(),
        p.len() - without.len()
    );
}

struct Finish;
impl ChatBackend for Finish {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        Ok(ChatResponse::text("Done."))
    }
}
struct OkTools;
impl ToolRunner for OkTools {
    fn run_tool(&mut self, _name: &str, _args: &Value) -> String {
        "[ok]".into()
    }
}

#[test]
fn the_task_prompt_carries_the_environment_line_unless_ablated() {
    let d = tempfile::tempdir().expect("tmp");
    let logs = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink_logs = logs.clone();
    let mut a = Agent::new(RunParams::plain("gemma4:e2b", d.path()), Finish, OkTools).with_sink(
        move |ev| {
            if let AgentEvent::Log(l) = ev {
                sink_logs.lock().expect("lock").push(l);
            }
        },
    );
    let m = a.task_messages("Tidy the notes");
    let sys = &m[0].content;
    let date = a.environment.date.clone();
    assert!(
        sys.contains(&format!(
            "\nDate: {date}. The working directory is not a git repository. Local tools on PATH: "
        )),
        "{sys}"
    );
    assert_eq!(m[1].content, "Tidy the notes");
    // A question's prompt does not carry it.
    let q = a.task_messages("What does EBITDA stand for?");
    let out = a.run(q, None, None).expect("loop");
    assert!(
        out[0].content.contains("chatting with the user") && !out[0].content.contains("\nDate: ")
    );

    let mut params = RunParams::plain("gemma4:e2b", d.path());
    params.ablation = vec![ablation::ENV_BLOCK.to_string()];
    let mut off = Agent::new(params, Finish, OkTools);
    let m = off.task_messages("Tidy the notes");
    assert!(!m[0].content.contains("\nDate: "), "{}", m[0].content);
    assert!(m[0].content.contains("Current working directory"));
}
