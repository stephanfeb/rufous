//! Deliberate domain bias: skills with `triggers`/`autoload` are prefilled into the task
//! prompt, and `<workdir>/SYSTEM.md` shapes the base prompt for task and chat turns.

use std::path::Path;
use std::sync::{Arc, Mutex};

use boubou_core::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use boubou_core::agent::prompt::{self, PromptOptions, REPLACE_MARK};
use boubou_core::agent::{Agent, AgentEvent, RunParams, ToolRunner};
use boubou_core::ollama::{Message, Role};
use boubou_core::skills::{Skill, SkillSet, MAX_PREFILL_SKILLS};
use serde_json::{json, Value};

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::create_dir_all(dir).expect("mkdir");
    std::fs::write(dir.join(name), text).expect("write");
}

fn skill(dir: &Path, name: &str, extra: &str) {
    write(
        dir,
        &format!("{name}.md"),
        &format!("---\nname: {name}\ndescription: about {name}\n{extra}---\n# {name} steps\nDo the {name} thing.\n"),
    );
}

#[test]
fn skill_frontmatter_triggers_and_autoload() {
    let d = tempfile::tempdir().expect("tmp");
    skill(
        d.path(),
        "finance",
        "triggers: Ledger, invoice , cash flow\n",
    );
    skill(d.path(), "house", "autoload: true\n");
    skill(d.path(), "plain", "");
    let f = Skill::from_path(&d.path().join("finance.md")).expect("parse");
    assert_eq!(f.triggers, vec!["ledger", "invoice", "cash flow"]);
    assert!(!f.autoload);
    let h = Skill::from_path(&d.path().join("house.md")).expect("parse");
    assert!(h.autoload && h.triggers.is_empty());
    let set = SkillSet::discover([d.path()]);
    let names =
        |t: &str| -> Vec<String> { set.for_task(t).iter().map(|s| s.name.clone()).collect() };
    // Autoload always; a trigger matches case-insensitively as a substring; plain never.
    assert_eq!(
        names("Reconcile the LEDGER for March"),
        vec!["finance", "house"]
    );
    assert_eq!(names("Draft a poem"), vec!["house"]);
    assert_eq!(names("Check the cash flows"), vec!["finance", "house"]);
    // The listing still shows every skill (progressive loading is unchanged).
    assert_eq!(set.len(), 3);
}

#[test]
fn prefill_is_capped() {
    let d = tempfile::tempdir().expect("tmp");
    for n in ["a", "b", "c", "d", "e"] {
        skill(d.path(), n, "autoload: yes\n");
    }
    let set = SkillSet::discover([d.path()]);
    assert_eq!(set.for_task("anything").len(), MAX_PREFILL_SKILLS);
}

#[test]
fn project_system_md_appends_or_replaces() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    let opts = |skills: Vec<(String, String)>| PromptOptions {
        workdir: Some(wd),
        system_file: None,
        unattended: false,
        skills,
    };
    let base = prompt::system_prompt(&opts(vec![]));
    assert!(base.starts_with("You are an autonomous AI agent"));
    assert!(!base.contains("PROJECT INSTRUCTIONS"));

    write(
        wd,
        "SYSTEM.md",
        "You are the finance desk. Always quote figures in GBP.\n",
    );
    let p = prompt::system_prompt(&opts(vec![]));
    assert!(
        p.starts_with("You are an autonomous AI agent"),
        "append keeps the base"
    );
    assert!(p.contains("PROJECT INSTRUCTIONS") && p.contains("quote figures in GBP"));
    assert!(
        p.contains("Current working directory"),
        "dynamic lines follow"
    );
    let c = prompt::chat_system(Some(wd));
    assert!(c.starts_with("You are Boubou") && c.contains("quote figures in GBP"));

    write(
        wd,
        "SYSTEM.md",
        &format!("{REPLACE_MARK}\nYou are Tally, a bookkeeper.\n"),
    );
    let p = prompt::system_prompt(&opts(vec![]));
    assert!(p.starts_with("You are Tally, a bookkeeper."), "{p}");
    assert!(!p.contains("You are an autonomous AI agent") && !p.contains("PROJECT INSTRUCTIONS"));
    let c = prompt::chat_system(Some(wd));
    assert!(c.starts_with("You are Tally, a bookkeeper.") && !c.contains("You are Boubou"));

    // The explicit override file still wins the base; the project text is not lost.
    write(wd, "override.txt", "OVERRIDE PROMPT");
    let p = prompt::system_prompt(&PromptOptions {
        workdir: Some(wd),
        system_file: Some(&wd.join("override.txt")),
        unattended: false,
        skills: vec![],
    });
    assert!(p.starts_with("OVERRIDE PROMPT") && p.contains("You are Tally"));

    // Blank file: nothing happens.
    write(wd, "SYSTEM.md", "  \n");
    assert!(!prompt::system_prompt(&opts(vec![])).contains("PROJECT INSTRUCTIONS"));

    // Skills in effect sit between the base and the dynamic lines.
    let p = prompt::system_prompt(&opts(vec![("finance".into(), "# finance steps\n".into())]));
    let a = p.find("SKILL IN EFFECT").expect("skill section");
    let b = p.find("### SKILL: finance").expect("skill header");
    let c = p.find("Current working directory").expect("workdir line");
    assert!(a < b && b < c, "{p}");
}

// ---- through the agent ----

struct Finish;
impl ChatBackend for Finish {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        Ok(ChatResponse::text("Done, here is the answer."))
    }
}
struct OkTools;
impl ToolRunner for OkTools {
    fn run_tool(&mut self, name: &str, _args: &Value) -> String {
        if name == "finish" {
            "[TASK_COMPLETE] ok".into()
        } else {
            "[ok]".into()
        }
    }
}

fn agent(wd: &Path, logs: Arc<Mutex<Vec<String>>>) -> Agent<Finish, OkTools> {
    Agent::new(RunParams::plain("gemma4:12b", wd), Finish, OkTools)
        .with_skill_dirs(vec![wd.join("skills")])
        .with_sink(move |ev| {
            if let AgentEvent::Log(l) = ev {
                logs.lock().expect("lock").push(l);
            }
        })
}

#[test]
fn matching_skills_enter_the_task_prompt_and_are_logged() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    skill(&wd.join("skills"), "finance", "triggers: ledger, invoice\n");
    skill(&wd.join("skills"), "unrelated", "triggers: telescope\n");
    let logs = Arc::new(Mutex::new(Vec::new()));
    let mut a = agent(wd, logs.clone());
    let m = a.task_messages("Reconcile the March ledger against the bank export");
    assert_eq!(m[0].role, Role::System);
    assert!(
        m[0].content.contains("### SKILL: finance")
            && m[0].content.contains("Do the finance thing.")
    );
    assert!(!m[0].content.contains("SKILL: unrelated"));
    assert_eq!(
        logs.lock().expect("lock").clone(),
        vec!["[skills in effect: finance]"]
    );
    // No trigger, no prefill, no log line.
    let m = a.task_messages("Write a haiku about rain");
    assert!(!m[0].content.contains("SKILL IN EFFECT"));
    assert_eq!(logs.lock().expect("lock").len(), 1);
}

#[test]
fn child_mode_ignores_skills_and_system_md() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    skill(&wd.join("skills"), "house", "autoload: true\n");
    write(wd, "SYSTEM.md", "You are the finance desk.");
    let mut p = RunParams::plain("gemma4:12b", wd);
    p.child_mode = true;
    let mut a = Agent::new(p, Finish, OkTools).with_skill_dirs(vec![wd.join("skills")]);
    let m = a.task_messages("[simple] convert a.csv to a.md");
    assert!(m[0].content.starts_with(prompt::CHILD_SYSTEM));
    assert!(!m[0].content.contains("SKILL") && !m[0].content.contains("finance desk"));
}

#[test]
fn question_turns_carry_the_project_instructions() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    write(
        wd,
        "SYSTEM.md",
        "Answer as the finance desk; figures in GBP.",
    );
    let mut a = agent(wd, Arc::new(Mutex::new(Vec::new())));
    let msgs = a.task_messages("What does EBITDA stand for?");
    let out = a.run(msgs, None, None).expect("loop");
    // Q&A swapped in the chat prompt, and the project text came with it.
    assert!(out[0].role == Role::System && out[0].content.contains("chatting with the user"));
    assert!(
        out[0].content.contains("figures in GBP"),
        "{}",
        out[0].content
    );
    let _ = json!(null);
}
