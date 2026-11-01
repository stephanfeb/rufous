//! The general-purpose harness's own things: its preset is the kernel's conversation under its name and
//! its prompt, its prompt is the kernel's task prefill less the
//! two finish sentences (bead boubou-twe), and its skills parse.

use boubou_general::harness;
use boubou_kernel::agent::prompt::TASK_BASE;
use boubou_kernel::agent::{Prompt, TurnPolicy};

#[test]
fn the_preset_is_the_kernel_conversation_under_this_name_and_prompt() {
    let p = harness::policy();
    assert_eq!(p.name, "general");
    assert_eq!(p.prompt, Prompt::Task(harness::TASK_PROMPT));
    assert_eq!(
        TurnPolicy {
            name: "conversation",
            prompt: Prompt::Task(TASK_BASE),
            ..p
        },
        TurnPolicy::conversation(),
        "every other field is the kernel's conversation"
    );
}

#[test]
fn the_prompt_is_the_kernel_task_prefill_less_its_two_finish_sentences() {
    // Bead boubou-twe: in a conversation text ends the turn, so the prefill does not ask
    // for finish. qwen3.5:4b's finish summaries read as status reports and dropped a
    // standing reply rule (2/14 kept it against 13/16 for text endings). The unattended
    // kernel prefill keeps both sentences; finish carries its gates there.
    assert_eq!(
        TASK_BASE
            .replace(" Then call finish.", "")
            .replace(" Call finish only when answered or fully verified done.", ""),
        harness::TASK_PROMPT
    );
    assert!(!harness::TASK_PROMPT.contains("finish"));
}

#[test]
fn the_built_in_skills_parse_and_are_this_harness_s() {
    let skills = harness::skills();
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "doc_digest",
            "file_organizer",
            "markdown_report",
            "web_research"
        ]
    );
    for s in &skills {
        assert!(s.embedded.is_some(), "{} is compiled in", s.name);
        assert!(!s.description.is_empty(), "{} has a description", s.name);
        let body = s.load().expect("body");
        assert!(body.len() > 100, "{} has a body", s.name);
        assert!(!body.starts_with("---"), "{}: frontmatter stripped", s.name);
    }
}

#[test]
fn chat_runs_the_general_policy() {
    // `boubou chat` and the web UI's chat are the same product at two surfaces: a text
    // reply ends the turn, with no finish gates, no Q&A route and no dispatch.
    let d = tempfile::tempdir().expect("tmp");
    let p = boubou_general::chat::params("m".into(), d.path()).expect("params");
    assert_eq!(p.policy, harness::policy());
    assert_eq!(p.workdir, d.path());
}
