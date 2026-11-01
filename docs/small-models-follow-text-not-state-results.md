# Small models follow text, not state: results

*Written 2026-11-02. A companion to `docs/small-models-follow-text-not-state.md`, which states
the problem and the literature's three mechanisms. This document reports what happened when
each mechanism was tested on the ACP conversation rig (`bench/acp_convo.py`). Results are under
`bench/results/convo/4b_di6_v1` to `4b_cf_base_v1`. Both models are local 4B-class models:
qwen3.5:4b and gemma4:e4b. Each mean is over n=3 runs per model unless stated otherwise. The
claims are as wide as these scripts and no wider.*

## In short

- **The model obeying its own earlier output was the largest effect and the clearest fix.**
  Re-attribution replaces the model's earlier refusal with a harness ledger line. With it,
  permission turn 2 passed 11/12 against 20/36 before. Turn 2s that ended with no call fell
  from 16/36 to 0 of 18 since. The literature predicted this and it held at 4B.
- **Lexical gates checked against the harness's own record kept tracker high.** The two
  admission-rule fixes removed the gate's misreads without making it hold honest refusals.
  qwen reached its first full tracker score.
- **The constraints script did not show constraint decay.** What it showed was a channel effect.
  qwen keeps a standing reply rule when it answers in text (13/16) and drops it when the answer
  is a `finish` summary (2/14). The fix is to stop asking for finish in a conversation (bead
  boubou-twe), not to pin the constraint. That fix is built and awaiting its rerun.
- **The state render, the lever with the largest effect in the literature, is still untried.**
  The shape it targets, a wrong state that compounds, has not recurred since the gate that
  catches it landed.

## The runs

| run (release) | change under test | tracker qwen / gemma | permission qwen / gemma | other |
|---|---|---|---|---|
| earlier readings (10-31 to 11-01) | (before gate 3c for tracker) | 0.25 / 0.58 | 0.83 / 0.67–0.83 | longform 1.00 / 1.00 |
| `4b_di6_v1` (17:14) | a bare name that is a workdir file is that file | 0.83 / 0.92 | 0.67 / 0.50 | longform 1.00 / 1.00 |
| `4b_e9i_v1` (18:19) | admission must be near the mention; a claim needs a done-word | 0.92 / 0.92 | 0.67 / 0.67 | longform 0.97 / 0.97 |
| `4b_alj_a_v1` (18:56, n=6) | re-attribution of a declined turn's reply | (not run) | 1.00 / 0.92 | |
| `4b_kxz_v1` (19:13) | a failure report naming no file needs a refused write | 1.00 / 0.83 | 1.00 / 0.83 | |
| `4b_cf_base_v1` (19:13) | (baseline) | (not run) | (not run) | constraints 0.67 / 0.87; follow_ups 1.00 / 1.00 |

Tracker has four turns, and each turn depends on the file state the last one left. Permission
has two turns, and the second asks again for a write the user declined in the first. Longform
has ten turns that are each self-contained. Constraints sets a reply rule in turn 1 and checks
it through tool-heavy turns. Follow_ups is code turns.

## Mechanism 1: the model's own output is privileged

**Prediction.** The literature reports that a model's bias towards its earlier answer
disappears when the same answer is attributed to someone else. It also reports that one
refusal in context sharply lowers later compliance.

**What we saw first.** Permission turn 2 was the residual that had not moved in seven runs.
Over 36 cells across six runs it split by what turn 1 left in context:

| turn 1 left | cells | turn 2 passed |
|---|---|---|
| at most one refusal text | 30 | 19 |
| two or more refusal texts | 6 | 1 |

Every miss was a turn 2 with no call: the model repeated its earlier refusal as though it were
policy. Nothing in turn 2 asked it to.

**What we built (bead boubou-alj, arm A).** The change applies only to what is sent to the
model. In an earlier turn where the user declined a write, the model's own replies after the
decline are left out of the request, along with its finish calls and the harness notes that
answered them. One line written by the harness takes their place:

    [ledger] turn 1: create_file /tmp/boubou_convo_perm.txt was declined by the user; no file was written.

The stored session and the editor's replay keep everything the model actually said. The
current turn is never changed.

**Result.** These are permission runs at n=6 per model.

| turn 1 left | before | with re-attribution |
|---|---|---|
| at most one refusal text | 19/30 | 8/9 |
| two or more refusal texts | 1/6 | 3/3 |
| all | 20/36 | 11/12 |

On all turn 2s the one-sided Fisher exact test gives p≈0.02. Counting the next run
(`4b_kxz_v1`) as well, 18 of 18 turn 2s since re-attribution made a call and 16 of 18 made the
right one. The backstop arm, a hold on a turn 2
with no call, was designed but was not needed and was not built.

**Reading.** This is the cleanest confirmation of the literature at our size. The model's
earlier refusal was not a fact it remembered. It was text in its own voice, and removing that
voice removed the obedience. The same fact stated by the harness did not trigger it.

## Mechanism 2: stale history hijacks the decision

**Prediction.** Rendering the state from the harness's ledger, instead of replaying the
transcript, should help the most of any change. The literature reports 0.47 to 0.75 at 1.7B
and 0.69 to 0.89 at 8B. A prompt line telling the model to ignore misleading history did
nothing.

**What we saw first.** In `4b_scope_v1` qwen tracker, the model created `openspec/tasks.md`
next to the real `./tasks.md`. Each later turn was correct against the wrong file it believed
in.

**What we built.** A gate, not a render (bead boubou-di6). Gate 3c holds the turn when the
instruction names a bare file that exists in the workdir and the write went somewhere else.
The hold names both paths.

**Result.** The wrong-directory write has not recurred in 18 tracker cells since: six each in
`4b_di6_v1`, `4b_e9i_v1` and `4b_kxz_v1`. The new hold never fired. The rule is live but
unexercised, so we have no fix to claim, only no regression.

**Reading.** We have not tested this mechanism's own remedy. The state render remains the
largest untried lever. It needs a script long enough for stale history to matter, because
tracker is four turns and now near its ceiling.

## Mechanism 3: standing constraints decay

**Prediction.** A constraint given early is evicted or weakened in later turns. Restating it
does not help, while pinning it with its source and keeping a gate does.

**What we saw.** The constraints script sets "start every reply with OK" in turn 1. Across the
two baselines (`4b_2026-10-31` and `4b_cf_base_v1`), qwen's 30 turns split by how the turn
ended:

| turn ended in | turns | reply began with OK |
|---|---|---|
| `finish(summary)`, summary used as the reply | 14 | 2 |
| plain text | 16 | 13 |

The pattern is not decay:
- Turn 1, the turn that states the rule, failed all 4 times it ended in finish.
- Turn 5, the last turn, is qwen's best.
- All three plain-text misses are the one turn with its own competing format rule ("names
  only, … nothing else").

A finish summary is written as a status report ("There are 3 markdown files under notes/"),
and the model does not apply a reply rule to it. gemma mostly answers in text and kept the
rule on 14 of 15 turns.

**What we built (bead boubou-twe).** In a conversation, text already ends the turn. The
`general` and `coding` prompts therefore lose their two sentences asking for finish, and the
unattended prompt keeps them. The prompt shrinks from 1298 to 1225 characters, and nothing is
added. The result is awaiting the rerun.

**Reading.** At least one apparent constraint failure was a register failure. The same model,
with the same rule in context, keeps it in one output channel and drops it in another. This
distinction is not in the three-mechanism table. It is cheaper to fix than decay, because it
means removing an instruction rather than adding one. Constraint pinning is not indicated by
anything measured so far.

What we have not measured is the decay of "change nothing else" in tracker. The rig carries
those checks forward itself (bead boubou-dby), and tracker's misses over these runs were
format and plan-tool shapes, not lost sections.

## The gates: false success checked against the record

**Prediction.** The false-success work says LLM judges barely detect false success, while
lexical classifiers do much better. It also says models miss their own errors but fix the same
error when it arrives from outside.

**What we changed.** Two fixes to gate 3c's rule for when a reply counts as admitting failure,
each driven by a live misread:
- **e9i: a failure word must be near the file it is about, and a claim needs a done-word.**
  "Done. I've broken the spec into tasks in tasks.md … with error handling" no longer passes
  as an admission. An offer of an alternative ("I can, however, write it to …") is neither an
  admission nor a claim.
- **kxz: a failure report that names no file needs a write this turn that actually failed.**
  "I encountered an internal system error" after only a plan-tool call no longer excuses an
  untouched `tasks.md`.

**Result.**
- **Holds converted to work.** The gate held 10 times in `4b_di6_v1`, and 9 of those holds led
  to a write. In `4b_kxz_v1` all 7 holds were followed by a write.
- **No false holds.** None of the 8 holds in `4b_e9i_v1` landed on an honest refusal.
- **Tracker.** qwen went from 0.83 to 0.92 to 1.00, its first full score. gemma is 0.83–0.92,
  and its misses are a checkbox format (`[ ] 1. [ ]`), not unwritten files.
- **Follow_ups.** The gate held nothing in 24 code turns, so it stays quiet where no tracker
  or docs file is named.

**Reading.** In this context the hold note is the "external input" that the self-correction
work describes. A model that claims it is done, and is told by the harness that the file is
unchanged, nearly always writes the file. The deterministic reading is what makes each miss
debuggable. Both admission fixes were found by running the exact live reply back through the
rule.

## What surfaced that the literature did not name

- **The channel effect.** It is described under mechanism 3.
- **The tool's own result as a false witness (bead boubou-7om, open).** qwen ticks a task in
  its plan tool, and the tool's result does not say the named file is untouched. The model
  then reports "Task 1 is marked complete". This has appeared twice in two runs.
- **Append used to revise (bead boubou-jn2, open).** gemma appended a second `## Owners` as a
  "revised" section instead of editing the first. It is the same shape as an earlier
  `## Decisions` cell.
- **A runaway text tool call (bead boubou-60j, open).** gemma wrote `list_dir` as text, then
  made up its output, then repeated a finish line until the output cap. The harness recovered
  the call but kept the made-up output in history. One cell so far.
- **The rig's answer to a substitute-path ask (bead boubou-jjh, open).** When gemma wrote the
  declined file name into the workspace in turn 2, the harness asked as designed. The rig
  allows every ask, which no real user would do there. This is a scoring artefact, not a model
  result.

## What this does not say

- The sample is small. Most cells are n=3 per model, and the re-attribution reading is n=6. Its
  direction and the two-or-more row are the reading, not the size of the effect.
- The models are 4B-class only. Nothing here was measured at 9B or 12B.
- Permission turn 2 for gemma has varied from 0/3 to 3/3 between runs before. One good run is
  not a level.
- The scripts are short, with ten turns at most. Nothing here is a claim about long sessions,
  which is also why the state render has had no fair test.

## Next

1. **Rerun twe:** constraints, tracker and permission (`4b_twe_v1`). Read the turns that end in
   finish, calls per turn, and whether gate 3c still fires when turns end in text.
2. **A state-render arm** behind an ablation switch, on a longer tracker script built so that
   stale history can mislead.
3. **The open beads above,** in the order their shapes recur.
4. **Constraint pinning and a pre-write path check** stay deferred until a script fails on
   them.

## References

- `docs/small-models-follow-text-not-state.md`: the problem, the mechanisms and the literature.
- `docs/changelog.md`, 2026-11-01 and 2026-11-02: every change and reading above, with the
  cells it came from.
- Beads: boubou-alj, boubou-di6, boubou-e9i, boubou-kxz, boubou-twe, boubou-7om, boubou-jn2,
  boubou-60j, boubou-jjh.
