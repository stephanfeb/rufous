# Small models follow text, not state

*Written 2026-11-01 from the ACP conversation rig readings of 2026-10-31 and 2026-11-01
(`bench/acp_convo.py`, results under `bench/results/convo/4b_*`). The numbers are from
those folders; the claim is as wide as those scripts and no wider.*

*What happened when each mechanism was tested: `docs/small-models-follow-text-not-state-results.md`.*

## The observation

A 2–12B model given one plain instruction usually does it. The same model, three turns
into a conversation, often does something adjacent to what was asked and reports success.
It is not worse at the fourth turn than the first. It is worse at any turn whose correct
action depends on something other than the words in front of it.

Put bluntly: a small model follows the most recent, most concrete text in its context. A
single instruction is exactly that. Turn-by-turn work fails wherever the right action
lives in state the model has to hold rather than text it can read.

## The evidence

The rig has three scripts that separate the two cases. Longform is ten turns, but each turn
is self-contained: append a section to `report.md`, answer a count. Tracker is four turns
whose each step depends on the state left by the last one: break a spec into `tasks.md`,
tick the first, add three more without changing anything else. Permission is two turns in
which the second asks again for a write the model refused in the first.

| script | turns | each turn depends on | qwen3.5:4b | gemma4:e4b |
|---|---|---|---|---|
| longform | 10 | the instruction only | 1.00 | 1.00, 0.97 |
| tracker, before gate 3c | 4 | the previous turn's file state | 0.25 | 0.58 |
| tracker, after gate 3c | 4 | same | 0.92, 0.75 | 0.83, 1.00 |
| permission | 2 | the model's own previous refusal | 0.83 | 0.67–0.83 |

Longform has scored full marks on both models since the append-newline fix (bead
boubou-l3e). Tracker went from a quarter to near full marks without any change to the
model, only to what the harness checks at the end of a turn (changelog, "Gate 3c measured",
2026-11-01). Permission's residual has not moved in seven runs.

## Four ways a turn goes wrong

Each of these was read from a cell, filed as a bead with a design, fixed or left open,
and remeasured.

- **The model's own history becomes an instruction.** Permission turn 2 is the clean
  case. The model refused in turn 1 ("cannot write outside the workspace"), the refusal
  sits in context, and in turn 2 it refuses again citing "safety restrictions" and makes
  no call at all. Nothing in turn 2 asks for that. Its earlier reply is being obeyed as
  policy. Open.
- **A wrong state compounds instead of correcting.** In `4b_scope_v1` qwen tracker r2 the
  model, told "tasks in tasks.md" with `./tasks.md` present, created `openspec/tasks.md`
  beside the other path in the sentence. Turns 3 and 4 then edited that file, never the
  real one. Each later turn was locally correct against the state the model believed in.
  No instruction text could fix it, which is why the fix is a gate (bead boubou-di6: a
  bare name that is a file in the workdir is that file).
- **Carried constraints decay.** "Change nothing else" holds on the turn it is said and is
  gone two turns later. The rig carries those checks forward itself (bead boubou-dby)
  because the model does not.
- **Every word in a tool result is an instruction.** The guide's principle
  (`docs/guide.md`). A file listing, an error message or a refusal text in turn *n* shapes
  turn *n+1* as strongly as the user's words do. A refusal that named the real path led
  the model to write there; a hold note that named the bare file led it to create that
  name under another directory.

The common thread is that the model does not distinguish instruction from history from
tool output. All of it is text, and the nearest concrete text wins. That is true at the
surface. The literature splits it into three mechanisms with three different fixes, and the
split decides which lever to reach for (see "Three mechanisms, not one").

## Why the single instruction works

Everything the model needs is in the instruction: the file, the verb, the content. There
is no state to carry, no earlier reply to misread as policy, no tool output between the
ask and the act. The model's pattern-matching on the instruction's words is enough, and
on longform it is enough ten times in a row.

This is also why the failure is not a context-length problem. Tracker's four turns fit in
a fraction of the window. The context is not too long; it is too flat.

## What the harness does about it

The harness cannot make the model hold state. What it can do is turn state back into
text at the moment it matters, and check the model's claims against evidence the model
did not write.

- **A ledger the model cannot write to.** Every tool call is recorded by the harness:
  tool, path or command, whether it landed, exit code. Declined names and scope decisions
  go in the same ledger (`integrity::CommandRecord`, `safety::DeclinedWrite`).
- **A cheap reading of the instruction.** File names with a write cue or mutation verb
  near them, a scope class, a target path. Regexes and word windows, tested against a
  labelled set (`bench/convo/scope_labels.jsonl`) so a change to the reading is a
  measured change.
- **Gates at the endings.** When the model finishes or replies with text, each gate
  compares the instruction's commitments to the ledger. Named file not written and no
  admitted failure: hold. Write of the name at another path when the file exists: hold
  with both paths. Same name the user declined, elsewhere: refuse, and carry the name to
  the next turn's ask.
- **One specific hold, then let it go.** The note says what is missing and what would
  satisfy it, with paths. It fires once per turn. An honest "cannot, because" always
  passes. The turn is never looped.

The design constraints are the point. Everything is lexical and deterministic, so a false
hold is debuggable from the log. Every gate accepts an honest failure report, so the
harness never fights the truth. A gate is added only after a script shows the failure and
kept only if the rerun shows the fix without a regression elsewhere.

## Three mechanisms, not one

A literature pass on 2026-11-01 (summarised by another agent; the papers themselves are
not quoted here, and the claims below are as reliable as that summary) found each of the
four shapes above named and measured, and found that "flat text" hides three distinct
mechanisms.

| mechanism | our shape | what fixes it | what does not |
|---|---|---|---|
| stale history hijacks the decision | a wrong state compounds; tool output read as instruction | render state from the harness's ledger instead of replaying the transcript | a prompt line saying "ignore misleading history" (no effect at 1.7B) |
| the model's own output is privileged | permission turn 2: its refusal obeyed as policy | re-attribute the earlier answer so it is no longer the model's own | repeating the instruction; the bias is to self, not to text |
| standing constraints get evicted or weakened | "change nothing else" decays | pin the constraint with provenance, and keep the gate | restating it: models restate the constraint they just violated |

Three results shape the ordering.

- Giving an instruction in pieces across turns costs about 39% against the same instruction
  at once, and an 8B model loses the same as a frontier one (Lost in Conversation). Restating
  every earlier piece on each turn recovers only part of the gap. "Put the state in the text"
  is real but partial, which is why the gates stay.
- A harness-built state summary in place of the raw history lifted a 1.7B model's correct tool
  decisions from 0.47 to 0.75 and an 8B from 0.69 to 0.89, untrained (When History Lies).
  That is the largest effect any of the papers report and the one lever this harness has not
  tried.
- The self-consistency bias disappears when the same earlier answer is attributed to another
  agent (choice-supportive bias study, tested on Gemma 3). One refusal in context drops later
  compliance from about 55% to 7% in red-teaming work. Our own thirty permission cells show
  the curve: turn 2 passed 19/23 after one refusal in context, 1/7 after two or more.

The false-success work backs the ledger-and-gates design directly: false success is 45–48%
of agent failures in one benchmark, no LLM judge beat AUROC 0.65 at detecting it, a lexical
classifier reached 0.83–0.95, and models miss 64.5% of their own errors but fix the identical
error when it arrives as external input.

What the pass did not find: any paper testing 4B models on multi-turn coding. The nearest
are 1.7B and 8B on tool calling. This rig is the only measurement at that size we have, so
each adoption stays a bead with a before and an after.

Order adopted from this: the admission-rule fix (bead boubou-e9i) as filed; permission
turn 2 (bead boubou-alj) as two measured arms, re-attribution first and the hold as the
backstop; a state-render ablation on tracker once alj has a number; a pre-write check at the
call boundary (di6 moved earlier) only if the wrong-directory shape recurs; constraint
pinning only when a script fails on a carried constraint.

## What this does not say

- The scripts separate *independent* turns from *state-carrying* turns, not few from
  many. Nothing here is a claim about sessions longer than ten turns.
- Two models, 4B and e4B. Bigger small models (12B) were not in these runs.
- The gates check what leaves a trace. Whether the six tasks written into `tasks.md` are
  the right six is beyond them; that is where a judge or a person belongs.
- Permission turn 2 shows the limit of the approach so far: the harness can see that no
  call was made, but the refusal is an honest report, and an honest report passes. Making
  the model not obey its own earlier refusal is a different kind of problem from making it
  write the right file.

## Implications for anyone building on small models

1. Put the state in the text. If a later turn depends on something, say it again in that
   turn, with the path. Do not rely on the model to remember the file it created. Expect a
   partial recovery, not a full one; the gate stays.
2. Treat the model's report as a claim and check it against your own record of what ran.
3. Never add a judge that reads the same text the model read. It has the same blind spot.
4. Measure state-carrying scripts separately from single-instruction ones. A model that
   scores well on the second will mislead you about the first.
5. When a fix is a prompt line, ask whether the failure was in reading the instruction or
   in holding state. Prompt lines fix the first. Only a gate fixes the second.
6. Ask which of the three mechanisms you are looking at. Stale history wants a state render,
   the model's own prior answer wants re-attribution, a decayed constraint wants pinning plus
   a gate. The same symptom, a wrong turn, has three causes with three fixes.

## References

- `docs/guide.md`: the principles, including "every word in a tool result is an
  instruction".
- `docs/changelog.md`, entries of 2026-10-31 and 2026-11-01: each reading and change.
- Beads boubou-78s (gate 3c), boubou-18l, boubou-vj3, boubou-dml, boubou-di6,
  boubou-dby, boubou-l3e, boubou-gqw.
- `bench/convo/README.md`: the scripts and their checks.
- Literature (as summarised, not read in full): Lost in Conversation, arXiv 2505.06120;
  When History Lies, arXiv 2608.06057; DriftBench, arXiv 2604.28031; choice-supportive
  bias in LLMs, arXiv 2507.03120; multi-turn red teaming, arXiv 2504.03174; false success
  in LLM agents, arXiv 2606.09863; The Unreliable Progress Bar, arXiv 2609.08589;
  Self-Correction Bench, arXiv 2507.02778; Stateless Language Agents, arXiv 2610.07625;
  StateMirror, arXiv 2610.32648; Better Harnesses, arXiv 2607.08938.
