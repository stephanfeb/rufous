//! The executor's word on each call as data (bead boubou-bj4): the gate's refusals carry
//! their kind and who decided, a tool's own failure is an error, a landed call is ok, and
//! none of it is read back out of the text.

use rufous_kernel::exec::{Executor, ToolStatus};
use rufous_kernel::safety::{
    ConfirmRequest, Confirmer, Gate, RefusalKind, Refused, SafetyPolicy, Verdict,
};
use rufous_kernel::scope::{Scope, ScopeGateMode};
use serde_json::json;

struct Deny;

impl Confirmer for Deny {
    fn confirm(&mut self, _req: &ConfirmRequest<'_>) -> Verdict {
        Verdict::Deny
    }
}

#[test]
fn every_outcome_has_a_status_and_a_tag() {
    let d = tempfile::tempdir().expect("tmp");
    let outside = tempfile::tempdir().expect("tmp");
    let mut ex = Executor::new(d.path());
    ex.gate = Gate::new(SafetyPolicy::default(), Box::new(Deny));

    ex.run_tool("create_file", &json!({"path": "a.txt", "content": "x"}));
    assert_eq!(ex.last_status(), ToolStatus::Ok);
    assert_eq!(ex.last_status().tag(), None);

    // The old text is gone and the new text is already there: the edit is already done.
    let r = ex.run_tool(
        "edit_file",
        &json!({"path": "a.txt", "old": "q", "new": "x"}),
    );
    assert_eq!(ex.last_status(), ToolStatus::NoChange, "{r}");

    ex.run_tool(
        "edit_file",
        &json!({"path": "a.txt", "old": "zzz", "new": "y"}),
    );
    assert_eq!(ex.last_status(), ToolStatus::Error);
    assert_eq!(ex.last_status().tag().as_deref(), Some("error"));

    ex.run_tool("read_file", &json!({"path": "missing.txt"}));
    assert_eq!(ex.last_status(), ToolStatus::Error);

    ex.run_tool("create_file", &json!({"content": "no path"}));
    assert_eq!(
        ex.last_status(),
        ToolStatus::Error,
        "a missing argument is an error, not a refusal"
    );

    let target = outside.path().join("b.txt").to_string_lossy().into_owned();
    ex.run_tool("create_file", &json!({"path": target, "content": "x"}));
    assert_eq!(
        ex.last_status(),
        ToolStatus::Refused(Refused {
            kind: RefusalKind::Boundary,
            by_user: true
        })
    );
    assert_eq!(
        ex.last_status().tag().as_deref(),
        Some("refused:boundary:user")
    );

    ex.run_tool("create_file", &json!({"path": "b.txt", "content": "x"}));
    assert_eq!(
        ex.last_status(),
        ToolStatus::Refused(Refused {
            kind: RefusalKind::Substitute,
            by_user: true
        })
    );

    ex.run_tool("run_bash", &json!({"command": "rm -rf /"}));
    assert_eq!(
        ex.last_status(),
        ToolStatus::Refused(Refused::gate(RefusalKind::Danger))
    );

    ex.run_tool("run_bash", &json!({"command": "echo ok"}));
    assert_eq!(
        ex.last_status(),
        ToolStatus::Ok,
        "a shell command that ran is ok whatever it printed"
    );

    ex.gate.set_scope(
        Some(Scope::of("Break the specs into beads.")),
        ScopeGateMode::On,
    );
    ex.run_tool(
        "create_file",
        &json!({"path": "lib/x.py", "content": "x = 1"}),
    );
    assert_eq!(
        ex.last_status(),
        ToolStatus::Refused(Refused {
            kind: RefusalKind::Scope,
            by_user: true
        })
    );
    assert_eq!(
        ex.last_status().tag().as_deref(),
        Some("refused:scope:user")
    );

    // A headless gate decides alone.
    let mut headless = Executor::new(d.path());
    let target = outside.path().join("c.txt").to_string_lossy().into_owned();
    headless.run_tool("create_file", &json!({"path": target, "content": "x"}));
    assert_eq!(
        headless.last_status(),
        ToolStatus::Refused(Refused::gate(RefusalKind::Boundary))
    );
    assert_eq!(
        headless.last_status().tag().as_deref(),
        Some("refused:boundary")
    );
}

#[test]
fn the_text_rule_for_runners_without_a_word_follows_the_executor() {
    assert_eq!(
        ToolStatus::of_text("[created x.txt (3 bytes)]"),
        ToolStatus::Ok
    );
    assert_eq!(ToolStatus::of_text("[exit 1]\nSTDOUT:\n"), ToolStatus::Ok);
    assert_eq!(
        ToolStatus::of_text("[no change: already"),
        ToolStatus::NoChange
    );
    assert_eq!(ToolStatus::of_text("[tool error: x"), ToolStatus::Error);
    assert_eq!(
        ToolStatus::of_text("[edit failed: old text not found"),
        ToolStatus::Error
    );
    assert_eq!(
        ToolStatus::of_text("[overwrite guard: 'a'"),
        ToolStatus::Error
    );
    assert_eq!(
        ToolStatus::of_text("[safety gate: the user declined this call: create_file on /x]"),
        ToolStatus::Refused(Refused::gate(RefusalKind::Boundary))
    );
    assert_eq!(
        ToolStatus::of_text("[scope gate: the instruction was"),
        ToolStatus::Refused(Refused::gate(RefusalKind::Scope))
    );
    assert_eq!(
        ToolStatus::of_text("[safety net: package uninstall"),
        ToolStatus::Refused(Refused::gate(RefusalKind::Risk))
    );
}
