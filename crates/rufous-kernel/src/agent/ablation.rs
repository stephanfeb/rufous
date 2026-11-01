//! The bench's ablation switches, in one place.
//!
//! An ablation names one kernel compensation and runs the loop without it, so a bench score
//! with and without is that compensation's contribution. The kernel is what the loop does
//! around the model because small models stumble; it is not the loop's infrastructure (tool
//! dispatch, compaction and pruning, the iteration cap and the end-of-turn rules, the safety
//! rings) and not the product policy ([`super::policy::TurnPolicy`]). Every compensation
//! therefore carries a switch, each switch is listed here with the mechanism it turns off,
//! and [`KERNEL_OFF`] is every switch at once: the only arm that measures the kernel, and it
//! must be reachable for the models the kernel exists for. A compensation without a switch
//! is a measurement the project cannot make, which is how Mingbird's `_small_model` rule
//! (`edit_file` hidden from every ≤4B model) ran unmeasured until 2026-10-30; measured under
//! its switch `small_model`, it was a net loss and was removed (bead boubou-6uj), so that
//! name is now reported as unknown and the folders recorded under it stay readable.
//! Switching a compensation off leaves the model the raw loop, never a half state the loop
//! then punishes: a tool the face will refuse is never advertised, on any path
//! ([`super::face::ToolFace::active_defs`]).
//!
//! `AGENT_ABLATION=a,b,c` is read into `RunParams::ablation`; the loop asks
//! `params.ablated(SWITCH)`.

/// Every finish gate (`Gates` empty for the turn): hollow finish, test re-run, claims,
/// named files, plan bookkeeping, wrap-up.
pub const FINISH_GATE: &str = "finish_gate";
/// Same as [`FINISH_GATE`]; kept because earlier bench runs are recorded under this name.
pub const FINISH_GATE_TEXT_ONLY: &str = "finish_gate_text_only";
/// The failure hint after a re-run check is reduced to its last output line.
pub const VERIFY_FEEDBACK: &str = "verify_feedback";
/// The corrector: the System-Two verdict ledger over repeated failing calls.
pub const CORRECTOR: &str = "corrector";
/// Category prefill: the model sees every active tool instead of the task's categories.
pub const FLAT_PREFILL: &str = "flat_prefill";
/// The call-pattern ladders: duplicate-call block, repeat streaks, idle nudges, the failure
/// ladder (missing-argument repair, redirect hints, the three-failure `edit_file` disable),
/// the research and plan-tool streaks, the shotgun-edit hint, the stale-plan reminder.
pub const ANTI_LOOP: &str = "anti_loop";
/// Format-leak salvage: a tool call the model wrote as text is not recovered, including
/// the profile's finish-prose rescue.
pub const SALVAGE: &str = "salvage";
/// The profile's workdir-prefix rule: a path under an invented subdirectory named like the
/// working directory is left as given (gemma4:e2b, bead boubou-8e6).
pub const PATH_REBASE: &str = "path_rebase";
/// `edit_file`'s matcher ([`crate::edit_match`]): the tiers, occurrence counting, the
/// already-applied rule and the nearest-region miss; off, Mingbird's exact-after-unescape
/// matcher with the file-head preview (bead boubou-bk9.3).
pub const EDIT_MATCH: &str = "edit_match";
/// The environment line of the task prompt (date, git state, local tools on PATH; bead
/// boubou-bk9.5): off, the model is told the working directory and the platform only.
pub const ENV_BLOCK: &str = "env_block";
/// The scope gate ([`crate::scope`], bead boubou-6x3): off, a tracker or docs instruction
/// is never asked about when the model starts writing code.
pub const SCOPE_GATE: &str = "scope_gate";
/// Re-attribution (bead boubou-alj): an earlier turn's reply after a write the user
/// declined is sent to the model as a harness ledger line instead of its own words.
pub const REATTRIBUTE: &str = "reattribute";
/// Every switch above at once.
pub const KERNEL_OFF: &str = "kernel";

/// The switches [`KERNEL_OFF`] expands to.
pub const KERNEL: &[&str] = &[
    FINISH_GATE,
    FINISH_GATE_TEXT_ONLY,
    VERIFY_FEEDBACK,
    CORRECTOR,
    FLAT_PREFILL,
    ANTI_LOOP,
    SALVAGE,
    PATH_REBASE,
    EDIT_MATCH,
    ENV_BLOCK,
    SCOPE_GATE,
    REATTRIBUTE,
];

/// Whether `flag` is off under `listed` (the parsed `AGENT_ABLATION`).
pub fn covers(listed: &[String], flag: &str) -> bool {
    listed
        .iter()
        .any(|a| a == flag || (a == KERNEL_OFF && KERNEL.contains(&flag)))
}

/// The switches `listed` turns off, expanded and in [`KERNEL`] order, and the names it
/// carries that are not switches (a typo turns nothing off and must be visible).
pub fn describe(listed: &[String]) -> (Vec<&'static str>, Vec<String>) {
    let off = KERNEL
        .iter()
        .copied()
        .filter(|f| covers(listed, f))
        .collect();
    let unknown = listed
        .iter()
        .filter(|a| a.as_str() != KERNEL_OFF && !KERNEL.contains(&a.as_str()))
        .cloned()
        .collect();
    (off, unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn kernel_is_every_switch_and_nothing_is_off_by_default() {
        assert!(!covers(&[], FINISH_GATE));
        let k = list(&[KERNEL_OFF]);
        for f in KERNEL {
            assert!(covers(&k, f), "{f} must be off under kernel");
        }
        assert!(covers(&list(&[PATH_REBASE]), PATH_REBASE));
        assert!(!covers(&list(&[PATH_REBASE]), ANTI_LOOP));
    }

    #[test]
    fn a_misspelt_switch_is_reported_and_turns_nothing_off() {
        let (off, unknown) = describe(&list(&["anti-loop", SALVAGE]));
        assert_eq!(off, vec![SALVAGE]);
        assert_eq!(unknown, vec!["anti-loop".to_string()]);
        let (off, unknown) = describe(&list(&[KERNEL_OFF]));
        assert_eq!(off, KERNEL.to_vec());
        assert!(unknown.is_empty());
    }
}
