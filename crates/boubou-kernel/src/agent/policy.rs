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
//! instead of through a spread of booleans (`docs/refactor.md`). The presets carry exactly
//! the behaviours the loop had before the struct existed: [`TurnPolicy::unattended`] is
//! Mingbird's batch harness (the old default), [`TurnPolicy::child`] is that harness
//! narrowed to a dispatched child's one mechanical task (the old `child_mode` minus the
//! sandbox, which is safety and stays on `RunParams::child_mode`), and
//! [`TurnPolicy::conversation`] is the old editor/chat semantics (`interactive = true`).
//! The products build their presets on `conversation`: `boubou-general`'s `general` and
//! `boubou-coding`'s `coding` each set their name and their own task prompt
//! (`harness::policy()` in each crate) and are otherwise equal by construction until
//! measurement says where to differ; a test in `boubou-cli`, the crate that sees both, says
//! so.

use std::fmt;

use super::prompt;
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// The task prompt built on this prefill: the text with its placeholders filled, the
    /// workdir's `SYSTEM.md`, the skills matched on the task, the profile note, the
    /// platform block. The kernel's prefill is [`prompt::TASK_BASE`]; a product's policy
    /// carries its own.
    Task(&'static str),
    /// The chat prompt for the question route: answer briefly, tools only to look things up.
    Chat,
    /// A dispatched child's sandbox prompt: one mechanical task, no skills, no `SYSTEM.md`.
    Child,
}

impl fmt::Debug for Prompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Prompt::Task(base) => write!(f, "Task({} chars)", base.len()),
            Prompt::Chat => f.write_str("Chat"),
            Prompt::Child => f.write_str("Child"),
        }
    }
}

impl Prompt {
    pub fn name(self) -> &'static str {
        match self {
            Prompt::Task(_) => "task",
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
            Prompt::Task(_) => continued,
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
    /// Gate 2: the turn's own verification holds at finish: a check that passed before
    /// later file changes is re-executed through the tool path, and a turn that changed
    /// files in a tested workdir without running any check is asked for one (three
    /// refusals, then let through). See `agent/testguard.rs`.
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
    /// the finish-gate ablations.
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
    /// The system prompt that frames a task turn: [`Prompt::Task`] on the kernel's prefill
    /// for the batch harness, on the product's own text for a product preset, the sandbox
    /// prompt for a child. The question route takes [`Prompt::Chat`] whatever this says.
    pub prompt: Prompt,
    /// The decider reads the opening message and its verdict is logged and, in shadow or on
    /// mode, recorded in the session's labelling sidecar. Off for a child: its task text is
    /// mechanical by construction and would only pollute the labelled set. Whether the
    /// verdict routes the turn is `intent_routing`.
    pub consult_decider: bool,
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
    /// Mingbird's unattended task run (`boubou run`, the web UI's batch mode): every finish
    /// gate, the Q&A and follow-up routes, continue notes and child dispatch.
    pub const fn unattended() -> Self {
        Self {
            name: "unattended",
            text_ends_turn: false,
            gates: Gates::ALL,
            prompt: Prompt::Task(prompt::TASK_BASE),
            consult_decider: true,
            intent_routing: true,
            parallel_dispatch: true,
        }
    }

    /// A dispatched child (`AGENT_CHILD_SANDBOX=1`): the batch harness narrowed to one
    /// mechanical task. The sandbox prompt; the delivery gates without the plan bookkeeping
    /// (no plan to maintain, and the dispatcher checks the exit contract); no decider and no
    /// Q&A, which would swap out the sandbox prompt and drop the producing tools; no dispatch
    /// of its own. The sandbox itself (ring-0 executor, narrowed tool face, circuit breaker)
    /// is safety, not policy, and follows `RunParams::child_mode`.
    pub const fn child() -> Self {
        Self {
            name: "child",
            text_ends_turn: false,
            gates: Gates::ALL
                .without(Gate::NamedFiles)
                .without(Gate::PlanFiles)
                .without(Gate::PlanSync)
                .without(Gate::ReRead),
            prompt: Prompt::Child,
            consult_decider: false,
            intent_routing: false,
            parallel_dispatch: false,
        }
    }

    /// A conversation: the turn semantics the products build on. The turn ends when the
    /// model replies in text or calls `finish`; `finish` is accepted as is (the gates exist
    /// for batch tasks and would run the user's own test suite and bounce the turn); every
    /// turn takes the task prompt and the full tool set; nothing is dispatched into the
    /// user's project. A product preset is `TurnPolicy { name, prompt, ..conversation() }`
    /// with its own name and its own task prefill.
    pub const fn conversation() -> Self {
        Self {
            name: "conversation",
            text_ends_turn: true,
            gates: Gates::NONE,
            prompt: Prompt::Task(prompt::TASK_BASE),
            consult_decider: true,
            intent_routing: false,
            parallel_dispatch: false,
        }
    }

    /// The route for a turn whose opening message the decider read as `intent`. Every turn
    /// under a policy without intent routing is a task.
    pub fn route(&self, intent: Intent) -> Route {
        if !self.intent_routing {
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
    pub fn turn(&self, intent: Intent, gates_ablated: bool) -> Turn {
        let route = self.route(intent);
        let working = route != Route::Question;
        Turn {
            route,
            prompt: if route == Route::Question {
                Prompt::Chat
            } else {
                self.prompt
            },
            working,
            gates: if working && !gates_ablated {
                self.gates
            } else {
                Gates::NONE
            },
            dispatch: working && self.parallel_dispatch,
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
    use super::{prompt, Gate, Gates, Intent, Prompt, Route, Turn, TurnPolicy};

    #[test]
    fn the_route_follows_the_intent_only_where_the_policy_routes() {
        let u = TurnPolicy::unattended();
        assert_eq!(u.route(Intent::Question), Route::Question);
        assert_eq!(u.route(Intent::FollowUp), Route::FollowUp);
        assert_eq!(u.route(Intent::Task), Route::Task);
        // A child and a conversation take the task route for every intent.
        for p in [TurnPolicy::child(), TurnPolicy::conversation()] {
            assert_eq!(p.route(Intent::Question), Route::Task);
            assert_eq!(p.route(Intent::FollowUp), Route::Task);
        }
    }

    #[test]
    fn the_turn_is_the_policy_applied_to_intent_and_ablation() {
        let u = TurnPolicy::unattended();
        assert_eq!(
            u.turn(Intent::Task, false),
            Turn {
                route: Route::Task,
                prompt: Prompt::Task(prompt::TASK_BASE),
                working: true,
                gates: Gates::ALL,
                dispatch: true
            }
        );
        // A question: the answer is the chat; nothing is gated, paced or dispatched.
        assert_eq!(
            u.turn(Intent::Question, false),
            Turn {
                route: Route::Question,
                prompt: Prompt::Chat,
                working: false,
                gates: Gates::NONE,
                dispatch: false
            }
        );
        // A follow-up works and is gated like a task; only its ending differs.
        let f = u.turn(Intent::FollowUp, false);
        assert_eq!(f.route, Route::FollowUp);
        assert!(f.working && f.gates == Gates::ALL && f.dispatch);
        // A child: its own prompt, the delivery gates but no plan bookkeeping, no decider,
        // no dispatch of its own, whatever the opening message looks like.
        let child = TurnPolicy::child();
        assert!(!child.consult_decider);
        let c = child.turn(Intent::Question, false);
        assert_eq!(
            (c.route, c.prompt, c.working, c.dispatch),
            (Route::Task, Prompt::Child, true, false)
        );
        assert_eq!(
            c.gates.names(),
            ["delivery", "tests", "claims", "claimed files", "wrap-up"]
        );
        assert_eq!(child.gates, Gates::ALL.without_plan_bookkeeping());
        // The ablation switches every gate off, the plan bookkeeping included.
        let a = u.turn(Intent::Task, true);
        assert!(a.working && a.gates.is_empty() && a.dispatch);
        // A conversation: working, nothing gated, nothing dispatched.
        let g = TurnPolicy::conversation().turn(Intent::Question, false);
        assert!(g.route == Route::Task && g.working && g.gates.is_empty() && !g.dispatch);
    }

    #[test]
    fn gates_are_listed_one_by_one() {
        // A coding product's likely shape: the named-deliverable gate without the test
        // guard, by changing the preset alone.
        let p = TurnPolicy {
            name: "editor",
            gates: Gates::NONE.with(Gate::NamedFiles),
            ..TurnPolicy::conversation()
        };
        let t = p.turn(Intent::Task, false);
        assert!(t.gates.has(Gate::NamedFiles));
        assert!(!t.gates.has(Gate::Tests) && !t.gates.has(Gate::Delivery));
        assert_eq!(
            p.describe(),
            "editor: text ends turn on, finish gates named files, intent routing off, dispatch off"
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
        let task = Prompt::Task(prompt::TASK_BASE);
        assert_eq!(u.turn(Intent::Task, false).prompt, task);
        assert_eq!(u.turn(Intent::FollowUp, false).prompt, task);
        assert_eq!(u.turn(Intent::Question, false).prompt, Prompt::Chat);
        assert_eq!(
            TurnPolicy::child().turn(Intent::Question, false).prompt,
            Prompt::Child
        );
        // A conversation: the task prompt for a question too, so an instruction phrased as
        // a question keeps the producing tools; a product's own prefill travels with it.
        assert_eq!(
            TurnPolicy::conversation()
                .turn(Intent::Question, false)
                .prompt,
            task
        );
        let own = TurnPolicy {
            prompt: Prompt::Task("You are a product."),
            ..TurnPolicy::conversation()
        };
        assert_eq!(
            own.turn(Intent::Question, false).prompt,
            Prompt::Task("You are a product.")
        );
        assert_eq!(format!("{:?}", own.prompt), "Task(18 chars)");
        // Rebuilding: chat every turn, task on a continued session, child never.
        assert!(Prompt::Chat.rebuild(false) && Prompt::Chat.rebuild(true));
        assert!(!task.rebuild(false) && task.rebuild(true));
        assert!(!Prompt::Child.rebuild(false) && !Prompt::Child.rebuild(true));
    }

    #[test]
    fn unattended_is_the_batch_harness_and_the_default() {
        let u = TurnPolicy::default();
        assert!(u.is_unattended());
        assert!(
            !u.text_ends_turn && u.gates == Gates::ALL && u.intent_routing && u.parallel_dispatch
        );
        assert!(!TurnPolicy::conversation().is_unattended());
        assert!(!TurnPolicy::child().is_unattended());
        assert_eq!(
            TurnPolicy::conversation().describe(),
            "conversation: text ends turn on, finish gates none, intent routing off, dispatch off"
        );
        assert_eq!(
            TurnPolicy::unattended().describe(),
            "unattended: text ends turn off, finish gates all, intent routing on, dispatch on"
        );
    }
}
