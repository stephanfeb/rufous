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

use crate::guards::qa::Intent;

/// Which way a turn is routed: the policy applied to the decider's intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The task prompt, the full tool set, the task-route mechanisms.
    Task,
    /// The chat prompt and tool set; the Q&A guard, the idle cap and a text reply that is
    /// the answer. No gates, no plan, no dispatch.
    Question,
    /// The task loop, ending once the model has acted and then reported (boubou-he2): the
    /// task route's continue notes would otherwise push it into the next piece of work the
    /// user never asked for.
    FollowUp,
}

/// Which system prompt frames a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// The task prompt: the static prefill, the workdir's `SYSTEM.md`, the skills matched
    /// on the task, the profile note, the platform block.
    Task,
    /// The chat prompt for the question route: answer briefly, tools only to look things up.
    Chat,
    /// A dispatched child's sandbox prompt: one mechanical task, no skills, no `SYSTEM.md`.
    Child,
}

impl Prompt {
    pub fn name(self) -> &'static str {
        match self {
            Prompt::Task => "task",
            Prompt::Chat => "chat",
            Prompt::Child => "child",
        }
    }

    /// Whether the turn rebuilds its system prompt instead of keeping what the history
    /// carries. The chat prompt is rebuilt every turn (a continued session would otherwise
    /// keep the previous turn's task prompt). The task prompt is rebuilt only for a
    /// continued session: a fresh run already carries it, and on a continued one the
    /// previous turn may have left the chat prompt (bead boubou-c4b) and the skills are
    /// matched on this turn's input too. A child's prompt is fixed.
    pub fn rebuild(self, continued: bool) -> bool {
        match self {
            Prompt::Chat => true,
            Prompt::Task => continued,
            Prompt::Child => false,
        }
    }
}

/// What the harness does on one turn: the policy applied to the decider's intent, child
/// mode and the bench's ablations, derived once by [`TurnPolicy::turn`] and read by name.
/// The loop reads these fields; it never recombines the inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turn {
    pub route: Route,
    /// The system prompt that frames the turn.
    pub prompt: Prompt,
    /// The model is working a task rather than answering: budget pacing, the stale-plan
    /// reminder, the plan note on compaction, small-model rewrite mode and the corrector
    /// apply.
    pub working: bool,
    /// `finish` meets the delivery gates: something delivered, the workdir's tests, the
    /// summary's claims, the closing wrap-up. Off on the question route, under a policy
    /// without gates, and under the finish-gate ablations.
    pub finish_gates: bool,
    /// The plan bookkeeping gates on top of `finish_gates`: files the task names, files the
    /// plan names, the plan in sync, the pre-delivery re-read. Children are exempt: one
    /// mechanical task, no plan to maintain, and the dispatcher checks their exit contract.
    pub plan_gates: bool,
    /// A plan may be dispatched to children this turn (the `parallel` config permitting).
    pub dispatch: bool,
}

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

    /// The route for a turn whose opening message the decider read as `intent`. A child
    /// always runs a task, and so does every turn under a policy without intent routing.
    pub fn route(&self, intent: Intent, child: bool) -> Route {
        if child || !self.intent_routing {
            return Route::Task;
        }
        match intent {
            Intent::Question => Route::Question,
            Intent::FollowUp => Route::FollowUp,
            Intent::Task => Route::Task,
        }
    }

    /// Everything the loop needs to know about this turn, derived once. `gates_ablated` is
    /// the bench's `finish_gate` / `finish_gate_text_only` ablation.
    pub fn turn(&self, intent: Intent, child: bool, gates_ablated: bool) -> Turn {
        let route = self.route(intent, child);
        let working = route != Route::Question;
        let finish_gates = working && self.finish_gates && !gates_ablated;
        let prompt = if child {
            Prompt::Child
        } else if route == Route::Question {
            Prompt::Chat
        } else {
            Prompt::Task
        };
        Turn {
            route,
            prompt,
            working,
            finish_gates,
            plan_gates: finish_gates && !child,
            dispatch: working && !child && self.parallel_dispatch,
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
    use super::{Intent, Prompt, Route, Turn, TurnPolicy};

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
    fn the_route_follows_the_intent_only_where_the_policy_routes() {
        let u = TurnPolicy::unattended();
        assert_eq!(u.route(Intent::Question, false), Route::Question);
        assert_eq!(u.route(Intent::FollowUp, false), Route::FollowUp);
        assert_eq!(u.route(Intent::Task, false), Route::Task);
        // A child always runs a task, whatever the decider read.
        assert_eq!(u.route(Intent::Question, true), Route::Task);
        // A conversation or an editor turn takes the task route for every intent.
        for p in [TurnPolicy::general(), TurnPolicy::coding()] {
            assert_eq!(p.route(Intent::Question, false), Route::Task);
            assert_eq!(p.route(Intent::FollowUp, false), Route::Task);
        }
    }

    #[test]
    fn the_turn_is_the_policy_applied_to_intent_child_and_ablation() {
        let u = TurnPolicy::unattended();
        assert_eq!(
            u.turn(Intent::Task, false, false),
            Turn {
                route: Route::Task,
                prompt: Prompt::Task,
                working: true,
                finish_gates: true,
                plan_gates: true,
                dispatch: true
            }
        );
        // A question: the answer is the chat; nothing is gated, paced or dispatched.
        assert_eq!(
            u.turn(Intent::Question, false, false),
            Turn {
                route: Route::Question,
                prompt: Prompt::Chat,
                working: false,
                finish_gates: false,
                plan_gates: false,
                dispatch: false
            }
        );
        // A follow-up works and is gated like a task; only its ending differs.
        let f = u.turn(Intent::FollowUp, false, false);
        assert_eq!(f.route, Route::FollowUp);
        assert!(f.working && f.finish_gates && f.plan_gates && f.dispatch);
        // A child: delivery gates but no plan bookkeeping and no dispatch of its own.
        assert_eq!(
            u.turn(Intent::Task, true, false),
            Turn {
                route: Route::Task,
                prompt: Prompt::Child,
                working: true,
                finish_gates: true,
                plan_gates: false,
                dispatch: false
            }
        );
        // The ablation switches every gate off, the plan bookkeeping included.
        let a = u.turn(Intent::Task, false, true);
        assert!(a.working && !a.finish_gates && !a.plan_gates && a.dispatch);
        // A conversation: working, nothing gated, nothing dispatched.
        let g = TurnPolicy::general().turn(Intent::Question, false, false);
        assert!(g.route == Route::Task && g.working && !g.finish_gates && !g.dispatch);
    }

    #[test]
    fn the_prompt_follows_the_route_and_a_child_keeps_its_own() {
        let u = TurnPolicy::unattended();
        assert_eq!(u.turn(Intent::Task, false, false).prompt, Prompt::Task);
        assert_eq!(u.turn(Intent::FollowUp, false, false).prompt, Prompt::Task);
        assert_eq!(u.turn(Intent::Question, false, false).prompt, Prompt::Chat);
        assert_eq!(u.turn(Intent::Question, true, false).prompt, Prompt::Child);
        // A conversation or an editor turn: the task prompt for a question too, so an
        // instruction phrased as a question keeps the producing tools.
        assert_eq!(
            TurnPolicy::general()
                .turn(Intent::Question, false, false)
                .prompt,
            Prompt::Task
        );
        // Rebuilding: chat every turn, task on a continued session, child never.
        assert!(Prompt::Chat.rebuild(false) && Prompt::Chat.rebuild(true));
        assert!(!Prompt::Task.rebuild(false) && Prompt::Task.rebuild(true));
        assert!(!Prompt::Child.rebuild(false) && !Prompt::Child.rebuild(true));
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
