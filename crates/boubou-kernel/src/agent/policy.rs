//! The turn policy: what the harness does around the model, as one named value.
//!
//! The loop in [`super::run`] is two things. One is the small-model kernel: format-leak
//! salvage, argument coercion, the duplicate, streak and idle ladders, the empty-reply
//! ladder, the integrity and honest-failure checks, the corrector, compaction, the five
//! safety rings and the per-model profiles. The other is a policy about the turn: whether a
//! plain text reply ends it, whether `finish` has to pass the delivery gates, whether a
//! question is routed away from the producing tools, whether a plan may be farmed out to
//! child processes. The kernel is the same for every product; the policy is what differs
//! between a conversation, an editor turn and an unattended task run.
//!
//! [`TurnPolicy`] names that second thing so it is set in one place and read by name,
//! instead of through a spread of booleans (`docs/refactor.md`). Today the three presets
//! carry exactly the two behaviours the loop had before the struct existed:
//! [`TurnPolicy::unattended`] is Mingbird's batch harness (the old default), and
//! [`TurnPolicy::general`] and [`TurnPolicy::coding`] are both the old editor/chat
//! semantics (`interactive = true`). The two products are expected to pull `general` and
//! `coding` apart; until then they are equal by construction and the tests say so.

/// What the harness does around the model for one turn. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnPolicy {
    /// The preset's name, for logs and the decision sidecar.
    pub name: &'static str,
    /// A text reply with no tool call ends the turn as the answer, and two empty replies in
    /// a row end it too. Off, the loop pushes a "continue with tools" note and resets the
    /// context after repeated empty replies, as the batch harness does.
    pub text_ends_turn: bool,
    /// `finish` has to pass the delivery gates: something delivered, the workdir's tests,
    /// claimed artefacts present, named deliverables present, the plan in sync, the
    /// pre-delivery re-read, the closing wrap-up call. Off, `finish` is accepted as is.
    pub finish_gates: bool,
    /// The decider's intent routes the turn: a question takes the Q&A prompt and loses the
    /// producing tools, a follow-up ends once acted on and reported. Off, every turn takes
    /// the task prompt and the full tool set, so an instruction phrased as a question can
    /// still write files.
    pub intent_routing: bool,
    /// A plan with enough simple items may be dispatched to sandboxed child processes when
    /// `parallel` is configured. Off, nothing is dispatched into the user's project.
    pub parallel_dispatch: bool,
}

impl TurnPolicy {
    /// Mingbird's unattended task run (`boubou run`, children, the web UI's batch mode):
    /// the finish gates, the Q&A and follow-up routes, continue notes and child dispatch.
    pub const fn unattended() -> Self {
        Self {
            name: "unattended",
            text_ends_turn: false,
            finish_gates: true,
            intent_routing: true,
            parallel_dispatch: true,
        }
    }

    /// A conversation with a general-purpose assistant (the web UI, chat). The turn ends
    /// when the model replies in text or calls `finish`; `finish` is accepted as is; every
    /// turn takes the task prompt and the full tool set; nothing is dispatched.
    pub const fn general() -> Self {
        Self {
            name: "general",
            text_ends_turn: true,
            finish_gates: false,
            intent_routing: false,
            parallel_dispatch: false,
        }
    }

    /// An editor turn over the user's project (the ACP server). The same turn semantics as
    /// [`Self::general`] today: the gates exist for batch tasks and would run the user's own
    /// test suite and bounce the turn, and dispatch would write into their project.
    pub const fn coding() -> Self {
        Self {
            name: "coding",
            ..Self::general()
        }
    }

    /// The batch harness, as opposed to a conversation or an editor turn.
    pub fn is_unattended(&self) -> bool {
        *self == Self::unattended()
    }

    /// One line for the run log: the name and what it switches.
    pub fn describe(&self) -> String {
        let on_off = |b: bool| if b { "on" } else { "off" };
        format!(
            "{}: text ends turn {}, finish gates {}, intent routing {}, dispatch {}",
            self.name,
            on_off(self.text_ends_turn),
            on_off(self.finish_gates),
            on_off(self.intent_routing),
            on_off(self.parallel_dispatch)
        )
    }
}

impl Default for TurnPolicy {
    fn default() -> Self {
        Self::unattended()
    }
}

#[cfg(test)]
mod tests {
    use super::TurnPolicy;

    #[test]
    fn the_two_product_presets_are_equal_until_they_diverge() {
        // The struct is the seam, not yet a difference: both products have the old editor
        // semantics. When this assertion is dropped, docs/refactor.md says why.
        let g = TurnPolicy::general();
        let c = TurnPolicy::coding();
        assert_eq!(g.text_ends_turn, c.text_ends_turn);
        assert_eq!(g.finish_gates, c.finish_gates);
        assert_eq!(g.intent_routing, c.intent_routing);
        assert_eq!(g.parallel_dispatch, c.parallel_dispatch);
        assert_ne!(g, c, "the names differ");
    }

    #[test]
    fn unattended_is_the_batch_harness_and_the_default() {
        let u = TurnPolicy::default();
        assert!(u.is_unattended());
        assert!(!u.text_ends_turn && u.finish_gates && u.intent_routing && u.parallel_dispatch);
        assert!(!TurnPolicy::general().is_unattended());
        assert!(!TurnPolicy::coding().is_unattended());
        assert_eq!(
            TurnPolicy::coding().describe(),
            "coding: text ends turn on, finish gates off, intent routing off, dispatch off"
        );
    }
}
