//! The coding harness's own things: its preset is the kernel's conversation under its name and
//! its prompt, its prompt is still the kernel's task prefill, and its skills parse.

use boubou_coding::harness;
use boubou_kernel::agent::prompt::TASK_BASE;
use boubou_kernel::agent::{Prompt, TurnPolicy};

#[test]
fn the_preset_is_the_kernel_conversation_under_this_name_and_prompt() {
    let p = harness::policy();
    assert_eq!(p.name, "coding");
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
fn the_prompt_is_the_kernel_task_prefill_until_measurement_says_otherwise() {
    // Refactor step 10 replaces this test with one stating the difference and why.
    assert_eq!(harness::TASK_PROMPT, TASK_BASE);
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
