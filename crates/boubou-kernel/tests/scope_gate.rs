//! The scope gate end to end through the executor (bead boubou-6x3): the ask, what Allow
//! and Deny do, unattended, shadow, and a new turn forgetting the allowance.

use std::sync::{Arc, Mutex};

use boubou_kernel::exec::Executor;
use boubou_kernel::safety::{ConfirmRequest, Confirmer, Gate, SafetyPolicy, Verdict};
use boubou_kernel::scope::{Scope, ScopeGateMode};
use serde_json::json;

/// `(tool, action, target)` of each ask.
type Asked = Arc<Mutex<Vec<(String, String, String)>>>;

/// Answers every ask with `verdict` and records what was asked.
struct Scripted {
    verdict: Verdict,
    asked: Asked,
}

impl Confirmer for Scripted {
    fn confirm(&mut self, req: &ConfirmRequest<'_>) -> Verdict {
        self.asked.lock().expect("lock").push((
            req.tool.to_string(),
            req.action.to_string(),
            req.target.clone(),
        ));
        self.verdict
    }
}

fn executor(dir: &std::path::Path, verdict: Verdict, unattended: bool) -> (Executor, Asked) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut ex = Executor::new(dir);
    let policy = if unattended {
        SafetyPolicy::unattended()
    } else {
        SafetyPolicy::default()
    };
    ex.gate = Gate::new(
        policy,
        Box::new(Scripted {
            verdict,
            asked: asked.clone(),
        }),
    );
    (ex, asked)
}

const BEADS: &str = "Break the specs down into beads, small slices, several per epic.";

#[test]
fn a_denied_drift_tells_the_model_what_the_instruction_was_and_blocks_the_write() {
    let d = tempfile::tempdir().expect("tmp");
    let (mut ex, asked) = executor(d.path(), Verdict::Deny, false);
    ex.gate.set_scope(Some(Scope::of(BEADS)), ScopeGateMode::On);
    // Tracker work and reads pass without a question.
    let out = ex.run_tool(
        "run_bash",
        &json!({"command": "echo 'bd create --title=x'"}),
    );
    assert!(out.contains("[exit 0]"), "{out}");
    let out = ex.run_tool("create_file", &json!({"path": "notes.md", "content": "n"}));
    assert!(out.starts_with("[created"), "{out}");
    assert!(asked.lock().expect("lock").is_empty());
    // The code write is asked about and, denied, refused with the instruction quoted.
    let out = ex.run_tool(
        "create_file",
        &json!({"path": "lib/key_pair_manager.dart", "content": "class K {}"}),
    );
    assert!(
        out.starts_with("[scope gate: the instruction was: \"Break the specs down into beads"),
        "{out}"
    );
    assert!(out
        .contains("this call writes code, which it did not ask for. Report what is done and stop"));
    assert!(!d.path().join("lib/key_pair_manager.dart").exists());
    let a = asked.lock().expect("lock").clone();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].0, "create_file");
    assert_eq!(a[0].1, "outside the instruction");
    assert!(a[0].2.ends_with("lib/key_pair_manager.dart"), "{}", a[0].2);
    // Denied, the gate asks again on the next drift (the user may allow it then).
    let out = ex.run_tool("run_bash", &json!({"command": "cargo build"}));
    assert!(out.starts_with("[scope gate:"), "{out}");
    assert!(out.contains("runs a build or test command"));
    assert_eq!(asked.lock().expect("lock").len(), 2);
}

#[test]
fn allow_clears_the_turn_and_a_new_scope_asks_again() {
    let d = tempfile::tempdir().expect("tmp");
    let (mut ex, asked) = executor(d.path(), Verdict::Allow, false);
    ex.gate.set_scope(Some(Scope::of(BEADS)), ScopeGateMode::On);
    let out = ex.run_tool(
        "create_file",
        &json!({"path": "lib/a.dart", "content": "a"}),
    );
    assert!(out.starts_with("[created"), "{out}");
    let out = ex.run_tool(
        "create_file",
        &json!({"path": "lib/b.dart", "content": "b"}),
    );
    assert!(out.starts_with("[created"), "{out}");
    assert_eq!(
        asked.lock().expect("lock").len(),
        1,
        "one ask covers the turn"
    );
    ex.gate.set_scope(Some(Scope::of(BEADS)), ScopeGateMode::On);
    let out = ex.run_tool(
        "create_file",
        &json!({"path": "lib/c.dart", "content": "c"}),
    );
    assert!(out.starts_with("[created"), "{out}");
    assert_eq!(
        asked.lock().expect("lock").len(),
        2,
        "a new turn asks afresh"
    );
}

#[test]
fn unattended_refuses_without_asking_and_shadow_only_notes() {
    let d = tempfile::tempdir().expect("tmp");
    let (mut ex, asked) = executor(d.path(), Verdict::Allow, true);
    ex.gate.set_scope(Some(Scope::of(BEADS)), ScopeGateMode::On);
    let out = ex.run_tool(
        "create_file",
        &json!({"path": "lib/a.dart", "content": "a"}),
    );
    assert!(out.starts_with("[scope gate:"), "{out}");
    assert!(
        asked.lock().expect("lock").is_empty(),
        "nobody to ask unattended"
    );

    let (mut ex, asked) = executor(d.path(), Verdict::Deny, false);
    ex.gate
        .set_scope(Some(Scope::of(BEADS)), ScopeGateMode::Shadow);
    let out = ex.run_tool(
        "create_file",
        &json!({"path": "lib/a.dart", "content": "a"}),
    );
    assert!(out.starts_with("[created"), "{out}");
    assert!(asked.lock().expect("lock").is_empty());
    let notes = std::mem::take(&mut ex.gate.scope_notes);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].starts_with("would ask: create_file"),
        "{}",
        notes[0]
    );
    assert!(
        notes[0].ends_with("writes code under a tracker instruction"),
        "{}",
        notes[0]
    );

    // Off, or an open class, never touches the call.
    ex.gate
        .set_scope(Some(Scope::of(BEADS)), ScopeGateMode::Off);
    let out = ex.run_tool(
        "create_file",
        &json!({"path": "lib/b.dart", "content": "b"}),
    );
    assert!(out.starts_with("[created"), "{out}");
    ex.gate.set_scope(
        Some(Scope::of("Implement the store in lib/store.dart")),
        ScopeGateMode::On,
    );
    let out = ex.run_tool(
        "create_file",
        &json!({"path": "lib/store.dart", "content": "s"}),
    );
    assert!(out.starts_with("[created"), "{out}");
    assert!(ex.gate.scope_notes.is_empty());
}
