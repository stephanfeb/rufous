//! The substitute write through the executor (bead boubou-sxx): gemma4:e4b's permission
//! sequence of 2026-10-31 (`create_file /tmp/x` declined, then `create_file x` in the
//! project) must leave no file behind and hand the model the refusal.

use boubou_kernel::exec::Executor;
use boubou_kernel::safety::{ConfirmRequest, Confirmer, Gate, SafetyPolicy, Verdict};
use serde_json::json;

struct Deny;

impl Confirmer for Deny {
    fn confirm(&mut self, _req: &ConfirmRequest<'_>) -> Verdict {
        Verdict::Deny
    }
}

#[test]
fn the_declined_file_is_not_written_into_the_project_under_its_own_name() {
    let d = tempfile::tempdir().expect("tmp");
    let outside = tempfile::tempdir().expect("tmp");
    let target = outside.path().join("boubou_convo_perm.txt");
    let mut ex = Executor::new(d.path());
    ex.gate = Gate::new(SafetyPolicy::default(), Box::new(Deny));
    let first = ex.run_tool(
        "create_file",
        &json!({"path": target.to_string_lossy(), "content": "hello"}),
    );
    assert!(first.contains("the user declined this"), "{first}");
    assert!(!target.exists());
    let second = ex.run_tool(
        "create_file",
        &json!({"path": "boubou_convo_perm.txt", "content": "hello"}),
    );
    assert!(second.contains("same thing somewhere else"), "{second}");
    assert!(!d.path().join("boubou_convo_perm.txt").exists());
    // Through the shell as well.
    let third = ex.run_tool(
        "run_bash",
        &json!({"command": "echo hello > boubou_convo_perm.txt"}),
    );
    assert!(third.contains("same thing somewhere else"), "{third}");
    assert!(!d.path().join("boubou_convo_perm.txt").exists());
    // Other work goes on.
    let other = ex.run_tool(
        "create_file",
        &json!({"path": "notes.txt", "content": "ok"}),
    );
    assert!(other.starts_with("[created"), "{other}");
}
