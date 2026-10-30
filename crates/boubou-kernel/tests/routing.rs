//! Category routing is domain-neutral: everyday words do not label a task as coding work,
//! and the built-in file and command tools are on the table for every task.

use std::collections::BTreeSet;

use boubou_kernel::agent::face::{route_categories, ToolFace};
use boubou_kernel::mcp::{CAT_CODE, CAT_FILES, CAT_WEB};
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
