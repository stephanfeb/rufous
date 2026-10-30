//! The turn policy: what the harness does around the model, as one named value.
//!
//! The loop in [`super::run`] is two things. One is the small-model kernel: format-leak
//! salvage, argument coercion, the duplicate, streak and idle ladders, the empty-reply
//! ladder, the integrity and honest-failure checks, the corrector, compaction, the five
//! safety rings and the per-model profiles. The other is a policy about the turn: whether a
//! plain text reply ends it, which delivery gates `finish` has to pass, whether a question
//! is routed away from the producing tools, whether a plan may be farmed out to child
//! processes. The kernel is the same for every product; the policy is what differs between
//! a conversation, an editor turn and an unattended task run.
//!
//! [`TurnPolicy`] names that second thing so it is set in one place and read by name,
//! instead of through a spread of booleans (`docs/refactor.md`). Today the three presets
//! carry exactly the two behaviours the loop had before the struct existed:
//! [`TurnPolicy::unattended`] is Mingbird's batch harness (the old default), and
//! [`TurnPolicy::general`] and [`TurnPolicy::coding`] are both the old editor/chat
//! semantics (`interactive = true`). The two products are expected to pull `general` and
//! `coding` apart; until then they are equal by construction and the tests say so.

use std::fmt;

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

/// One finish gate: a check `finish` has to pass before the turn may end. Each one is a
/// block in the loop's `finish` handling; the numbers are the loop's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Gate 1: something was delivered: a file or command, body text, or a substantive
    /// summary (two refusals).
    Delivery,
    /// Gate 2: the workdir's Python tests pass (three refusals, then let through).
    Tests,
    /// Gate 2b: the summary claims no verified outcome the turn's commands do not back,
    /// and reports no success over a failed check (two refusals).
    Claims,
    /// Gate 3: files the summary claims exist, unless it reports them as failed (two
    /// refusals).
    ClaimedFiles,
    /// Gate 3b: files the task names exist, unless the summary reports them as failed (one
    /// refusal).
    NamedFiles,
    /// Gate 4: files the plan names exist (two refusals).
    PlanFiles,
    /// Gate 5: the plan is synced (two refusals; waived, and the plan synced instead, when
    /// the turn's last command was a passing check).
    PlanSync,
    /// Gate 6: once, the original task is re-injected for a self-check (skipped after a
    /// passing check).
    ReRead,
    /// A bare finish, no body and no summary, gets one tool-less call so the user sees a
    /// wrap-up.
    WrapUp,
}

impl Gate {
    /// Every gate, in the order the loop runs them.
    pub const ALL: [Gate; 9] = [
        Gate::Delivery,
        Gate::Tests,
        Gate::Claims,
        Gate::ClaimedFiles,
        Gate::NamedFiles,
        Gate::PlanFiles,
        Gate::PlanSync,
        Gate::ReRead,
        Gate::WrapUp,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Gate::Delivery => "delivery",
            Gate::Tests => "tests",
            Gate::Claims => "claims",
            Gate::ClaimedFiles => "claimed files",
            Gate::NamedFiles => "named files",
            Gate::PlanFiles => "plan files",
            Gate::PlanSync => "plan sync",
            Gate::ReRead => "re-read",
            Gate::WrapUp => "wrap-up",
        }
    }

    /// Plan bookkeeping: the gates a dispatched child is exempt from. One mechanical task,
    /// no plan to maintain; its exit contract (artefacts and `finish`) is checked by the
    /// dispatcher, and a blocked child only burns its budget into a timeout.
    pub const fn plan_bookkeeping(self) -> bool {
        matches!(
            self,
            Gate::NamedFiles | Gate::PlanFiles | Gate::PlanSync | Gate::ReRead
        )
    }

    const fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// A set of finish gates. Built in `const` context so a preset can list its gates.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Gates(u16);

impl Gates {
    pub const NONE: Gates = Gates(0);
    pub const ALL: Gates = Gates((1 << Gate::ALL.len()) - 1);

    pub const fn with(self, g: Gate) -> Gates {
        Gates(self.0 | g.bit())
    }

    pub const fn without(self, g: Gate) -> Gates {
        Gates(self.0 & !g.bit())
    }

    pub const fn has(self, g: Gate) -> bool {
        self.0 & g.bit() != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The set a dispatched child runs under: the plan bookkeeping gates removed.
    pub fn without_plan_bookkeeping(self) -> Gates {
        Gate::ALL
            .into_iter()
            .filter(|g| g.plan_bookkeeping())
            .fold(self, Gates::without)
    }

    pub fn names(self) -> Vec<&'static str> {
        Gate::ALL
            .into_iter()
            .filter(|g| self.has(*g))
            .map(Gate::name)
            .collect()
    }
}

impl fmt::Debug for Gates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Gates[{}]", self.names().join(", "))
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
    /// The finish gates this turn runs: the policy's, none on the question route or under
    /// the finish-gate ablations, and without the plan bookkeeping for a child.
    pub gates: Gates,
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
    /// The finish gates `finish` has to pass ([`Gate`]). Empty, `finish` is accepted as is.
    pub gates: Gates,
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
    /// every finish gate, the Q&A and follow-up routes, continue notes and child dispatch.
    pub const fn unattended() -> Self {
        Self {
            name: "unattended",
            text_ends_turn: false,
            gates: Gates::ALL,
            intent_routing: true,
            parallel_dispatch: true,
        }
    }

    /// A conversation with a general-purpose assistant (the web UI, `boubou chat`). The
    /// turn ends when the model replies in text or calls `finish`; `finish` is accepted as
    /// is; every turn takes the task prompt and the full tool set; nothing is dispatched.
    pub const fn general() -> Self {
        Self {
            name: "general",
            text_ends_turn: true,
            gates: Gates::NONE,
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
        let gates = if !working || gates_ablated {
            Gates::NONE
        } else if child {
            self.gates.without_plan_bookkeeping()
        } else {
            self.gates
        };
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
            gates,
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
        let gates = if self.gates == Gates::ALL {
            "all".to_string()
        } else if self.gates.is_empty() {
            "none".to_string()
        } else {
            self.gates.names().join("+")
        };
        format!(
            "{}: text ends turn {}, finish gates {}, intent routing {}, dispatch {}",
            self.name,
            on_off(self.text_ends_turn),
            gates,
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
    use super::{Gate, Gates, Intent, Prompt, Route, Turn, TurnPolicy};

    #[test]
    fn the_two_product_presets_are_equal_until_they_diverge() {
        // The struct is the seam, not yet a difference: both products have the old editor
        // semantics. When this assertion is dropped, docs/refactor.md says why.
        let g = TurnPolicy::general();
        let c = TurnPolicy::coding();
        assert_eq!(g.text_ends_turn, c.text_ends_turn);
        assert_eq!(g.gates, c.gates);
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
                gates: Gates::ALL,
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
                gates: Gates::NONE,
                dispatch: false
            }
        );
        // A follow-up works and is gated like a task; only its ending differs.
        let f = u.turn(Intent::FollowUp, false, false);
        assert_eq!(f.route, Route::FollowUp);
        assert!(f.working && f.gates == Gates::ALL && f.dispatch);
        // A child: the delivery gates but no plan bookkeeping and no dispatch of its own.
        let c = u.turn(Intent::Task, true, false);
        assert_eq!(
            (c.route, c.prompt, c.working, c.dispatch),
            (Route::Task, Prompt::Child, true, false)
        );
        assert_eq!(
            c.gates.names(),
            ["delivery", "tests", "claims", "claimed files", "wrap-up"]
        );
        // The ablation switches every gate off, the plan bookkeeping included.
        let a = u.turn(Intent::Task, false, true);
        assert!(a.working && a.gates.is_empty() && a.dispatch);
        // A conversation: working, nothing gated, nothing dispatched.
        let g = TurnPolicy::general().turn(Intent::Question, false, false);
        assert!(g.route == Route::Task && g.working && g.gates.is_empty() && !g.dispatch);
    }

    #[test]
    fn gates_are_listed_one_by_one() {
        // The coding product's expected shape: the named-deliverable gate without the test
        // guard, by changing the preset alone.
        let p = TurnPolicy {
            gates: Gates::NONE.with(Gate::NamedFiles),
            ..TurnPolicy::coding()
        };
        let t = p.turn(Intent::Task, false, false);
        assert!(t.gates.has(Gate::NamedFiles));
        assert!(!t.gates.has(Gate::Tests) && !t.gates.has(Gate::Delivery));
        assert_eq!(
            p.describe(),
            "coding: text ends turn on, finish gates named files, intent routing off, dispatch off"
        );
        // Set algebra and the child's exemption.
        assert_eq!(Gates::ALL.names().len(), Gate::ALL.len());
        assert_eq!(
            Gates::ALL.without(Gate::Tests).with(Gate::Tests),
            Gates::ALL
        );
        for g in Gate::ALL {
            assert_eq!(
                Gates::ALL.without_plan_bookkeeping().has(g),
                !g.plan_bookkeeping(),
                "{g:?}"
            );
        }
        assert_eq!(
            format!("{:?}", Gates::NONE.with(Gate::ReRead)),
            "Gates[re-read]"
        );
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
        assert!(
            !u.text_ends_turn && u.gates == Gates::ALL && u.intent_routing && u.parallel_dispatch
        );
        assert!(!TurnPolicy::general().is_unattended());
        assert!(!TurnPolicy::coding().is_unattended());
        assert_eq!(
            TurnPolicy::coding().describe(),
            "coding: text ends turn on, finish gates none, intent routing off, dispatch off"
        );
        assert_eq!(
            TurnPolicy::unattended().describe(),
            "unattended: text ends turn off, finish gates all, intent routing on, dispatch on"
        );
    }
}
