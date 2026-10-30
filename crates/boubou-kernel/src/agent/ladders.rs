//! The small-model ladders: the kernel's stateful compensation mechanisms, each a struct
//! the loop feeds an observation and gets a verdict from. Nothing here reads the turn
//! policy: a ladder says what the model just did and what that calls for on its own terms,
//! and the loop in [`super::run`] decides what to do with it under the policy
//! (`docs/refactor.md`, step 8).

use std::collections::{BTreeSet, HashMap};

use super::face::{PRODUCTIVE, RESEARCH_ALL};

/// The text-only turn ladders: a reply with no tool call is either empty (the empty-reply
/// ladder: nudge, hard reset, give up) or talk (near-duplicate detection over the last few
/// replies, and the talk streak that bounds a model that talks instead of working).
#[derive(Debug, Default)]
pub struct TextLadder {
    empty_turns: usize,
    empty_resets: usize,
    empty_total: usize,
    /// The previous reply hit the generation cap with no text and no call: the next empty
    /// reply is truncation debris, not idleness.
    truncated: bool,
    recent_texts: Vec<String>,
    repeat_count: usize,
    talk_streak: usize,
}

/// How many recent text-only outputs the repeat detector compares against.
const TEXT_WINDOW: usize = 4;
/// The text-only turn that draws the sharper "tool call or finish" note (once).
const TALK_SHARP_AT: usize = 2;
/// Consecutive text-only turns after which the turn is ended.
const TALK_END_AT: usize = 4;
/// Consecutive empty replies that draw the "do real work" nudge.
const EMPTY_NUDGE_AT: usize = 3;
/// Consecutive empty replies that reset the context.
const EMPTY_RESET_AT: usize = 6;
/// Context resets allowed before the run is given up.
const EMPTY_RESETS_MAX: usize = 3;

/// What an empty reply calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyVerdict {
    /// The first empty reply right after a generation-cap cut: the model is not idle, one
    /// shot could not hold its output. Tell it to chunk ([`TextLadder::truncation_note`]).
    TruncationDebris,
    /// Six empties in a row: reset the context ([`TextLadder::reset_note`]). `resets` so
    /// far of the three allowed; `total` empty replies over the run.
    Reset { resets: usize, total: usize },
    /// Three more empties after the last allowed reset: give the run up.
    GiveUp { total: usize },
    /// The third empty in a row: the "do real work" nudge ([`TextLadder::EMPTY_NUDGE`]).
    Nudge,
    /// Nothing to say yet; the model gets another go.
    Quiet,
}

/// What a text reply calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TalkVerdict {
    /// Near-identical to one of the last few replies, `count` times in a row; from the
    /// second, a dead loop ([`TextLadder::DEAD_LOOP_NOTE`]).
    Repeated { count: usize, dead_loop: bool },
    /// New text, the `streak`-th text-only reply in a row.
    Fresh { streak: usize, talk: Talk },
}

/// What a run of text-only replies calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Talk {
    /// The plain continue note.
    Continue,
    /// The sharper "tool call or finish" note ([`TextLadder::TALK_SHARP_NOTE`]).
    Sharp,
    /// The model is talking, not working: end the turn ([`TextLadder::TALK_END_NOTE`]).
    End,
}

impl TextLadder {
    pub const EMPTY_NUDGE: &'static str = "⚠️ You have output empty text repeatedly. Call a tool and do real work now: read_file to read, run_bash to run a command, create_file to write. Do not output empty text.";
    pub const DEAD_LOOP_NOTE: &'static str = "⚠️ You have output nearly identical content repeatedly; this counts as a dead loop. Call finish now to end this turn. Do not repeat yourself.";
    pub const TALK_SHARP_NOTE: &'static str = "⚠️ You have replied in text without calling any tool. Reply with a tool call (create_file / run_bash / read_file …) to do the work, or call finish(summary=your answer) if the text already answers the user. Do not reply with more text.";
    pub const TALK_END_NOTE: &'static str = "⚠️ You have replied in text several times without calling a tool. The turn ends here; your earlier text stands as the answer.";

    /// The model hit the generation cap with no text and no call.
    pub fn note_truncated(&mut self) {
        self.truncated = true;
    }

    /// Consecutive empty replies so far (after [`Self::on_empty`] counted the latest).
    pub fn empty_turns(&self) -> usize {
        self.empty_turns
    }

    /// An empty reply (no text, no tool call).
    pub fn on_empty(&mut self) -> EmptyVerdict {
        self.empty_turns += 1;
        self.empty_total += 1;
        if self.truncated && self.empty_turns == 1 {
            self.truncated = false;
            return EmptyVerdict::TruncationDebris;
        }
        if self.empty_turns >= EMPTY_RESET_AT && self.empty_resets < EMPTY_RESETS_MAX {
            self.empty_resets += 1;
            self.empty_turns = 0;
            return EmptyVerdict::Reset {
                resets: self.empty_resets,
                total: self.empty_total,
            };
        }
        if self.empty_turns >= EMPTY_NUDGE_AT && self.empty_resets >= EMPTY_RESETS_MAX {
            return EmptyVerdict::GiveUp {
                total: self.empty_total,
            };
        }
        if self.empty_turns == EMPTY_NUDGE_AT {
            return EmptyVerdict::Nudge;
        }
        EmptyVerdict::Quiet
    }

    /// A text reply with no tool call.
    pub fn on_text(&mut self, content: &str) -> TalkVerdict {
        self.empty_turns = 0;
        self.truncated = false;
        self.talk_streak += 1;
        let repeated = self
            .recent_texts
            .iter()
            .any(|t| super::history::similar(t, content, 0.75));
        self.recent_texts.push(content.to_string());
        if self.recent_texts.len() > TEXT_WINDOW {
            self.recent_texts.remove(0);
        }
        if repeated {
            self.repeat_count += 1;
            return TalkVerdict::Repeated {
                count: self.repeat_count,
                dead_loop: self.repeat_count >= 2,
            };
        }
        self.repeat_count = 0;
        let talk = if self.talk_streak >= TALK_END_AT {
            Talk::End
        } else if self.talk_streak == TALK_SHARP_AT {
            Talk::Sharp
        } else {
            Talk::Continue
        };
        TalkVerdict::Fresh {
            streak: self.talk_streak,
            talk,
        }
    }

    /// The model called a tool: the talk streak is over.
    pub fn on_tool_call(&mut self) {
        self.talk_streak = 0;
    }

    /// The note for [`EmptyVerdict::TruncationDebris`].
    pub fn truncation_note(num_predict: i32) -> String {
        format!(
            "⚠️ Your previous output hit the generation token limit ({num_predict}) and was truncated without producing a valid tool call. Deliver large content in chunks: write the skeleton or opening part with create_file, then add the rest with append_file in pieces of a few hundred lines at most; or simplify the implementation. Do not try to output a whole large file in one go."
        )
    }

    /// The user message a reset context opens with: the original task and a progress
    /// snapshot (the plan and the files already in the working directory).
    pub fn reset_note(task: &str, plan: &str, files: &str) -> String {
        format!(
            "{task}\n\n⚠️ [system] Several consecutive empty outputs; the context has been reset. Progress snapshot:\nPlan:\n{plan}\nFiles already in the working directory:\n{files}\nContinue from the first unfinished step and make progress with tools immediately. Do not output empty text."
        )
    }
}

/// The per-call ladders: what a model's tool calls, taken one after another, add up to.
/// Each method takes one observation about the call just made and answers with what the
/// ladder calls for; the loop carries out the answer (a note, a disabled tool, a context
/// reset) and owns every message text and tool-face change.
#[derive(Debug, Default)]
pub struct CallLadders {
    /// Calls to a tool that is disabled, absent or malformed, per name.
    disabled_recall: HashMap<String, usize>,
    /// The last call that actually ran (name, sorted-JSON arguments): an exact repeat of it
    /// cannot produce a different result.
    last_ran: Option<(String, String)>,
    dup_warns: usize,
    /// The failure ladder: consecutive failures per tool, and how many redirect notes and
    /// shell-teaching hints have gone out.
    fail_count: HashMap<String, usize>,
    redirect_warns: usize,
    bash_teach_warns: usize,
    /// The missing-argument ladder, per tool.
    missing_arg_streak: HashMap<String, usize>,
    missing_arg_warns: usize,
    /// Repeat streaks over calls that ran: the same tool in a row, the same exact call in a
    /// row.
    last_tool: Option<String>,
    tool_streak: usize,
    streak_warns: usize,
    last_sig: Option<(String, String)>,
    same_sig_streak: usize,
    /// Consecutive calls that only read (boubou-9c5).
    idle_streak: usize,
    /// Web research: searches in a row that found nothing, and repeats of a query already
    /// made (by fingerprint).
    no_result_searches: usize,
    research_streak: usize,
    research_warns: usize,
    research_seen: BTreeSet<String>,
    /// Plan-tool calls without producing work in between.
    todo_streak: usize,
}

/// A call to a tool that is disabled, absent or malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recall {
    /// The tenth call: reset the context, this is not going to stop by itself.
    Reset,
    /// Refuse the call; `note` on the third, the strong "change action now" note.
    Refuse { note: bool },
}

/// What a failed call calls for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The second consecutive call missing the same required argument (`streak` so far):
    /// prune the earlier failed pairs and prescribe the chunked call shape (four times).
    MissingArg { key: String, streak: usize },
    /// `edit_file` failed three times in a row: disable it.
    DisableEditFile,
    /// The tool failed `count` times in a row: the "stop retrying, change strategy" note
    /// (four times).
    Redirect { count: usize },
    /// Nothing yet.
    Quiet,
}

/// What a run of calls that ran calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Streak {
    /// The same tool `count` times in a row: the "if nothing new is coming, move on" note
    /// (three times).
    Note {
        count: usize,
    },
    /// A true loop (the same tool eight times, the same exact call four times): disable it.
    Disable {
        count: usize,
    },
    Quiet,
}

/// What a web search or fetch adds up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Research {
    /// The second search in a row that found nothing.
    pub no_results_twice: bool,
    /// The same query repeated `Some(n)` times: disable the web tools (three times).
    pub disable: Option<usize>,
}

impl CallLadders {
    /// A call to a tool that is disabled, not assembled or malformed.
    pub fn on_recall(&mut self, name: &str) -> Recall {
        let cnt = self.disabled_recall.entry(name.to_string()).or_insert(0);
        *cnt += 1;
        if *cnt >= 10 {
            *cnt = 0;
            return Recall::Reset;
        }
        Recall::Refuse { note: *cnt == 3 }
    }

    /// Exactly the call that last ran (three refusals, then it is allowed through).
    pub fn is_duplicate(&mut self, name: &str, sig: &str) -> bool {
        let dup = name != "finish"
            && self
                .last_ran
                .as_ref()
                .is_some_and(|(n, s)| n == name && s == sig)
            && self.dup_warns < 3;
        if dup {
            self.dup_warns += 1;
        }
        dup
    }

    /// The call ran (also for a call the corrector substituted).
    pub fn ran(&mut self, name: &str, sig: &str) {
        self.last_ran = Some((name.to_string(), sig.to_string()));
    }

    /// Whether the shell-teaching hint may go out (twice).
    pub fn bash_teach(&mut self) -> bool {
        if self.bash_teach_warns < 2 {
            self.bash_teach_warns += 1;
            return true;
        }
        false
    }

    /// The call failed; `missing_arg` is the required argument it lacked, if that is why.
    pub fn on_failure(&mut self, name: &str, missing_arg: Option<&str>) -> Failure {
        if let Some(key) = missing_arg {
            let st = self.missing_arg_streak.entry(name.to_string()).or_insert(0);
            *st += 1;
            if *st >= 2 && self.missing_arg_warns < 4 {
                self.missing_arg_warns += 1;
                return Failure::MissingArg {
                    key: key.to_string(),
                    streak: *st,
                };
            }
            return Failure::Quiet;
        }
        let fc = self.fail_count.entry(name.to_string()).or_insert(0);
        *fc += 1;
        if *fc < 3 {
            return Failure::Quiet;
        }
        if name == "edit_file" {
            *fc = 0;
            return Failure::DisableEditFile;
        }
        if self.redirect_warns < 4 {
            self.redirect_warns += 1;
            let count = *fc;
            *fc = 0;
            return Failure::Redirect { count };
        }
        Failure::Quiet
    }

    /// The call succeeded: only a success of the same tool resets its ladders.
    pub fn on_success(&mut self, name: &str) {
        if self.fail_count.get(name).is_some_and(|c| *c > 0) {
            self.fail_count.insert(name.to_string(), 0);
        }
        self.missing_arg_streak.insert(name.to_string(), 0);
    }

    /// Repeat streaks over a call that ran. `anti_loop` is off under the bench's
    /// `anti_loop` ablation.
    pub fn on_streak(&mut self, name: &str, sig: &str, anti_loop: bool) -> Streak {
        if self
            .last_sig
            .as_ref()
            .is_some_and(|(n, s)| n == name && s == sig)
        {
            self.same_sig_streak += 1;
        } else {
            self.same_sig_streak = 1;
        }
        self.last_sig = Some((name.to_string(), sig.to_string()));
        if self.last_tool.as_deref() == Some(name) {
            self.tool_streak += 1;
        } else {
            self.last_tool = Some(name.to_string());
            self.tool_streak = 1;
        }
        if self.tool_streak >= 5 && self.streak_warns < 3 {
            self.streak_warns += 1;
            let count = self.tool_streak;
            self.tool_streak = 1;
            return Streak::Note { count };
        }
        if anti_loop && self.tool_streak >= 8 && self.same_sig_streak >= 4 && name != "finish" {
            let count = self.tool_streak;
            self.tool_streak = 1;
            return Streak::Disable { count };
        }
        Streak::Quiet
    }

    /// The same exact call, this many times in a row (after [`Self::on_streak`]).
    pub fn same_sig_streak(&self) -> usize {
        self.same_sig_streak
    }

    /// Read-only calls in a row so far.
    pub fn idle_streak(&self) -> usize {
        self.idle_streak
    }

    /// The call only read (`idle`) or changed the world. Returns the streak after it.
    pub fn on_idle(&mut self, idle: bool) -> usize {
        if idle {
            self.idle_streak += 1;
        } else {
            self.idle_streak = 0;
        }
        self.idle_streak
    }

    /// A call ran: what it adds up to for web research. `no_results` is a search that found
    /// nothing relevant.
    pub fn on_research(&mut self, name: &str, sig: &str, no_results: bool) -> Research {
        let research = RESEARCH_ALL.contains(&name);
        let mut out = Research {
            no_results_twice: false,
            disable: None,
        };
        if research && no_results {
            self.no_result_searches += 1;
            out.no_results_twice = self.no_result_searches == 2;
        } else if research {
            self.no_result_searches = 0;
        }
        if research {
            let fp = crate::text::head(sig, 256).to_string();
            if self.research_seen.contains(&fp) {
                self.research_streak += 1;
            } else {
                self.research_seen.insert(fp);
                self.research_streak = 0;
            }
            if self.research_streak >= 6 && self.research_warns < 3 {
                self.research_warns += 1;
                out.disable = Some(self.research_streak);
                self.research_streak = 0;
                self.research_seen.clear();
            }
        } else if matches!(
            name,
            "create_file" | "edit_file" | "append_file" | "finish" | "run_bash"
        ) {
            self.research_streak = 0;
        }
        out
    }

    /// A call ran: the plan-tool loop. `Some((same exact call streak, todo calls))` says
    /// disable `todo` (after [`Self::on_streak`]).
    pub fn on_todo(&mut self, name: &str) -> Option<(usize, usize)> {
        if name == "todo" {
            self.todo_streak += 1;
            if self.same_sig_streak >= 6 || self.todo_streak >= 15 {
                let out = (self.same_sig_streak, self.todo_streak);
                self.todo_streak = 0;
                return Some(out);
            }
        } else if PRODUCTIVE.contains(&name) {
            self.todo_streak = 0;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{EmptyVerdict, Talk, TalkVerdict, TextLadder};

    #[test]
    fn the_empty_ladder_nudges_resets_and_gives_up() {
        let mut l = TextLadder::default();
        assert_eq!(l.on_empty(), EmptyVerdict::Quiet);
        assert_eq!(l.on_empty(), EmptyVerdict::Quiet);
        assert_eq!(l.on_empty(), EmptyVerdict::Nudge);
        assert_eq!(l.empty_turns(), 3);
        assert_eq!(l.on_empty(), EmptyVerdict::Quiet);
        assert_eq!(l.on_empty(), EmptyVerdict::Quiet);
        assert_eq!(
            l.on_empty(),
            EmptyVerdict::Reset {
                resets: 1,
                total: 6
            }
        );
        assert_eq!(l.empty_turns(), 0, "the reset starts the count over");
        for _ in 0..2 {
            for _ in 0..5 {
                l.on_empty();
            }
            assert!(matches!(l.on_empty(), EmptyVerdict::Reset { .. }));
        }
        // After the third reset, three more empties end the run.
        assert_eq!(l.on_empty(), EmptyVerdict::Quiet);
        assert_eq!(l.on_empty(), EmptyVerdict::Quiet);
        assert_eq!(l.on_empty(), EmptyVerdict::GiveUp { total: 21 });
    }

    #[test]
    fn truncation_debris_is_told_apart_from_idleness_once() {
        let mut l = TextLadder::default();
        l.note_truncated();
        assert_eq!(l.on_empty(), EmptyVerdict::TruncationDebris);
        assert_eq!(l.on_empty(), EmptyVerdict::Quiet, "the flag is spent");
        // A text reply clears both the empty count and the flag.
        l.note_truncated();
        l.on_text("working on it");
        assert_eq!(l.empty_turns(), 0);
        assert_eq!(l.on_empty(), EmptyVerdict::Quiet);
    }

    #[test]
    fn talk_is_bounded_and_repeats_are_a_dead_loop() {
        let mut l = TextLadder::default();
        let fresh = |v: TalkVerdict| match v {
            TalkVerdict::Fresh { streak, talk } => (streak, talk),
            other => panic!("{other:?}"),
        };
        assert_eq!(fresh(l.on_text("one")), (1, Talk::Continue));
        assert_eq!(fresh(l.on_text("two")), (2, Talk::Sharp));
        assert_eq!(fresh(l.on_text("three")), (3, Talk::Continue));
        assert_eq!(fresh(l.on_text("four")), (4, Talk::End));
        // A tool call ends the streak.
        l.on_tool_call();
        assert_eq!(fresh(l.on_text("five")), (1, Talk::Continue));
        // Near-duplicates of a recent reply: the second in a row is a dead loop.
        assert_eq!(
            l.on_text("five"),
            TalkVerdict::Repeated {
                count: 1,
                dead_loop: false
            }
        );
        assert_eq!(
            l.on_text("five"),
            TalkVerdict::Repeated {
                count: 2,
                dead_loop: true
            }
        );
        // Fresh text resets the repeat count.
        assert!(matches!(
            l.on_text("something else entirely"),
            TalkVerdict::Fresh { .. }
        ));
        assert_eq!(
            l.on_text("five"),
            TalkVerdict::Repeated {
                count: 1,
                dead_loop: false
            }
        );
    }

    #[test]
    fn the_call_ladders_count_refusals_failures_and_streaks() {
        use super::{CallLadders, Failure, Recall, Streak};
        let mut l = CallLadders::default();
        // Disabled-tool recalls: a note on the third, a reset on the tenth, then over again.
        for i in 1..=9 {
            assert_eq!(l.on_recall("x"), Recall::Refuse { note: i == 3 }, "{i}");
        }
        assert_eq!(l.on_recall("x"), Recall::Reset);
        assert_eq!(l.on_recall("x"), Recall::Refuse { note: false });
        // Duplicates: only of the call that ran, three refusals.
        assert!(!l.is_duplicate("read_file", "{}"));
        l.ran("read_file", "{}");
        assert!(l.is_duplicate("read_file", "{}"));
        assert!(!l.is_duplicate("read_file", "{\"a\":1}"));
        assert!(!l.is_duplicate("finish", "{}"));
        assert!(l.is_duplicate("read_file", "{}") && l.is_duplicate("read_file", "{}"));
        assert!(!l.is_duplicate("read_file", "{}"), "fourth time through");
        // Failures: the redirect note at three, edit_file disabled instead; a success resets.
        assert_eq!(l.on_failure("run_bash", None), Failure::Quiet);
        assert_eq!(l.on_failure("run_bash", None), Failure::Quiet);
        assert_eq!(
            l.on_failure("run_bash", None),
            Failure::Redirect { count: 3 }
        );
        l.on_failure("edit_file", None);
        l.on_failure("edit_file", None);
        assert_eq!(l.on_failure("edit_file", None), Failure::DisableEditFile);
        l.on_failure("run_bash", None);
        l.on_success("run_bash");
        assert_eq!(l.on_failure("run_bash", None), Failure::Quiet);
        // Missing arguments: from the second consecutive miss, four prescriptions.
        assert_eq!(l.on_failure("create_file", Some("path")), Failure::Quiet);
        assert_eq!(
            l.on_failure("create_file", Some("path")),
            Failure::MissingArg {
                key: "path".into(),
                streak: 2
            }
        );
        assert!(l.bash_teach() && l.bash_teach() && !l.bash_teach());
        // Streaks: the note at five in a row (three times), the loop at eight with the same
        // exact call four times.
        let mut l = CallLadders::default();
        for i in 1..=4 {
            assert_eq!(
                l.on_streak("read_file", &format!("{i}"), true),
                Streak::Quiet
            );
        }
        assert_eq!(
            l.on_streak("read_file", "5", true),
            Streak::Note { count: 5 }
        );
        let mut l = CallLadders {
            streak_warns: 3,
            ..Default::default()
        };
        for i in 1..=7 {
            l.on_streak("read_file", if i < 4 { "a" } else { "b" }, true);
        }
        assert_eq!(l.same_sig_streak(), 4);
        assert_eq!(
            l.on_streak("read_file", "b", true),
            Streak::Disable { count: 8 }
        );
        assert_eq!(
            l.on_streak("read_file", "b", false),
            Streak::Quiet,
            "ablated"
        );
    }

    #[test]
    fn idle_research_and_plan_tool_ladders() {
        use super::CallLadders;
        let mut l = CallLadders::default();
        assert_eq!(l.on_idle(true), 1);
        assert_eq!(l.on_idle(true), 2);
        assert_eq!(l.idle_streak(), 2);
        assert_eq!(l.on_idle(false), 0);
        // Two empty searches in a row draw the note once; a search with results resets.
        assert!(!l.on_research("web_search", "q1", true).no_results_twice);
        assert!(l.on_research("web_search", "q2", true).no_results_twice);
        assert!(!l.on_research("web_search", "q3", true).no_results_twice);
        assert!(!l.on_research("web_search", "q4", false).no_results_twice);
        assert!(!l.on_research("web_search", "q5", true).no_results_twice);
        // The same query over and over: disabled at the sixth repeat, then a clean slate.
        let mut l = CallLadders::default();
        assert_eq!(l.on_research("web_search", "same", false).disable, None);
        for _ in 0..5 {
            assert_eq!(l.on_research("web_search", "same", false).disable, None);
        }
        assert_eq!(l.on_research("web_search", "same", false).disable, Some(6));
        assert_eq!(l.on_research("web_search", "same", false).disable, None);
        // A file write resets the repeat streak; an unrelated read does not.
        l.on_research("web_search", "same", false);
        l.on_research("create_file", "{}", false);
        for _ in 0..5 {
            assert_eq!(l.on_research("web_search", "same", false).disable, None);
        }
        assert_eq!(l.on_research("web_search", "same", false).disable, Some(6));
        // Plan tool: fifteen calls without producing work, or the same call six times.
        let mut l = CallLadders::default();
        for i in 0..14 {
            l.on_streak("todo", &format!("{{\"index\":{i}}}"), true);
            assert_eq!(l.on_todo("todo"), None, "{i}");
        }
        l.on_streak("todo", "x", true);
        assert_eq!(l.on_todo("todo"), Some((1, 15)));
        let mut l = CallLadders::default();
        for i in 1..=6 {
            l.on_streak("todo", "x", true);
            let v = l.on_todo("todo");
            assert_eq!(v.is_some(), i == 6, "{i}");
        }
        l.on_todo("create_file");
    }
}
