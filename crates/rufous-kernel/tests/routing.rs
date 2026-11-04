//! Category routing is domain-neutral: everyday words do not label a task as coding work,
//! and the built-in file and command tools are on the table for every task.

use std::collections::BTreeSet;

use rufous_kernel::agent::face::{route_categories, session_categories, ToolFace};
use rufous_kernel::mcp::{CAT_CODE, CAT_FILES, CAT_WEB};
use serde_json::Value;

fn names(defs: &[Value]) -> Vec<String> {
    defs.iter()
        .filter_map(|d| d["function"]["name"].as_str().map(String::from))
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn everyday_words_do_not_route_to_code() {
    // Each of these used to hit the code list through fix / test / error / calculate / run.
    for t in [
        "Calculate the quarterly totals in sales.csv and write a short report",
        "Fix the typos in the minutes and save the document",
        "Run a quick comparison of the three suppliers and write up the result",
        "Explain this error message from the printer and document the fix",
        "Test the three slogans against the brief and write the verdict to notes.md",
        "Draft a training programme for new volunteers and save it as a file",
    ] {
        let c = route_categories(t);
        assert!(!c.contains(CAT_CODE), "{t}: {c:?}");
        assert!(c.contains(CAT_FILES), "{t}: {c:?}");
    }
}

#[test]
fn programming_terms_still_route_to_code() {
    for t in [
        "Fix the failing test in m08.py and run pytest",
        "Write a Python script that parses the access log",
        "Debug the traceback from last night's job",
        "Refactor the parser and compile it again",
        "Clone the git repository and list the branches",
    ] {
        let c = route_categories(t);
        assert!(c.contains(CAT_CODE), "{t}: {c:?}");
        assert!(c.contains(CAT_FILES), "code implies files: {t}: {c:?}");
    }
}

#[test]
fn nothing_matched_keeps_the_wide_default() {
    assert_eq!(
        route_categories("Help me think through my options for next year"),
        set(&[CAT_FILES, CAT_CODE, CAT_WEB])
    );
}

#[test]
fn file_and_command_tools_are_offered_whatever_the_routing() {
    // A research task routes to web only; the built-in files/code tools come anyway, since
    // nothing else can unlock run_bash or create_file later in the run.
    let c = route_categories("Research the latest news on tidal energy");
    assert_eq!(c, set(&[CAT_WEB]));
    let mut face = ToolFace::new();
    let n = names(&face.tools_for_categories(&c));
    for t in [
        "read_file",
        "list_dir",
        "todo",
        "finish",
        "create_file",
        "edit_file",
        "run_bash",
        "web_search",
        "web_fetch",
    ] {
        assert!(n.contains(&t.to_string()), "missing {t} in {n:?}");
    }
    // Memory stays behind its own routing (or enable_tools).
    assert!(!n.contains(&"memory_store".to_string()), "{n:?}");
    // Files-only routing (no web words) still has run_bash.
    let c = route_categories("Fix the typos in the minutes and save the document");
    let n = names(&face.tools_for_categories(&c));
    assert!(n.contains(&"run_bash".to_string()) && !n.contains(&"web_search".to_string()));
}

#[test]
fn a_session_keeps_every_category_its_instructions_routed_to() {
    // bead boubou-g11, acp_1104_132038: a keyworded follow-up dropped the web tools and
    // 'proceed' brought them back, each costing a full prefill.
    let turns = [
        "Which of the beads are complete and can be closed ?",
        "Refactor the python module into two files",
        "proceed",
    ];
    assert!(!route_categories(turns[1]).contains(CAT_WEB));
    for n in 1..=turns.len() {
        let c = session_categories(&turns[..n]);
        assert!(c.contains(CAT_WEB) && c.contains(CAT_FILES), "{n}: {c:?}");
    }
    assert!(session_categories(&turns[1..2]).contains(CAT_CODE));
    assert!(session_categories::<&str>(&[]).is_empty());
}

#[test]
fn harness_notes_and_the_summary_are_not_instructions() {
    use rufous_kernel::guards::qa::{instruction_texts, CONTEXT_SUMMARY_MARK};
    use rufous_kernel::ollama::Message;
    let history = vec![
        Message::system("sys"),
        Message::user("Refactor the python module"),
        Message::assistant("ok"),
        Message::user("⚠️ Plan progress: search the web next"),
        Message::user(format!(
            "{CONTEXT_SUMMARY_MARK} the user asked to research a topic"
        )),
        Message::tool_output("web search results"),
        Message::user("proceed"),
    ];
    assert_eq!(
        instruction_texts(&history),
        vec!["Refactor the python module", "proceed"]
    );
}
