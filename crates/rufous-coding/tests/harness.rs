//! The coding harness's own things: its preset is the kernel's conversation under its name,
//! its prompt and the named-writes gate; its prompt is the kernel's task prefill less the two
//! finish sentences (bead boubou-twe); its skills parse.

use rufous_coding::harness;
use rufous_kernel::agent::prompt::TASK_BASE;
use rufous_kernel::agent::{Gate, Gates, Prompt, TurnPolicy};

#[test]
fn the_preset_is_the_kernel_conversation_under_this_name_prompt_and_gate() {
    let p = harness::policy();
    assert_eq!(p.name, "coding");
    assert_eq!(p.prompt, Prompt::Task(harness::TASK_PROMPT));
    // Bead boubou-78s: the one gate a conversation turn takes here, a file the instruction
    // asks to write must have been written (or the reply say why not) before the turn ends.
    assert_eq!(p.gates, Gates::NONE.with(Gate::NamedWrites));
    assert_eq!(
        TurnPolicy {
            name: "conversation",
            prompt: Prompt::Task(TASK_BASE),
            gates: Gates::NONE,
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
        TASK_BASE.replace(" Then call finish.", "").replace(
            " Call finish only when answered or fully verified done.",
            ""
        ),
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
            "architecture",
            "bug_fix",
            "code_review",
            "documentation",
            "git_workflow",
            "performance",
            "python_best_practices",
            "refactor",
            "security_review",
            "test_driven"
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
