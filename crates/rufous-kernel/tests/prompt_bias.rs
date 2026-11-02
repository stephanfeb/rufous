//! Deliberate domain bias: skills with `triggers`/`autoload` are prefilled into the task
//! prompt, and `<workdir>/SYSTEM.md` shapes the base prompt for task and chat turns.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rufous_kernel::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use rufous_kernel::agent::prompt::{self, PromptOptions, REPLACE_MARK, REPLACE_MARK_OLD};
use rufous_kernel::agent::{Agent, AgentEvent, RunParams, ToolRunner};
use rufous_kernel::ollama::{Message, Role};
use rufous_kernel::skills::{Skill, SkillSet, MAX_PREFILL_SKILLS};
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
        base: None,
        workdir: Some(wd),
        system_file: None,
        unattended: false,
        skills,
        skill_catalogue: None,
        profile_note: None,
        hook_context: None,
        environment: None,
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
    assert!(c.starts_with("You are rufous") && c.contains("quote figures in GBP"));

    write(
        wd,
        "SYSTEM.md",
        &format!("{REPLACE_MARK}\nYou are Tally, a bookkeeper.\n"),
    );
    let p = prompt::system_prompt(&opts(vec![]));
    assert!(p.starts_with("You are Tally, a bookkeeper."), "{p}");
    assert!(!p.contains("You are an autonomous AI agent") && !p.contains("PROJECT INSTRUCTIONS"));
    let c = prompt::chat_system(Some(wd));
    assert!(c.starts_with("You are Tally, a bookkeeper.") && !c.contains("You are rufous"));
    // A SYSTEM.md written under the name before rufous still replaces.
    write(
        wd,
        "SYSTEM.md",
        &format!("{REPLACE_MARK_OLD}\nYou are Tally, a bookkeeper.\n"),
    );
    let p = prompt::system_prompt(&opts(vec![]));
    assert!(p.starts_with("You are Tally, a bookkeeper."), "{p}");

    // The explicit override file still wins the base; the project text is not lost.
    write(wd, "override.txt", "OVERRIDE PROMPT");
    let p = prompt::system_prompt(&PromptOptions {
        base: None,
        workdir: Some(wd),
        system_file: Some(&wd.join("override.txt")),
        unattended: false,
        skills: vec![],
        skill_catalogue: None,
        profile_note: None,
        hook_context: None,
        environment: None,
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
fn a_profile_prompt_note_enters_the_task_prompt_only() {
    // bead 65d.15: the honest-failure nudge is a per-tag profile setting. gemma4:12b carries
    // it; gemma4:e2b (same family) does not. It sits in the system prompt, so the task turn
    // and the Q&A routing that reads it are untouched.
    let d = tempfile::tempdir().expect("tmp");
    let logs = Arc::new(Mutex::new(Vec::new()));
    let mut a = agent(d.path(), logs);
    let task = "Run python fetch.py and count the rows";
    let m = a.task_messages(task);
    assert_eq!(m[0].role, Role::System);
    let note_at = m[0]
        .content
        .find("MODEL PROFILE NOTE")
        .expect("note present for gemma4:12b");
    assert!(m[0].content[note_at..].contains(rufous_kernel::profile::HONEST_FAILURE_NUDGE));
    assert!(
        note_at
            < m[0]
                .content
                .find("Current working directory")
                .expect("workdir line"),
        "note comes before the dynamic workdir line"
    );
    assert_eq!(m[1].content, task, "the task turn is untouched");
    // A question still routes to Q&A, whose prompt carries no profile note.
    let q = a.task_messages("What does EBITDA stand for?");
    let out = a.run(q, None, None).expect("loop");
    assert!(out[0].content.contains("chatting with the user"));
    assert!(!out[0].content.contains("MODEL PROFILE NOTE"));

    let mut e2b = Agent::new(RunParams::plain("gemma4:e2b", d.path()), Finish, OkTools);
    let m = e2b.task_messages(task);
    assert!(!m[0].content.contains("MODEL PROFILE NOTE"));
    assert_eq!(m[1].content, task);
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

#[test]
fn built_in_skills_sit_below_every_directory_and_load_from_memory() {
    // Refactor step 11: a product compiles its skills in. They list and load like files,
    // prefill on their triggers, and a user's file of the same name shadows one.
    const LEDGER: &str = "---\nname: ledger\ndescription: Reconcile a ledger\ntriggers: ledger\n---\nBuilt-in steps.\n";
    const TIDY: &str = "---\nname: tidy\ndescription: Tidy a folder\n---\nBuilt-in tidy.\n";
    let ledger = Skill::embedded(LEDGER).expect("skill");
    assert_eq!(ledger.name, "ledger");
    assert_eq!(ledger.triggers, ["ledger"]);
    assert_eq!(ledger.path, Path::new("<built-in>/ledger.md"));
    assert_eq!(ledger.load().expect("body"), "Built-in steps.\n");
    assert!(Skill::embedded("no frontmatter").is_err());

    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    skill(&wd.join("skills"), "tidy", "");
    let builtin = vec![ledger, Skill::embedded(TIDY).expect("skill")];
    let set = SkillSet::discover([wd.join("skills")]).with_builtin(builtin.clone());
    assert_eq!(set.len(), 2);
    assert_eq!(
        set.get("tidy").expect("tidy").path,
        wd.join("skills").join("tidy.md"),
        "the workdir's file wins over the built-in of the same name"
    );
    assert_eq!(set.get("ledger").expect("ledger").embedded, Some(LEDGER));

    // Through the agent: the built-in prefills on its trigger and the tool loads it.
    let mut a = Agent::new(RunParams::plain("gemma4:12b", wd), Finish, OkTools)
        .with_skill_dirs(vec![wd.join("skills")])
        .with_builtin_skills(builtin);
    let m = a.task_messages("reconcile the ledger for March");
    assert!(
        m[0].content.contains("### SKILL: ledger\nBuilt-in steps."),
        "{}",
        m[0].content
    );
    let mut ex = rufous_kernel::exec::Executor::new(wd);
    ex.set_builtin_skills(vec![Skill::embedded(LEDGER).expect("skill")]);
    let listed = ex.run_tool("skills", &json!({"action": "list"}));
    assert!(listed.contains("ledger: Reconcile a ledger"), "{listed}");
    let loaded = ex.run_tool("skills", &json!({"action": "load", "name": "ledger"}));
    assert_eq!(loaded, "### SKILL: ledger\nBuilt-in steps.\n");
}

// ---- the SKILLS AVAILABLE block (bead boubou-15s) ----

#[test]
fn skills_without_a_trigger_are_named_in_the_task_prompt() {
    // A Claude-style skill (name and description only) was found but never named to the
    // model; in two Zed sessions it never called `skills` and so never learnt of it.
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    let home = tempfile::tempdir().expect("home");
    write(
        &wd.join(".agents").join("skills").join("beads"),
        "SKILL.md",
        "---\nname: beads\ndescription: Use when working in a repository that uses bd or Beads for durable project task tracking, issue dependencies, blocker management.\n---\nbody\n",
    );
    skill(home.path(), "zeta", "");
    skill(home.path(), "finance", "triggers: ledger\n");
    const BUILT: &str = "---\nname: tidy\ndescription: Tidy a folder\n---\nBuilt-in tidy.\n";
    let mut a = Agent::new(RunParams::plain("gemma4:12b", wd), Finish, OkTools)
        .with_skill_dirs(vec![
            wd.join(".agents").join("skills"),
            home.path().to_path_buf(),
        ])
        .with_builtin_skills(vec![Skill::embedded(BUILT).expect("skill")]);
    let m = a.task_messages("Reconcile the ledger");
    let p = &m[0].content;
    let at = p.find("SKILLS AVAILABLE").expect("block");
    let block = &p[at..p.find("Current working directory").expect("workdir")];
    // The project's own first; the one in effect and the built-in are not listed.
    assert!(
        block.contains(
            "\n- beads: Use when working in a repository that uses bd or Beads for durable project task…\n- zeta: about zeta"
        ),
        "{block}"
    );
    assert!(
        !block.contains("finance") && !block.contains("tidy"),
        "{block}"
    );
    assert!(p.find("### SKILL: finance").expect("in effect") < at);

    // The Q&A prompt carries none of it.
    let q = a.task_messages("What does EBITDA stand for?");
    let out = a.run(q, None, None).expect("loop");
    assert!(!out[0].content.contains("SKILLS AVAILABLE"));
}

#[test]
fn the_block_is_capped_and_absent_without_skills_on_disk() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    let mut a = Agent::new(RunParams::plain("gemma4:12b", wd), Finish, OkTools)
        .with_skill_dirs(vec![wd.join("skills")]);
    let m = a.task_messages("Write a haiku");
    assert!(!m[0].content.contains("SKILLS AVAILABLE"));

    for i in 0..15 {
        skill(&wd.join("skills"), &format!("s{i:02}"), "");
    }
    let set = SkillSet::discover([wd.join("skills")]);
    let c = set
        .catalogue(&[], wd, rufous_kernel::skills::MAX_CATALOGUE_SKILLS)
        .expect("block");
    assert_eq!(c.lines().filter(|l| l.starts_with("- ")).count(), 12);
    assert!(c.ends_with("\n(3 more: skills action \"list\")"), "{c}");
    assert_eq!(set.catalogue(&[], wd, 0), None);
}

#[test]
fn a_skill_added_between_turns_is_named_on_the_next_one() {
    // "Reload the agent in Zed and the new skill is not picked up": a continued session
    // rebuilds the task prompt every turn, so the next turn names it.
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    let mut p = RunParams::plain("gemma4:12b", wd);
    p.policy = rufous_kernel::agent::TurnPolicy::conversation();
    let mut a = Agent::new(p, Finish, OkTools).with_skill_dirs(vec![wd.join("skills")]);
    let msgs = a.task_messages("Tidy up the notes folder");
    let mut out = a.run(msgs, None, None).expect("turn 1");
    assert!(!out[0].content.contains("SKILLS AVAILABLE"));
    skill(&wd.join("skills"), "openspec-propose", "");
    out.push(Message::user("Now propose the change"));
    let out = a.run(out, None, None).expect("turn 2");
    assert!(
        out[0]
            .content
            .contains("\n- openspec-propose: about openspec-propose"),
        "{}",
        out[0].content
    );
}

#[test]
fn a_rebuilt_prompt_logs_whether_it_changed_and_each_call_its_timing() {
    // bead boubou-xwi: a follow-up turn waited 190 s before its first step. The log has to
    // say whether the rebuild moved the prompt's text and where the call's time went.
    use rufous_kernel::agent::chat::Timing;
    use rufous_kernel::agent::run::prompt_change;
    use std::time::Duration;
    struct Timed;
    impl ChatBackend for Timed {
        fn chat(
            &mut self,
            _m: &str,
            _msgs: &[Message],
            _t: &[Value],
            _on: &mut dyn FnMut(StreamPiece<'_>),
        ) -> Result<ChatResponse, ChatError> {
            let mut r = ChatResponse::text("Done, here is the answer.");
            r.prompt_eval_count = Some(48806);
            r.eval_count = Some(312);
            r.timing = Some(Timing {
                load: Duration::ZERO,
                prompt_eval: Duration::from_millis(160_200),
                eval: Duration::from_millis(9400),
            });
            Ok(r)
        }
    }
    let d = tempfile::tempdir().expect("tmp");
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink = logs.clone();
    let mut p = RunParams::plain("gemma4:12b", d.path());
    p.policy = rufous_kernel::agent::TurnPolicy::conversation();
    let mut a = Agent::new(p, Timed, OkTools).with_sink(move |ev| {
        if let AgentEvent::Log(l) = ev {
            sink.lock().expect("lock").push(l);
        }
    });
    let msgs = a.task_messages("Tidy up the notes folder");
    let mut out = a.run(msgs, None, None).expect("turn 1");
    out.push(Message::user("Now sort them by date"));
    a.run(out, None, None).expect("turn 2");
    let logs = logs.lock().expect("lock").join("\n");
    assert!(
        logs.contains("[timing: load 0.0s · prompt 48806 tok 160.2s · gen 312 tok 9.4s]"),
        "{logs}"
    );
    assert!(logs.contains("[prompt] system prompt "), "{logs}");

    assert_eq!(
        prompt_change("abc", "abc"),
        "system prompt unchanged (3 chars)"
    );
    let c = prompt_change("one\ntwo", "one\nTWO!");
    assert!(
        c.starts_with("system prompt changed at char 4 of 7→8: "),
        "{c}"
    );
    assert!(c.contains("one⏎two") && c.contains("one⏎TWO!"), "{c}");
    let c = prompt_change("abc", "abcdef");
    assert!(
        c.starts_with("system prompt changed at char 3 of 3→6"),
        "{c}"
    );
}
