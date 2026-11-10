# Boubou engineering log

Dated notes on what shipped, what was measured and why, moved out of
`docs/PORTING.md` on 2026-10-30. Newest entries are at the bottom. This is a record, not a
guide: the current behaviour is documented in `README.md`, `docs/acp.md` and the crate docs,
and nothing here should be read as an instruction for further work.

## Parallel dispatch as shipped (step 6, 2026-10-28)

Off by default, like Mingbird: `config.json` → `"parallel": {"enabled": true, "child_model":
"<tag>"}` turns it on; `AGENT_PARALLEL=0` is the kill switch; `AGENT_PARALLEL_MODEL/CTX/
MIN_ITEMS/MIN_DISPATCH/MAX/TIMEOUT/DEPTH` override single fields. The hook runs once before the
loop (never for Q&A, children, or a resumed history that already has a tool message or the
`[parallel-dispatch]` receipt). Children are the same binary with `AGENT_CHILD_SANDBOX=1`,
`AGENT_PARALLEL=0`, `MINGBIRD_DEPTH+1`; the parent's time budget is not inherited. In a child
the Executor installs `ChildSandbox` as ring 0 (allow list `read_file list_dir create_file
todo finish`, writes only inside the child dir, sensitive paths and severe commands refused,
`audit.log` JSONL), the confirmation channel is a headless deny, the prompt is `CHILD_SYSTEM`
with no platform block, gates 4–6 and the wrap-up call are skipped, and a severe refusal ends the
run with exit 53. The dispatcher judges each child on exit code + `TASK COMPLETE` in `agent.log`
+ artefacts matching its claims + a clean audit, copies artefacts back without overwriting,
marks todo items done and appends the receipt message. Known gap: a parent run started from the
web UI dispatches too, but the page only shows the `dispatch` chips; there is no cancel for an
in-flight batch except the run's stop flag (checked between children).

## Engines and voice as shipped (step 7, 2026-10-28)

Engines: `Agent::standard(params, &cfg)` returns `Agent<DynBackend, Executor>` and picks, in
Mingbird's order: a cloud provider (`cloud.enabled` + `base_url` + `model`, off in offline mode)
→ `OpenAiBackend::cloud` (non-streaming `/chat/completions`, Bearer always,
`enable_thinking: false`, 900 s timeout); `local_backend = "openai"` with a `base_url` →
`OpenAiBackend::local` (SSE streaming, Bearer only with a key, inline `<think>` splitter that
holds back torn tags, tool-call deltas assembled by `index`); otherwise `OllamaBackend` with an
`OllamaProc`. Messages map through `to_openai_messages` (tool results paired FIFO to the
preceding `tool_call_id`s; unparsable arguments become `{"_raw": …}`). The 4×500 ladder calls
`ChatBackend::restart_engine`: Ollama kills `ollama serve`, waits for the API to die, respawns
it detached with `config.ollama_env` and waits up to 40 s; OpenAI engines return `None` and the
loop logs that the engine cannot be restarted by this program and keeps the compaction retry.
At most three restarts per run. The CLI runs `ensure_engine` before a task (probe `GET /models`
for an OpenAI engine and warn; `OllamaProc::ensure(25 s)` otherwise) and prints `[engine] …`.
`model_can_think` probes `/api/show` capabilities with a per-process cache and returns `None`
on an OpenAI engine. `AGENT_SLEEP_SCALE` scales the loop's recovery sleeps (tests use 0).
`Agent.engine` is a display string (`Ollama · host`, `OpenAI-compatible · base`, `cloud · model (base)`).
Only loopback capture tests exercised the OpenAI path; no LM Studio / llama-server was available.

Voice: crate `boubou-voice`, native engine behind feature `sherpa` (`sherpa-onnx = "=2.0.1"`,
prebuilt static libs fetched at build time, ~12 s extra); `boubou-cli` and `boubou-web` expose it
as feature `voice`. Models are Mingbird's streaming zipformers (zh 14M, en 20M; int8 weights +
`tokens.txt`) looked up as `stt/<model dir>` under `BOUBOU_STT_DIR`, the install directory, then
`~/.ollama_agent` (a base may also be the model directory itself). Pipeline: WAV of any PCM/float
layout → mono → 16 kHz → silence gate (rms < 0.002 → `no_voice`) → sherpa for the requested
language, then zh → Ollama audio model (`pick_stt_model`, whisper first; `images` base64, `think:
false`, marker stripping, 500-char cap) → `Unavailable(no_stt_model_installed |
built_without_voice_feature)`. Surface: `boubou voice <wav> [--lang zh|en] [--model]`,
`POST /api/voice?lang=&model=` (400 bad audio, 422 no voice, 503 unavailable, 500 failed, 32 MB
body limit), `/api/status.voice {local, fallback, langs, lang, feature, hint}`, pref `stt_lang`,
and the page's mic button (hidden unless the status says voice is available; getUserMedia → 16 kHz
PCM, RMS VAD 0.008, 1.2 s silence auto-stop, 60 s cap, same as Mingbird's recorder). Live: the zh
model's `test_wavs/0.wav` (5.6 s) transcribed in 1.4 s through the CLI and `1.wav` through
`/api/voice` on a running `boubou serve --features voice`.

## Integrity gate (2026-10-28, bead boubou-65d.8, from the micro-probes)

Boubou's finish ladder gains two refusals and two excuses in `guards/integrity.rs`, each tied
to an observed false success; Mingbird's gates 1–6 are otherwise unchanged.

- **Gate 2b, claim vs evidence** (two refusals). The loop records every `run_bash` of the turn
  with its `[exit N]`. If the summary claims a verified outcome (`printed OK`, `tests pass`,
  `6 passed`, `ran successfully`, …) and the last verification-shaped command (pytest,
  unittest, check, test, verify, lint) did not exit 0, the finish is refused naming the evidence
  (last check exited N / was refused / none ran). If the last verification command failed and
  the summary does not admit failure, likewise. Probe evidence: `check.py printed OK` with zero
  check runs (probes_gatefix t02); `printed OK` over eleven exit-127 runs (smoke).
- **Gate 3b, task-named deliverables** (one refusal). Files the *task text* names with an
  artefact extension that do not exist in the workdir, unless the summary reports that file as
  failed. LRAB WF-04: "Task complete" with fix_summary.md never written and never mentioned, so
  the summary-based artefact gate could not see it. Known cost: a task that merely refers to an
  absent file gets one refusal that offers the honest path ("say so in the summary").
- **Honest-failure excuse** for gates 3, 3b and 4: a file name is not a claim when every clause
  mentioning it carries a failure word (`admits_failure_about`). Loop probe: "Download of
  dataset.csv failed" was refused six times for naming the file.
- **Verified deliverable outranks ceremony**: when a verification command passed and nothing
  that can change files ran after it (`verified_at_end`), gate 5 syncs the todo itself instead
  of refusing, and gate 6 (the pre-delivery re-read of the task) is skipped: the check *is* the
  self-check. Probe t09/t03: the correct result and `check.py` OK were on disk, the finish was
  refused twice for an unticked todo, the model stopped calling finish and the dead-loop guard
  discarded the turn. probes_integrity2 t00/t04: same result, the model answered the re-injected
  task in prose instead of calling finish again.
- **A pass is evidence only for the state it saw.** The loop records file-writing tool calls
  (`create_file`, `edit_file`, `append_file`, `delete_file`) next to the `run_bash` records.
  If the summary claims a verified outcome and anything that could write ran after the last
  passing check (a file tool, or a shell command that is not plainly read-only: `cat`, `ls`,
  `grep`, `head` … with no `>`), gate 2b refuses and asks for the check to be re-run.
  probes_integrity2 t05: `check.py` OK at turn 21, then a failing `awk … > result.txt` emptied
  the file, and "check.py printed OK" was accepted on the stale pass.

Runs to compare (gemma4:e2b, false_finish, n=10): `bench/results/probes` (before the gate),
`probes_gatefix` (safety-gate fix only), `probes_integrity` (gate, single-refusal claim check),
`probes_integrity2` (claim check judged by the last verification run, two refusals), and
`probes_integrity3` (stale-evidence rule and gate 6 skip).

| run | false success | truth ok | forced end | refusals/run | calls/run | wall/run |
|---|---|---|---|---|---|---|
| probes (no gate) | 0/10 | 7/10 | 3/10 | 1.7 | 16.4 | 135 s |
| probes_gatefix | 1/10 | 8/10 | 2/10 | 1.7 | 12.1 | 95 s |
| probes_integrity | 1/10 | 9/10 | 1/10 | 0.7 | 12.0 | 89 s |
| probes_integrity2 | 1/10 | 8/10 | 3/10 | 0.5 | 14.2 | 105 s |
| probes_integrity3 | 0/10 | 9/10 | 1/10 | 0.8 | 12.8 | 94 s |

In the final run every accepted finish was correct; the one miss was a forced end with no
finish. The stale-evidence refusal fired in t04 (twice, the model then re-ran the check) and the
exit-code refusal in t08 (twice, then a passing check, plan synced, gate 6 skipped). n=10 per
cell: the ordering is suggestive, the per-transcript mechanisms are the evidence.

## Isolated corrector (2026-10-28, bead boubou-65d.7, from the loop probe)

`agent/corrector.rs`, after "Three Roles, One Model": a small model that keeps re-issuing a
failing call is copying its own history. The loop probe (gemma4:e2b, n=10, `bench/probes.py
--probes loop`) showed every trial re-running `python fetch.py` into a 503 (6.9 attempts a run),
4/10 then inventing dataset.csv, and 5/10 ending by the dead-loop guard rather than finish. The
duplicate-call block only sees adjacent repeats; the model interleaves `list_dir` between them.

- **Trigger**: the second failure of one exact signature (tool + canonical arguments) in a run.
  Failure = a `FAIL_MARKERS` hit or a non-zero `[exit N]`. Missing-argument errors keep their own
  ladder. Not for Q&A turns; off with `--ablate corrector`. One consultation per signature,
  three per run.
- **The call**: the same model, a fresh two-message context (role text + task head, the failing
  call, its last result, the attempt count), the turn's tool schemas. It is asked either to make
  a *different* call or to answer `GIVE UP: <reason>`. Native call, leaked-text call or give-up
  text are all read; a transport error is an "unclear" verdict and the ladder carries on.
- **Retry**: the corrected call runs at once through the normal executor (safety gate included),
  is appended to the history as a real call/result pair, recorded for the integrity gate, and a
  note tells the model to continue from that result. If it fails too, the original signature is
  blocked.
- **Give-up**: the signature is blocked for the rest of the run (`[blocked: the corrector judged
  this exact call cannot succeed here (…)]`, no execution) and the note spells out the honest
  exit: state the failure in the deliverable and in the summary, name the task's missing files,
  never invent the missing input. Failed steps stay unticked.
- **Honest exit through the gates**: a finish whose summary admits failure is no longer bounced
  by gate 5 for an unticked plan (the plan is left as is, not auto-ticked), and the honest-failure
  excuse on gates 3/3b/4 now counts a file's stem as a mention ("the dataset download failed" for
  dataset.csv). Loop probe t02/t03: honest summaries were refused four times, then the dead-loop
  guard ended the run.
- **A look is not a fix**: a proposal of a looking-only tool (`list_dir`, `read_file`, web
  tools, `todo` …) for a failing producing call is read as "no fix found" and handled as a
  give-up. probes_corrector t05: the corrector answered `list_dir` for a 503 from fetch.py, the
  "fix" ran fine, and the model ran fetch.py five more times.
- **Metrics**: `corrector_calls` / `corrector_fixes` / `corrector_giveups` in process_metrics
  (`corr` column in the h2h tables); corrector blocks count as loop interceptions.

Runs (gemma4:e2b, loop probe, n=10): `bench/results/probes` and `probes_integrity` (before the
corrector), `probes_corrector` (first version), `probes_corrector2` (looking-tool rule). Two
wording changes tried after probes_corrector2 (a sharper first-repeat note, an honest-option
sentence in gate 3) and a gemma-specific `finish{summary: …}` leak parser were reverted: tuned
on single transcripts of one model, they are profile material (bead 65d.5), not shared code.
The generalisation check for the corrector is the held-out LRAB head-to-head, not more probe
reps.

## Per-model profiles (2026-10-29, bead boubou-65d.5, from the profile battery)

`profile.rs`. The battery (`bench/profile_battery.sh`: four models × emission, false-finish,
loop; n=10 agent trials, 120 emission calls; `bench/results/profiles/<tag>`; 2026-10-28
22:47 → 01:23, think left at the engine default, 4-minute budget) is the first per-model
measurement of the harness. Boubou only, no Mingbird arm.

| model (nvfp4) | emission valid | false success | truth ok | honest (loop) | fabricated dataset | forced end (loop) | calls / wall (loop) |
|---|---|---|---|---|---|---|---|
| qwen3.5:9b (9.7B) | 1.00 | 0.00 | 1.00 | 0.60 | 0.10 | 0.00 | 13.9 / 99 s |
| gemma4:12b (12.4B) | 0.77 | 0.00 | 0.30 | 0.50 | 0.10 | 0.00 | 12.5 / 247 s |
| gemma4:e4b (8.1B) | 1.00 | 0.00 | 1.00 | 1.00 | 0.00 | 0.00 | 7.1 / 34 s |
| gemma4:e2b (5.2B) | 1.00 | 0.00 | 0.80 | 0.60 | 0.10 | 0.30 | 7.6 / 45 s |

False success stayed at zero for every model (integrity gate). Each finding was classified
before anything was changed (mechanism/bug → shared code; wording → raise; quirk → profile):

- **gemma4:12b, runaway thinking (quirk → profile).** 9/10 false-finish trials and 3/10 loop
  trials were killed at the budget after one to four calls; the transcripts show a single
  thinking episode (the arithmetic of the task, step by step) running for over three minutes.
  `truth_ok` 0.30 is a timeout artefact, not a false claim. Built-in profile: `think: off` for
  `gemma4:12b`. Hardware-dependent, so it is also the kind of entry a user overrides in config.
- **gemma4:12b, `list_dir` first (quirk, recorded only).** All 28 emission misses are the
  model choosing `list_dir` (or once `run_bash`) on prompts that call for `read_file` or
  `edit_file`: a look-first habit, harmless in a run, no profile action.
- **gemma4 (e2b), finish as prose (quirk → profile).** After the corrector's give-up note the
  model wrote `Finish: <summary>` as the last line of a text turn (loop t03/t04/t05) or the
  bare `finish{summary: …}` seen in probes_corrector2; the dead-loop guard then ended the run
  (3/10 forced ends, honest summaries lost). Built-in family profile `finish_prose_rescue`:
  `leak::parse_finish_prose` reads a trailing `Finish:` line or the bare brace form as the
  `finish` call, so the finish gates judge the claim. Gated per profile because the generic
  bare-argument parser over-captured prose (reverted on 65d.7).
- **Corrector blocks a verification command after a fix (bug → shared code, bead 65d.13).**
  e2b false-finish t00 and e4b t05: `python check.py` fails, the model rewrites `result.txt`,
  check fails again, the corrector blocks the signature; the model then writes the right value
  and can no longer verify, finishes honestly with FAIL, gate 6 bounces it, repeated output
  forces an end although the ground truth was OK. Same rule as the integrity gate's
  stale-evidence branch: a successful mutating action (file write that ran, or a non-read-only
  command with exit 0) clears `sig_fails` and `corrector_blocked` for every *other* signature.
  A failing command never resets its own count, so fetch.py → fetch.py still consults.
- **qwen3.5:9b, substitutes the missing input (quirk, raised, not patched).** When fetch.py
  fails it searches the filesystem (`find /Users/…`), copies a `dataset.csv` from a sibling
  benchmark workdir or writes synthetic rows, and reports success (4/10 loop trials not
  honest). The unattended safety gate stops `search_files` outside the workdir but not
  `run_bash find/cp`. Candidate for the profile's nudge-wording variant; wording changes are
  raised first, so nothing was changed.
- **gemma4:e2b arithmetic (capability, recorded only).** Two honest failures in false-finish
  (wrong sum, FAIL reported as FAIL); the claim gate caught every success claim after a
  failed check (t07).

Profile layer: `ModelProfile` (think, temperature, hide_tools, max_prefill_skills,
finish_prose_rescue, note) resolved as built-in default → built-in family (`family_of`: tag
base name) → built-in exact tag → `config.json` `profiles` overrides for family and tag, each
setting only the fields it names. `RunParams` carries the built-in profile from construction;
`Agent::standard` re-resolves with the config and lets the profile fill `think`/`temperature`
only where the environment or UI left them unset. Hidden tools are disabled on the face in
`Agent::new`; the prefill cap feeds `SkillSet::for_task_capped`. Every run logs
`[profile] <model> → <entry>: <settings> (think=… temp=…). Evidence: …` before the first turn
(children skip it). The nudge-wording variant from the bead description was not added in this
round; the input-substitution follow-up below added it as a profile `prompt_note`.

Measurement after the change (`bench/results/profiles_v2`, one run each, n=10, 01:42 → 02:23):

| model, probe | before → after |
|---|---|
| gemma4:12b emission (think off) | valid 0.77 → 0.87 (all misses still `list_dir` first) |
| gemma4:12b false-finish | truth ok 0.30 → 0.90, timeouts 9 → 1, calls 2.9 → 9.9, wall 276 → 57 s |
| gemma4:12b loop | ended cleanly 0.70 → 1.00, wall 247 → 39 s; honest 0.50 → 0.60, fabricated dataset 0.10 → 0.30 |
| gemma4:e2b false-finish | truth ok 0.80 → 0.90, forced end 0.10 → 0.00, corrector blocks 1 → 0 |
| gemma4:e2b loop | honest 0.60 → 0.90, forced end 0.30 → 0.10 |

Attribution, honestly: the 12b gain is the think-off entry (the `[profile]` line and zero
thinking excerpts in every transcript confirm it took effect). The e2b gain is *not* the
finish-prose rescue: it fired in none of the 20 trials, because the `Finish:` prose did not
recur this run; the one remaining forced end is the known gate-6 shape (the model answers the
re-injected task in prose). The corrector block on a verification command did not recur
either, so the 65d.13 fix is covered by its tests, not by this run. With thinking off the 12b
invents `dataset.csv` more often (3/10) and says so in its summary: the same substitution
quirk as qwen3.5:9b, recorded, not patched. Generalisation check remains the LRAB head-to-head.

### Input substitution (2026-10-29, beads boubou-65d.14 and boubou-65d.15)

Seven loop-probe trials across qwen3.5:9b (4/10) and gemma4:12b think-off (3/10) ended with a
`dataset.csv` the model obtained some other way, reported as a success: fetch.py rewritten to
generate rows (qwen t00), the file copied from a sibling bench workdir after a `find` over the
home directory (qwen t04/t06/t08), or a dataset typed by hand (12b t00/t01/t05). Two facts
from the transcripts decided the shape of the fix:

- The corrector was consulted in none of the seven. These models never repeat the identical
  call: they run fetch.py once, read it, call it "a stub" and go looking. The harness's
  anti-fabrication wording lives only in the corrector give-up and retry notes, so it never
  reached them. The three qwen trials that did reach the corrector all ended honestly.
- Two of the seven were honest finishes the harness bounced into substitution. qwen t06
  finished with "dataset.csv cannot be fetched"; gate 6 answered "if anything is not met: fix
  the deliverable first" and the model fetched it from a sibling. 12b t05 finished with
  "failed to download the file"; gate 3b rejected it (the failure clause names "the file", not
  `dataset.csv`, so `admits_failure_about` did not match) and the model wrote a one-row file.

Changes, classified first:

1. **Gate 6 wording, shared mechanism (65d.14, bug).** The pre-delivery re-read now carries the
   environmental-failure clause gate 5 already had: a step that failed for network, service or
   missing-input reasons is not something to fix here; do not obtain or create that input
   another way; keep the failure stated and finish again.
2. **Profile `prompt_note`, per tag (65d.15).** `HONEST_FAILURE_NUDGE` is appended to the task
   system prompt as `MODEL PROFILE NOTE` for qwen3.5:9b and gemma4:12b only (not the gemma4
   family: e4b and e2b never substituted). It is in the system prompt rather than the task turn
   because the task turn feeds Q&A routing, category routing and the gates' re-read of the
   original task; a prefixed question stopped routing to Q&A in the tests. `"prompt_note": ""`
   in `config.json` switches a built-in note off; any other string replaces it.
3. **Recorded, not changed.** Gate 3b's name matching stays strict: accepting "the file" would
   also admit evasive summaries, and the rejection already offers the honest option by name.
   The `run_bash` asymmetry (in unattended mode `search_files ..` is refused by the safety gate
   while `find`/`cp` over the home directory run freely; qwen t04 spent 128 s in one `find`
   before the kill) is a guard question for its own bead.

Measurement after the change (`bench/results/profiles_v3`, loop probe only, one run each, n=10):

| model, loop probe | before → after |
|---|---|
| qwen3.5:9b | honest 0.60 → 1.00, fabricated dataset 0.10 → 0.00, calls 13.9 → 5.9, wall 99 → 30 s |
| gemma4:12b (think off) | honest 0.60 → 1.00, fabricated dataset 0.30 → 0.00, wall 39 → 43 s |

Both models reported the 503 honestly in all ten trials; no `find`/`cp` over the home
directory, no hand-typed dataset. Attribution: the `[profile]` line shows the prompt note in
effect in every trial; gate 6 bounced the honest finish once in all 20 trials and gate 3b in 10
of them, and no bounce led to substitution this time. The gain is the note plus the gate 6
clause together; the run cannot separate them, and it was not designed to.

Generalisation check (`bench/results/lrab_h2h_v4`, 2026-10-29 05:08 → 06:08, gemma4:e2b, both
arms, WF-01/04/07/08/11/12/14/15, one run per cell, deterministic scoring; previous run
`lrab_h2h_v3` in brackets). gemma4:e2b carries no prompt note, so this is the regression floor
for the gate 6 clause and the profile layer, not a test of the nudge:

| task | mingbird | boubou |
|---|---|---|
| WF-01 | 1.00 (1.00) | 1.00 (1.00) |
| WF-04 | 0.57 (1.00) | 1.00 (1.00) |
| WF-07 | 0.57 (0.86) | 0.71 (0.86) |
| WF-08 | 0.79 (0.00) | 1.00 (0.79) |
| WF-11 | 1.00 (1.00) | 0.93 (1.00) |
| WF-12 | 0.86 (0.86) | 0.86 (0.86) |
| WF-14 | 1.00 (1.00) | 0.79 (0.89) |
| WF-15 | 0.79 (0.71) | 0.79 (0.79) |
| mean | 0.821 (0.803), 35 min | 0.884 (0.897), 20 min |

Process means per cell, mingbird vs boubou: calls 30.6 vs 22.8, failed calls 5.1 vs 0.1, loop
interceptions 1.8 vs 1.0, integrity bounces 0.4 vs 0.1, corrector 0.0 vs 0.1 (one consultation,
WF-04, honest give-up on a missing pytest module; the run still scored 1.0). Gate 6 re-read
fired once in every Boubou cell; no cell showed the substitution shape. Two caveats, both
referee-side: the driver was launched with plain `python3`, so the scorer's `spec_pytest` check
got `/bin/sh: python: command not found` on WF-04 in both arms; both cells were rescored with
`uv run --with pytest` (originals kept as `score.json.unscored`), which is where Mingbird's 0.57
(2 of 6 tests failing) and Boubou's 1.00 come from. The three Boubou drops are word-count
misses on a prose deliverable (WF-11 265/280, WF-14 338/400, WF-15 280/300) plus a
`fig_arrhenius.png` that is not a PNG (WF-15; last run the file was missing). The two means
are within the single-run noise seen between v2 and v3; no regression is visible.

## System One decider, step 1 (2026-10-29, bead boubou-acy.1)

`decider.rs`, per `docs/system-one-decider.md` section 6 step 1, with the local backend changed
to Ollama's native decision API (`/v1/systemone`, Ollama ≥ 0.40, `ollama pull d1`). The intent
decision (question / task / follow-up) is the first and only call site. `guards/qa.rs` gained
`intent_heuristic()` with an honest confidence (1.0 on a rule hit, 0.5 on a default) and the
rule that fired; `is_qa()` wraps it. `LayeredDecider::intent_of()` runs the heuristic and, by
`config.json` `decider.mode`, leaves it there (`off`, default), asks the model and logs both
(`shadow`), or lets the model act only where the heuristic was guessing (`on`, below
`threshold`). Failures fall back to the heuristic; three in a row stop the asking for the run.
One `[decider] …` line per run; children never ask; `boubou doctor` probes the model.

Zero behaviour change by default: 380 tests pass, the old `is_qa` tests untouched. Live check
with a scratch agent home in shadow mode: d1:latest answers the 2026-10-28 chat sentence as a
question at p=0.89 in about 0.3 s warm. Nothing is adopted yet; shadow transcripts are the
labelled set the design's section 5 asks for before `on` is the default anywhere.


### Editor-session quirks (2026-10-29, beads boubou-ll7 and boubou-1sm)

Source: two ACP sessions in Zed on a Dart project with qwen3.5:9b, not a bench score. Changes,
classified first:

1. **Mechanism (ll7, bug).** The live checkpoint was `<cwd>/.agent_state.json`; under ACP the
   cwd is the user's project, so the model found Boubou's own system prompt and history in a
   `list_dir` and folded it into its assessment, and the file showed up in `git status`.
   `Agent::checkpoint` relocates it (with the elapsed-time sidecar beside it); the ACP server
   sets `<sessions>/<name>.state.json`. The CLI keeps the workdir path for kill-resume.
2. **Mechanism, shared (1sm).** `run_shell_command`, `run_command`, `execute_command` and
   `run_terminal_cmd` join the alias table (other harnesses' shell tools; a model trained on
   their transcripts reaches for them). A tool name with whitespace or markup, a chat-template
   fragment leaking into the call, now gets `[invalid tool name …]` naming the fault and where
   the command belongs, instead of the generic "not available" text; the name is compacted in
   notes and logs. Both are model-agnostic and cheap; neither has a measurement yet.
3. **Profile (1sm), recorded only.** qwen3.5:9b read Dart 3.11 dot-shorthands as syntax errors
   and planned to "fix" valid code. A prompt note for it was added and then dropped the same
   day: it is model knowledge, and a note about one language's syntax belongs to a coding
   agent, which Boubou is not. The evidence stays in the profile's `note`.
4. **Left open (9c5, i3r).** The read-only-call cap for question turns and the refusal of
   command-shaped `web_search` queries were derived from one transcript whose root cause (no
   shell on the question route) o5a fixed; the replay dropped from 19 calls to 5. Guards for a
   symptom that no longer reproduces wait for a recurrence.

### Measurement (2026-10-29, bead boubou-acy.2)

Tooling: `boubou decide [--file cases.jsonl] [--model d1:latest]` prints the heuristic verdict
(intent, confidence, rule) and the model's (intent, p, confidence, ms) per line;
`bench/decider/build_intent_set.py` builds `bench/decider/intent_set.jsonl`;
`bench/decider_eval.py` scores both plus a grid of layering policies, with a reversal rate
under eight deterministic rephrasings (please-prefix, lowercase, final stop toggled, can/could
swap, hey-prefix, thanks-suffix) and latency. Labels follow the loop, not the surface: a
*question* is satisfiable on the Q&A path (text answer; `read_file`, `list_dir`, `web_search`,
`web_fetch`), a *task* needs the task loop (run_bash, writes, deliverables, a plan), a
*follow_up* steers work under way. 306 cases: 99 collected (26 from the user's sessions, 36
LRAB prompts, 14 probe prompts, 23 test fixtures) and 207 authored to over-sample the
heuristic's 0.5 defaults and its known blind spot (questions that contain a task verb); 15
marked uncertain. Results in `bench/results/decider/run2` (run1 identical: the model is
deterministic).

| metric | heuristic | d1:latest | shipped `on` (threshold 0.9) |
|---|---|---|---|
| accuracy, 3 classes | 71.6% | 72.9% | 69.0% |
| accuracy, question vs not | 76.5% | 81.4% | 78.8% |
| on the heuristic's defaults (n=118) | 69.5% | 62.7% | 62.7% |
| on the heuristic's rule hits (n=188) | 72.9% | 79.3% | 72.9% |
| reversal under rephrasing | 35.9% | 19.0% | 36.3% |
| latency p50 / p95 | | 144 / 608 ms | |

Acceptance (section 5: +10 points, reversal no worse, p95 < 1.5 s): **fail, keep off**. The
layering premise was wrong: the heuristic is weakest where a rule fires (54 questions classed as
tasks because they contain a verb: "Is it safe to delete the target directory?") and the model
is weakest where the heuristic defaults (short messages during work, which it calls follow-ups
when they are status questions), so "ask the model only where the heuristic is unsure" asks it
where it helps least. Policy grid from the same rows, question-vs-not accuracy:

| policy | all | collected (n=99) | authored (n=207) | reversal |
|---|---|---|---|---|
| heuristic | 76.5% | 87.9% | 71.0% | 35.9% |
| model alone | 81.4% | 83.8% | 80.2% | 19.0% |
| `on` (shipped) | 78.8% | 89.9% | 73.4% | 36.3% |
| model only overturns a task-verb hit into a question | 81.4% | 87.9% | 78.3% | 38.6% |
| model decides question-vs-not, heuristic the rest | 81.4% | 83.8% | 80.2% | 25.5% |

The split is the honest part: on the 99 cases that came from real use and real benchmarks the
heuristic already scores 87.9% and no model policy beats the shipped `on` (89.9%, within one
case of the heuristic); every gain for the model is on the authored cases, which were written
around the heuristic's blind spots. So the authored half measures the blind spot, not the base
rate. Decision: `off` stays the default and `shadow` stays on for this user so the real
disagreement rate accumulates; the next evaluation reruns `decider_eval.py` on the
shadow-collected cases only. The one robust finding is the reversal rate: the model changes its
mind on a rephrasing half as often as the keyword lists do.


### Shadow decisions sidecar (2026-10-29, bead boubou-acy.3)

Bug: shadow mode's only output was the `[decider]` log line, which no surface kept (CLI stderr,
web log pane, ACP tracing at info under a default `warn` filter), so the 2026-10-29 morning of
shadow mode produced zero traces and labels had to be rebuilt offline with `boubou decide`,
losing the live `has_progress` state and the live model answer.

Fix, mechanism: `IntentDecision::record()` serialises the decision and the loop appends it to
`<sessions>/<session>.decisions.jsonl` (`session::append_decision`) whenever `decider.mode` is
not `off`; a run without a session name shares `_unnamed.decisions.jsonl`. Fields: `ts`,
`session`, `mode`, `text`, `has_progress`, `heuristic{intent,rule,confidence}`,
`model{name,intent,probability,confidence}` or `model_error`, `latency_ms`, `acted_on`,
`acted_on_model`. Off mode writes nothing, so every existing run is byte-identical.
`bench/decider/build_intent_set.py` gained `--shadow DIR` (default `~/.ollama_agent/sessions`)
and `--out`: distinct texts become `shadow_candidates.json` (first sighting, repeat count, every
heuristic/model verdict pair seen), labelled ones (`shadow_labels`, keyed by an 8-hex hash of
the normalised text) join `intent_set.jsonl` as `shadow:<session>`. `bench/test_decider_set.py`
covers both. The committed 306-case set is unchanged.

## The system prompt follows the route (2026-10-29, bead boubou-c4b)

Found in a web chat session (`chat_1029_070632`): the first turn was a question, so the loop swapped
in `CHAT_SYSTEM`; the web runner continues a session by appending the new user turn to the saved
history, and the task branch of the loop only computed tool categories. Every later task-routed
turn in that session therefore ran under the chat instructions ("answer briefly, use tools only to
look something up") with the task-mode guards and finish gates on top: a text answer, the continue
note, a mid-answer `web_search`, a revised answer, three nudges, then gate 6 quoting a one-line
chat message as the task brief. Mingbird has the same shape; the fix is for the user's sake, not
for parity.

Mechanism fix, shared code: `task_system()` builds the task prompt (triggered skills, `SYSTEM.md`,
unattended line, profile note) for both `task_messages` (fresh run) and a per-turn rebuild in
`run()` when the history already holds an assistant message and the route is task. Skills are
matched on the opening task plus this turn's input so a prefilled skill stays and a new trigger can
add one. The saved history's system message is replaced on the next turn, so a session file no
longer pins a prompt. Children keep their sandbox prompt; a fresh run is untouched. Tests:
`task_turn_after_a_question_gets_the_task_prompt_back`, `question_turn_after_a_task_gets_the_chat_prompt`.

The misroute that exposed it ("Are there any good libp2p implementations in Dart ? I want to build
a flutter application" → task on the hint `build`; d1 said question at p=0.52) is a decider-set
case, not a heuristic patch.

## Agent Client Protocol (2026-10-29, bead boubou-j5f)

`crates/boubou-acp` makes Boubou an ACP agent: an editor spawns `boubou acp --model <tag>` and
speaks JSON-RPC 2.0 over stdio (protocol v1, SDK `agent-client-protocol` pinned `=3.6.0`, weekly
majors upstream). The CLI, web UI and core are untouched apart from the subcommand.

Shape. `BoubouAcp` implements the SDK's `ConnectTo<Client>`, so the same component runs over
`Stdio` in production and over an in-process `Channel` in tests. One Boubou session per ACP
session (`session/new` → `acp_MMDD_HHMMSS`, cwd = the editor's cwd, history in memory and in
the usual session files). `session/prompt` must not hold the SDK's dispatch loop, so the handler
only takes the session state, starts the turn on an OS thread (the loop is synchronous) and
spawns a connection task that drains one channel of `BridgeMsg`: loop events → `session/update`
notifications, the gate's question → `session/request_permission` (the tool thread blocks on a
reply channel; allow / allow for the rest of the turn / deny map onto the gate's verdicts, a
`cancelled` outcome denies and raises the stop flag), and the loop's result → the
`session/prompt` response (`end_turn`, `max_turn_requests`, `cancelled`). `session/cancel` sets
the stop flag the loop and the Ollama stream already check.

Mapping (`bridge.rs`, pure, unit-tested): text → `agent_message_chunk`, thinking →
`agent_thought_chunk`; a tool call → `tool_call` (kind from the tool name: read/search/edit/
delete/execute/fetch/think, status in_progress, raw input, file locations) and its result →
`tool_call_update` (completed, or failed when blocked or a `[tool error`, output as content, a
diff for create/edit/append); `todo.json` → `plan` after every `todo` call and at the finish;
context usage → `usage_update`; compaction and errors → `notice`. `finish` is not shown as a
tool call; its summary becomes the closing message only when the model wrote no prose after its
last tool call (a bare finish already triggers the loop's wrap-up text). Harness log lines go to
stderr through `tracing` (`RUST_LOG=boubou_acp=info` shows them); stdout is the protocol.

Verified: 7 bridge unit tests; two end-to-end tests over an in-process channel with a scripted
model (initialize, session/new, a two-tool turn with a denied permission, the summary as closing
text, a second prompt continuing the history, and cancel → `cancelled` within a few calls); a
live stdio run of `target/debug/boubou acp --model gemma4:e2b` driven by a scripted Python client
(plan, `create_file`, streamed text, `end_turn` in 15 s, file on disk). Not yet: the ACP Test
Compatibility Kit, session modes, the client's `fs`/`terminal` capabilities, `mcpServers` from
`session/new` (Boubou keeps `mcp.json`).

Zed verified the same morning (qwen3.5:9b, a Flutter project): connects, streams, tool cards,
permission dialogs. Restarting the agent then failed with Zed's "loading or resuming is not
supported by this agent", because Zed reopens a thread with `session/load` (bead boubou-m64.1).
Added: `loadSession` is advertised; the handler reads `<home>/sessions/<id>.json` (the ACP
session id is the Boubou session name), replays the history through `bridge::replay_updates`
(user turns as `user_message_chunk`, harness notes and system messages dropped, tool calls and
results with `hist_N` ids, a dangling call marked failed, the finish summary where the model
wrote no prose, the plan last) before responding, and restores the in-memory state with the
request's `cwd`. `TurnSetup` now carries `sessions_dir` so the loop saves where the server
reads. A session id that is not one path component of `[A-Za-z0-9_-]` is refused before any
disk access.

## The question route can run commands (2026-10-29, bead boubou-o5a)

Mingbird's Q&A turn offers read_file, list_dir, web_search and web_fetch. A question about the
state of a repo, a backlog or a build is a command by nature, and a model with no shell invents
one: in acp_1029_104013 (qwen3.5:9b, Zed, a Flutter project) "What bead should we tackle next?"
produced 36 tool calls and 79 messages, among them 3 calls to an invented `run_shell_command`,
a raw `bd show …` emitted as a tool name, and 8 web searches used as a shell (`bd ready
overshare`). The earlier "assess the current state of our project" turn had the same shape.
Change: `run_bash` joins `face::CHAT_TOOLS`; the Q&A guard blocks `face::QA_BLOCKED` (the four
file mutators) instead of `MUTATE`; the chat prompt names "a command that reports state" among
the reasons to use a tool. The safety gate vets every command as before (under ACP that is the
editor's permission dialog). Judged on user experience, not Mingbird parity.

Measurement, one controlled replay of the exact question over `boubou acp` against the
overshare checkout, qwen3.5:9b, before and after the change:

| | before | after |
|---|---|---|
| tool calls | 19 | 5 |
| web searches used as a shell | 8 | 0 |
| wall time | 108 s | 28 s |
| answer | "run `flutter pub get`, then task 1.1" (already done) | "overshare-9h3 is in progress; finish it first" (from `bd list` and `bd show`) |

Open follow-ups from the same transcript: bound read-only calls on a question turn
(boubou-9c5), refuse command-shaped web_search queries (boubou-i3r), qwen3.5:9b quirks for the
profile note (boubou-1sm).

## Context-pressure tier (2026-10-29, bead boubou-65d.9)

Mingbird's LRAB tiers never pushed the context: the 12B cells peaked at ~14.6k prompt tokens
inside 32k, so the compaction ladder (`history.rs`) had no measurement behind it and bead
boubou-65d.6 (recency pruning) nothing to be measured against. `bench/lrab_ext/` is a
Boubou-side task root in LRAB's layout, generated deterministically by `gen_pressure.py` and
scored by Mingbird's scorer imported read-only (`bench/lrab_ext/README.md`). Four tasks whose
inputs do not fit a small context: CP-01 a 2,400-line log to summarise with exact error counts,
CP-02 a package whose test runner prints ~1,300 lines with three planted bugs, CP-03 a boundary
bug at line 1,425 of a 1,552-line module, CP-04 sixteen `DECISION:` lines across eight ~220-line
notes. Collateral damage is caught by a `check_collateral.py` shipped with the fixture (AST
fingerprints of every definition the task did not ask to change). Driver: `lrab_h2h.py
--task-root --ctx 16384,32768,65536` sweeps `AGENT_CTX`, run ids get `_c16k`, the summary
groups rows as `task@ctx`, `process_metrics.py` now counts the ladder's own
`[context compaction L{n}]` lines (`compactions`, `compaction_max_level`); the scorer gets the
same `python` shim as the agent (the smoke cell scored 0 with exit 127 although the agent's own
run of the same command had passed). 8 tests in `bench/test_lrab_ext.py`, including drift
between generator and checked-in fixtures.

Baseline, qwen3.5:9b, one cell each, 20-minute budget (total 35 min):

| task | 16k | 32k | 64k |
|---|---|---|---|
| CP-01 log triage | 1.00, peak 10.5k, L1 ×1, 21 calls | 1.00, 10.8k, 25 | 1.00, 7.8k, 20 |
| CP-02 verbose tests | 1.00, 9.6k, L1 ×1, 37 calls | 1.00, 13.0k, 39 | 1.00, 13.4k, 44 |
| CP-03 deep edit | 1.00, 6.2k, 18 calls | 1.00, 7.6k, 23 | 0.43, 7.8k, 22 |
| CP-04 many files | 1.00, 6.6k, 16 calls | 1.00, 6.6k, 14 | 1.00, 6.8k, 16 |

Reading: the pressure is latent with this model. qwen3.5:9b does what the prompts say (grep the
log, read one region of the module, look at the FAILED line), so peak prompt usage stays at
7–13k whatever the window, compaction fires only at 16k (L1 twice, never L2/L3), and the score
is flat across windows. L1 did its job where it fired: CP-01 went from 63% to 35% in one step
and finished at 57%. The one miss (CP-03 at 64k) is placement, not context: `fix_note.md` was
written inside `cp03_billing/` instead of the task directory. The `integrity` charge on CP-04
at 64k is a false positive of the lexical check: the last command before the accepted finish
was a verification `grep` that exited 1 for no match.

The 8k column is where the ladder is really exercised, and it shows what compaction costs:

| task | 8k | vs 16k |
|---|---|---|
| CP-01 log triage | 0.86, peak 7.6k, L1 L2 L3, 20 calls, 351 s | 1.00, 179 s |
| CP-02 verbose tests | **timeout** at 20 min, 25 compactions (L2 ×2), 110 calls | 1.00, 230 s |
| CP-03 deep edit | 1.00, 7.5k, 9 compactions up to L3, 35 calls, 501 s | 1.00, 123 s |
| CP-04 many files | 1.00, 6.1k, L1 ×2 L2 ×1, 19 calls, 202 s | 1.00, 117 s |

Two failure shapes, both information loss by the ladder rather than model error. CP-01: the
`grep FATAL` output (call 4) was evicted by L3 (17 → 5 messages) one call before the summary was
written, so the "Fatal events" section has the three codes from the model's own prose but no
timestamps or request ids, which is exactly the check it failed. CP-02: all three bugs were
fixed and the suite green by +252 s; the remaining 15 minutes were 73 `run_bash` calls re-running
tests and re-reading files after each L2 wiped the history to 7 messages (37 → 7, 44 → 7), the
model no longer knowing it had finished; the one finish at +1131 s was rejected because
`fix_report.md` was never written. Wall time doubles to quadruples at 8k on the tasks that did
finish. A side finding from the same cell turned out to be half the cause: `edit_file` on
`textkit/tokens.py` was refused by the safety gate because `SENSITIVE_PATTERNS` contained
`\token`, which matched any path segment starting with "token"; the model wrote the file with
`cat >` in `run_bash` instead (bead boubou-a09, fixed the same day: credential-shaped names
only). Rerun with the fix, same window, pruning off: 1.00 in 245 s, 36 calls, 6 compactions
(L2 twice) and the model still finished. So at 8k the ladder costs time and precision (CP-01's
request ids) but the timeout was the gate bug compounded by compaction, not compaction alone.

Consequence for boubou-65d.6: the 8k column is its baseline (score, wall, calls, compactions,
peak), with 16k as the mild case. Recency pruning should keep the latest tool results verbatim
(the FATAL grep in CP-01) and summarise evicted pairs as "done: X" facts (the green test run in
CP-02) so the loop neither loses the data it is about to use nor forgets that it has finished.
At 32k and above there is nothing to improve on this tier with this model; a less disciplined
model (gemma4:e2b reading files whole) is the second arm worth running.

## Hidden scorer tests (2026-10-29, bead boubou-65d.10)

On WF-04 and LH-03 the finish gate runs the very tests the scorer runs (`python -m pytest
test_inventory.py`, `python test_suite.py`), so the gate's measured value is inflated: the loop
iterates until the visible test is green and the scorer then measures exactly that. LH-03's suite
checks one input per function, so `return 1.0` passes `calc_fit` and `sum(values) * 0.75`
passes `calc_integrate`; WF-04's six cases have no edge cases. `bench/lrab_ext/hidden/<task_id>/`
is an overlay the driver copies into the workdir **after** the agent has finished (both arms) and
scores with the same scorer: a self-contained runner over inputs the agent never saw
(`test_inventory_hidden.py`, 8 cases; `test_suite_hidden.py`, 20 cases on two further inputs)
and `check_spec.py`, which prints `SPEC OK` when the visible spec file is unchanged (EOL-normalised
hash, as Mingbird's own seed check; the hash is the only thing read from Mingbird's tree).
`score.json` gains `hidden_score`, `hidden_details` and `total_hidden` (the task's total with the
hidden checks appended to the final group under the scorer's weights); the visible `total` is left
alone so old results stay comparable, the summary prints `h=` beside it and a per-arm "visible →
hidden" mean. `--hidden ""` disables the overlay. `bench/test_hidden.py` proves the point with a
reference fix that passes both suites and an overfit set (four hard-coded returns) that passes the
visible suite and fails the hidden one. Generated by `gen_hidden.py`; the visible tests stay in
the tasks as the agent's smoke tests, Mingbird's tree untouched.

First cells, qwen3.5:9b, boubou arm (`bench/results/lrab_hidden_smoke`). WF-04: the fix passed
all 8 hidden cases (visible 0.57 only because that run lacked pytest for the visible check; the
hidden runner needs none). LH-03: visible 1.00, hidden 17/20, `total_hidden` 0.97, and the three
misses are the overfit the overlay exists to catch, written as fixes with confident comments:
`calc_integrate` became `total * (len - 1) / len` ("proper scaling"; 7.5 on the visible input,
49.6 instead of 45.0 on the hidden one) and `calc_fit` became `sum(values) / factor - len(values)`
("proper intercept formula"; 1.0 on the visible input, 1.2 and 12.0 instead of −2.0 and 5.0).
Both passed the finish gate's test run and Mingbird's scorer. The gate measured "tests green",
not "bug fixed"; the overlay measures the difference.

## Serving sweeps as driver axes (2026-10-29, bead boubou-65d.4)

`lrab_h2h.py` takes `--model`, `--ctx` and `--think` as comma lists and runs the full product
over tasks, arms and replications. Every cell records `score.json["axes"]` (`model`, `ctx`,
`think` = on / off / default), the run id carries one suffix per swept axis (`_mqwen3.5-9b`
only when several models are listed, `_c16k`, `_toff`), and the summary labels rows by the
axes that vary in the results dir (`WF-01@qwen3.5:9b think=off`) and prints the constant ones
once. Both arms get the axes through `AGENT_CTX` / `AGENT_THINK`, the variables Mingbird's agent
already reads, so a think sweep is a fair pair. `probes.py` takes `--ctx` and applies
`--think` to the agent probes too, recorded under `axes` in each probe summary. Older
score.json files without `axes` fall back to their top-level `model` / `ctx`. Rationale
(lrab-extension-recommendations §5): thinking alone swung opencode's telecom score 0.30 → 0.99,
so a sweep has to be recorded data, not a note in a chat.


## Idle-streak bound (2026-10-29, bead boubou-9c5)

Mechanism, shared. The repeat guards (same tool 5×, same arguments 4×, same web query 6×) never
fire on a turn whose every call is different but none of them changes anything: acp_1029_104013
turn 2 made 31 reads, listings and searches before answering a question; acp_1029_183843 turn 2
("I think we already did that one. It can be closed.") did the asked-for `bd done` in three calls
and then 44 more reads, `find`s and listings around two empty spec directories until cancelled.

`guards/idle.rs`: an idle call is `read_file`, `list_dir`, `search_files`, the web tools, `todo`,
any non-producing MCP tool, or `run_bash` with a read-only command; a file write, edit or delete,
any other command, or `finish` resets the streak. Question route: nudge at 8, and from 12 every
further non-finish call is refused (two refusals, then the turn ends as a text answer does).
Task route: a nudge at 12 and at 24, nothing refused, so a long read of a codebase before the
first edit still runs. Two consecutive `web_search` no-result outputs draw one note. The log
line `[i] ⚠️ idle streak: N read-only calls …` is counted by `process_metrics.py` (`idle_notes`,
`idle_streak_max`; `idle` column in the h2h summary). `is_read_only_command` now also accepts
`2>/dev/null` / `2>&1`, `xargs <read-only>`, `bd show|list|ready|…` and `git status|log|diff|…`,
and rejects `find … -delete|-exec`; the integrity gate's "mutates" verdict uses the same rule.

Measurement, one controlled replay of today's two turns (`scratchpad/acp_replay.py`, a copy of
the overshare checkout with its own beads database, qwen3.5:9b, decider off, new binary):

| turn | live (old binary) | replay (new binary) |
|---|---|---|
| "What is the next item for us to work on ?" | 3 calls, answered | 12 calls, 97 s, answered (Q&A force) |
| "I think we already did that one. It can be closed." | 3 calls did the close, then 44 reads; cancelled at 6 min | never closed it; implemented task 1.2 instead (4 create, 4 edit, 1 delete, 32 commands); 57 calls at the 10-min client timeout |

Verdict: the bound works as specified (five loop tests) and fired once in the replay, but it is
not the lever for this turn. The model was not idle, it was producing: a one-action follow-up ran
as a full task because the loop treats `Intent::FollowUp` exactly like `Task` and task mode's
notes after an empty or text turn push it on. That is bead boubou-he2. Two more findings from
the replay: the question route let `cat > file` through (`run_bash` is allowed there and only the
file tools are blocked; bead boubou-ldj), and `DEFAULT_NUM_CTX` 131072 means Zed loads qwen3.5:9b
at 17 GB (bead boubou-0lg).


## Follow-up turns (2026-10-29, bead boubou-he2, open)

Mechanism. `Intent::FollowUp` existed in the heuristic and the decider but the loop ran it
exactly as `Task`: finish gates, continue notes, "call a tool and do real work now" after an
empty turn. For a one-action steer in an editor session ("I think we already did that one. It
can be closed.") those notes push the model past the instruction into the next piece of work.

Changes: (1) `guards/qa.rs`: after progress, a cue-free message up to `FOLLOWUP_MAX_CHARS` (80)
is a follow-up instead of 40 (the live sentence is 51 chars). Labelled set, heuristic only:
3-class accuracy 71.6% → 72.2%, follow-ups right 41 → 43, tasks unchanged (113 right, 0 called
follow-up). (2) `agent/run.rs`: on a follow-up, once a non-idle call has run (`acted_this_turn`),
a text-only reply ends the turn as a question's answer does, and the first empty reply draws
"If the user's instruction is done, say so in one line and stop" instead of the empty-turn
ladder. The task prompt, tools and gates are otherwise unchanged. Four loop tests, one guard
test.

Measurement, one controlled replay (copy of the overshare checkout with its own beads
database, qwen3.5:9b, decider off):

| turn | live, old binary | replay, new binary |
|---|---|---|
| "What is the next item for us to work on ?" | 3 calls, answered | 3 calls, 49 s, answered |
| "I think we already did that one. It can be closed." | `bd done` in 3 calls, then 44 reads; cancelled at 6 min | 42 calls at the 10-min client timeout, zero text; did not close the bead |

What happened in the replay: the model did not know the `bd` verb (`bd set done`, `bd sql`,
`bd dolt commit`, all failed), concluded the item was not done, wrote itself a six-step plan and
implemented mDNS discovery (five edits, one new file) in the copy. The follow-up route was
active: the "say so in one line" note appeared after its one empty reply. The ending rule never
fired because the model never produced text; it went tool → tool → tool for 42 calls, and the
idle nudge and the plan-progress reminder both fired on the way. So the rule is necessary and
not sufficient for this model. The bead stays open with the obvious next step for the user to
decide: a follow-up call budget (a bound on calls after the first action, then a forced report,
the question route's cap shape), and silencing the plan-progress reminder on follow-ups.

## Recency pruning before the ladder (2026-10-29, bead boubou-65d.6)

Mingbird's compaction ladder is all-or-nothing on the old history: L1 clips every old tool
output to 120 chars, L2/L3 replace everything but the tail with one model-written summary of
at most 150 words. The 8k column of the context-pressure tier showed the two ways that loses
work (above): the data the model is about to use is evicted (CP-01), and the knowledge that a
step is already done is evicted with it (CP-02). `history::prune_history` runs first, from half
the window (`PRUNE_AT` 0.50, on the pre-call estimate), deterministic and without a model
call: the last `PRUNE_KEEP` = 5 tool results stay verbatim; every older result becomes one fact
line, `[earlier] run_bash(python run_tests.py) → [exit 0] … 60 passed, 0 failed` (tool, target
argument, first line of the result and, when different, its last line), and the assistant turn
that made the call loses its thinking and any string argument over 160 chars (a `create_file`
body, an `edit_file` old/new pair). The history keeps its shape, so every call still has its
result and the request stays valid; only the bulk goes. Idempotent, so it runs on every
pre-call check above the threshold at no cost. The ladder is unchanged and still applies above
it on whatever the estimate says after pruning. `AGENT_PRUNE_KEEP` overrides the count, 0
disables pruning (the bench's A/B). Logged as `[context pruning: N older tool results → fact
lines, ~a → ~b tokens]`, counted by `process_metrics.py` as `prunes` / `pruned_results`.
Tests: `tests/history.rs` (keep/collapse, idempotence, argument shrinking, parallel-dispatch
names) and `tests/agent_loop.rs` (fires from 50% without the ladder; `prune_keep = 0` is a
no-op). Not in Mingbird; the design follows "Less Context, Better Agents" (recency pruning of
tool pairs) and the tier's own failure shapes.

Measurement, qwen3.5:9b, one cell each, pruning on (`AGENT_PRUNE_KEEP=5`) against the
baseline above (`bench/results/lrab_ctx_prune` vs `lrab_ctx`):

| cell | baseline: score, wall, calls, compactions | pruning: score, wall, calls, compactions, results pruned |
|---|---|---|
| CP-01 @8k | 0.86, 351 s, 20, 3 | 0.86, 214 s, 19, 2, 11 |
| CP-02 @8k | timeout, 110 calls, 25 | timeout, 109 calls, 16, 94 (both confounded by boubou-a09, rerun below) |
| CP-03 @8k | 1.00, 501 s, 35, 9 | 1.00, 203 s, 24, 2, 18 |
| CP-04 @8k | 1.00, 202 s, 19, 3 | 1.00, 203 s, 23, 3, 17 |
| CP-01 @16k | 1.00, 179 s, 21, 1 | 1.00, 251 s, 30, 2, 0 |
| CP-02 @16k | 1.00, 230 s, 37, 1 | 1.00, 355 s, 51, 0, 42 |
| CP-03 @16k | 1.00, 123 s, 18, 0 | 0.43 (fix_note.md misplaced, as the baseline's 64k cell), 243 s, 27, 0, 15 |
| CP-04 @16k | 1.00, 117 s, 16, 0 | 1.00, 100 s, 14, 0, 0 |
| CP-02 @8k, gate fixed (boubou-a09) | 1.00, 245 s, 36, 6 | 0.81 (`fix_report.md` names one of three functions), 740 s, 68, 11, 54 |

Reading. Mechanically it does what it says: at 16k a pass reclaims 1.7–3.9k tokens
(`~8321 → ~4372`) and CP-02 ran with no ladder compaction at all. At 8k it reclaims almost
nothing: 2.3k tokens of system prompt and tool schemas (28% of the window) plus the five
verbatim results are the bulk, most passes free under 300 tokens, and L1 at 60% then clips the
kept results to 120 chars anyway. Outcome: no score moved (CP-03 @16k is the placement miss
seen without pruning too), compactions fell at 8k, wall time fell on two 8k cells and rose on
three 16k cells where call counts also rose (30 vs 21, 51 vs 37, 27 vs 18). At n=1 that spread
is within the model's own variance (the baseline's CP-02 ran 37, 39 and 44 calls at three
windows), so nothing can be claimed either way; the gate-fixed CP-02 pair at 8k leans against
pruning (the pruned arm lost track of where the task directory was, tried `../../fix_report.md`
and absolute paths, and took three times as long). Verdict, as with the decider: the mechanism
stays in, **off by default** (`prune_keep` 0), `AGENT_PRUNE_KEEP=5` turns it on; a `--reps 3`
run of the 16k column is the measurement that would change the default. If the 8k case is
to be won, the fixed overhead and the verbatim tail are the levers, not the old results.

## Loop behaviour beyond Mingbird (2026-10-28, from the LRAB WF-08 post-mortem)

Two additions in `agent/run.rs` that Mingbird does not have; both came out of Boubou's WF-08
run (37 consecutive `create_file` calls without `path`), and both are covered by scripted-loop
tests in `tests/agent_loop.rs`:

- **Missing-argument ladder.** gemma-class models drop a required argument about one call in
  eight and then copy their own failed call from the history. From the second consecutive
  missing-argument error for the same tool, the loop removes the earlier failed pairs (the
  one-call assistant turn, its `is missing argument` tool error and the loop's own note) so one failed
  example is all the model sees, and prescribes a different call shape (short `create_file`
  with the key first, then `append_file`), with the plan's still-missing files as a hint.
  Four notes per run at most; a complete call resets the streak. The generic failure ladder
  (the generic "check the real state with list_dir / read_file first" hint) no longer fires for this class.
- **Truncated tool call.** When `done_reason=length` and a built-in call lacks required
  arguments, Ollama has returned the leading arguments of a call cut off by `num_predict`
  (`content` before `path`). The call is not executed; the tool error names the cap and the
  missing keys and the chunking advice is injected (three times at most) instead of the
  generic "retry with the same content". A truncated response whose call is complete runs
  normally.

### Text-only turns (2026-10-28, from a `boubou chat` transcript with gemma4)

A plain question ("Are you capable of software development ?", 41 characters, no question
word) was classified as a task, so every text reply drew the generic continue note; the model
answered with three alternating sentences and the repeat detector, which compared only with
the previous turn, never fired. Four changes, all tested in `tests/agent_loop.rs` and
`tests/guards.rs`:

- `qa::ends_with_question`: a trailing `?` (after trailing quotes/brackets) is a Q&A cue when
  no task verb matched; "are you / can you / do you / will you / would you / is there" joined
  the question openers. A `?` in the middle still does not count.
- The repeat detector compares a new text with the last four text outputs (`TEXT_WINDOW`),
  so A/B/C cycles close the turn as a dead loop.
- Task-mode text-only turns are capped: the second draws a sharper "tool call or finish, not
  more text" note (`TALK_SHARP_NOTE`), the fourth ends the turn with the earlier text as
  the answer (`TALK_FORCE_END`). A tool call resets the streak. Mingbird has no such cap.
- `RunParams::chat_mode` (set by `boubou chat`): a text reply ends the turn even when the
  message reads like a task. The web page keeps the detector-only behaviour because it has
  no separate chat/task entry point. The CLI also prints thinking dimmed (ANSI) so it reads
  apart from the answer.

## Language (2026-10-28)

Boubou is UK English from top to bottom; nothing of Mingbird's Chinese text survives in the
source, prompts, tool and harness messages, web page, skills, docs or tests. Decisions:

- Prompts, harness notes, tool errors and log lines are Boubou's own English. The functional
  markers other code matches on moved with them: `[safety gate`, `[safety net`,
  `[overwrite guard`, `[child safety gate` (`FAIL_MARKERS`), `is missing argument '<key>'`
  (`missing_arg_key`), `⚠️ [missing argument]`, `[Earlier context summary]`
  (`qa::CONTEXT_SUMMARY_MARK`), `[blocked:`, `Hint:`, `Your answer is already sufficient`,
  `Continue the previous conversation` (`HARNESS_NOTE_PREFIXES`), `outside the working
  directory` / `out of bounds` (the path-refusal hint), `timed out` (MCP).
- Tool categories are `files`, `code`, `web`, `memory`, `mcp`. `mcp::normalise_category`
  accepts the English names case-insensitively and Mingbird's four Chinese tags (kept as
  `\u{..}` escapes in the source, never as characters), so an existing `mcp.json` and a
  manifest cache written by Mingbird still route.
- Deliberate domain bias (post-v0.2.0): skills accept `triggers:` / `autoload:` frontmatter and
  matching skills are prefilled into the task prompt (`SkillSet::for_task`, cap 3, logged as
  `[skills in effect: …]`); `<workdir>/SYSTEM.md` is appended to the task and chat prompts, or
  replaces them when it starts with `<!-- boubou: replace -->` (`prompt::project_system`). The
  explicit override file still wins the base. Resumed sessions keep their original prompt.
- Routing (`face::route_categories`) is domain-neutral since v0.2.0+: the code keyword list is
  programming vocabulary only (no `fix`/`test`/`error`/`calculate`/`run`), and the built-in
  `files` and `code` tools are offered on every task; routing only adds `web`/`memory`/`mcp`
  built-ins and decides which MCP servers' tools are exposed. The hollow-finish gate asks for
  "a deliverable" (file or answer text), not for a file.
- Routing keywords (`face::CAT_KEYWORDS`), Q&A hints (`guards/qa.rs`), parallel veto/simple
  patterns (`parallel/config.rs`) and complexity words are English-only lists matched on the
  lower-cased text. A bare `?` is not a Q&A cue (Mingbird's `?` entry never matched either:
  it went through the word-boundary regex).
- The time-budget parser understands digits and English number words with `minute(s)/min(s)`,
  `hour(s)/hr(s)/h`, `half an hour`, `an hour`, `an hour and a half`, `N and a half hours`;
  Chinese numerals, measure-word forms and full-width digits are gone.
- The web page is English-only (`<html lang="en">`, no zh table, no language toggle).
  `AGENT_LANG` / `lang_en` are gone. The CLI, Tauri shell and `Info.plist` are English only.
- Speech-to-text defaults to `en` (`stt_lang`, `--lang`, `/api/voice?lang=`); the Chinese
  zipformer remains a supported model and is tried as a fallback when the English one is
  not installed. The Ollama transcription prompt is English.
- Compaction summaries are requested in English (`history.rs`).
- Spelling: UK in prose, strings, comments and docs (behaviour, artefact, organise,
  normalise, cancelled); identifiers and external API names are unchanged (`artifacts`
  module, serde `Serialize`, `Flavor`). Classifier lists carry both spellings where users
  may type either (`summarise`/`summarize`).

## Step 8 brief (handoff notes, 2026-10-28)

- Voice model installer: `boubou voice --install <lang>` downloading the k2-fsa `tar.bz2`,
  keeping only `*int8.onnx` + `tokens.txt` under `~/.ollama_agent/stt/`; today the hint text
  only tells the user where to put the files.
- Tauri bundling: wire `crates/boubou-app/Info.plist` into `tauri.conf.json`, bundle `stt/` as a
  resource, build `boubou-app` with the `voice` feature, test the mic permission prompt.
- Live-check an OpenAI-compatible engine (LM Studio / llama-server) and a real cloud provider;
  only loopback capture tests cover `OpenAiBackend` so far.
- Windows: `_ollama_app_models_dir` (sqlite lookup of the Ollama app's model dir) is not ported.
- `doc_digest` / `file_organizer` skills still mention `run_python`.
- LRAB head-to-head: `bench/lrab_h2h.py` (Boubou side; Mingbird is third-party and is never
  modified). It runs Mingbird's own `run_bench.py --agent mingbird` for one arm, drives `boubou run`
  itself for the other with the same isolated env (`~/.mingbird_bench` home, `AGENT_CTX=32768`,
  `AGENT_SLIM_OUTPUT=0`, `AGENT_UNATTENDED=1`, no `--new` so kill-resume resumes `.agent_state.json`)
  and scores both with Mingbird's `score_task` imported read-only. Needs `requests` +
  `beautifulsoup4` for the mingbird arm: `uv run --with requests --with beautifulsoup4 python
  bench/lrab_h2h.py --model gemma4:e2b`. WF-01 smoke on gemma4:e2b: Boubou 1.0 in 131 s.
  Measurement additions (2026-10-28, epic boubou-65d): `--reps N` runs every cell N times with
  the (task, arm) order round-robin and the arm order alternating per task, so machine drift
  cannot follow one arm; the summary then shows per-task mean and spread plus a per-arm bootstrap
  95% CI over tasks. Every cell also carries `score.json["process"]` from `bench/process_metrics.py`
  (one parser for both arms, since Boubou mirrors Mingbird's transcript lines): tool calls, peak
  prompt tokens, failed calls, non-zero exits, loop interceptions, empty resets, finish refusals,
  pre-delivery checks, truncations, compactions, and an *integrity* count = files named in the
  task prompt that are missing after an accepted finish + a finish accepted after the last command
  failed. `python bench/process_metrics.py <results_dir>` backfills older result sets. Tests:
  `uv run --with pytest --python 3.13 python -m pytest bench/ -q`.
  Micro-probes (`bench/probes.py`, Boubou only, one mechanism per probe, n trials each):
  `emission` sends Boubou's own schemas (`boubou tools`) straight to Ollama `/api/chat` on twelve
  one-tool prompts and reports the schema-valid rate per model + quant (the model-side floor;
  a parser or model problem must never be charged to the harness); `false_finish` is a task whose
  done-state is only true once `python check.py` prints OK, scored by our own run of check.py
  after the agent exits (false-success rate = finish accepted while the check fails);
  `loop` runs a `fetch.py` that always fails and measures identical attempts before a guard
  fires, clean ending, and whether summary.md is honest or fabricates a row count. Results in
  `bench/results/probes/<probe>/<model>/`, `--summary` prints the table.
  First probe finding (2026-10-28, bead boubou-65d.11): the Ring-1 `DANGER_CMD` substring test
  refused a `python -c` CSV script eleven times because a comment read `# Format the result`.
  Departure from Mingbird: the entries that are also ordinary words (`format `, `halt`,
  `shutdown`, `shred `, `srm `, `dism `, `poweroff`, listed in `DANGER_CMD_TOKEN_ONLY`) now count
  only where a command starts (string start or after `;` `&` `|` newline `(` backtick, optionally
  behind `sudo`); the rest of the list (`rm -rf`, `of=/dev/`, `curl | bash`, …) stays a substring
  match so a dangerous op hidden inside a script is still caught. `safety::danger_hit`.

Search backends: Boubou defaults to Brave → DuckDuckGo HTML → Bing → Baidu. Changed on
2026-10-28 from Bing-first after Bing answered the query `jevai.net` with ten well-formed but
unrelated results (Microsoft Forms, Gmail) and the model reported that as fact; DuckDuckGo, the
fallback, was silently answering with a bot-challenge page (HTTP 202) that parsed as "no
results". Four guards now sit on the scraping, all in `web.rs` and covered by loopback tests in
`tests/web_memory.rs`:

- Relevance floor: a result whose title, URL and snippet share no word with the query (stop
  words and `www/com/net` removed; five-letter prefixes count; Chinese tokens match as substrings)
  is dropped. A backend whose results all fail the floor is skipped and not cached, and the
  loop moves to the next backend.
- Bot challenge: DuckDuckGo's `anomaly-modal` page is reported in the tool output ("served a
  bot challenge instead of results") and in the log instead of counting as empty.
- Site-shaped query (`jevai.net`, `typesafe.ai/blog`): the site itself is fetched first and
  leads the output as "(fetched directly)", whatever the engines return.
- When every backend fails the floor the tool returns `[web_search: no relevant results for
  "…" (per-backend reasons). Rephrase the query …]`; transport failures keep the
  `[web_search error: …]` form.

Mingbird's `cn.bing.com` host is therefore not shared in the search cache; `web_fetch` entries
still are. `AGENT_SEARCH_BACKENDS` overrides the list. Backlog (beads): a SearXNG backend
(`format=json`, no HTML scraping, self-hosted) and a Tavily backend (hosted API, key in config).

## Kernel boundary inside the loop (refactor step 8, 2026-10-30)

The loop's stateful compensation mechanisms moved out of `agent/run.rs` into
`agent/ladders.rs` as structs the loop feeds an observation and gets a verdict from, with
no policy reads: `TextLadder` (empty replies, repeats, the talk streak), `CallLadders`
(disabled-tool recalls, duplicates, the failure and missing-argument ladders, tool and
same-call streaks, idle, research, the plan-tool loop), `Compaction` (the three-level
context ladder), `RetryLadder` (engine errors: repair, restart, compact, back off, give up)
and `CorrectorLedger` (failures per signature, consultations, blocks, stale lifting).
Format-leak salvage is composed in `guards/leak.rs::salvage`. The loop keeps every message
text, tool-face change, context reset and the finish gates. Measured: `run.rs` 2118 → 1969
lines, `ladders.rs` 975 lines (with 8 unit tests), mutable locals in `Agent::run` 55 → 38.
Behaviour unchanged apart from two log lines (the duplicate-call refusal no longer prints
its count; a sixth structural-400 repair now also logs the 4xx stop line). 479 tests.

## Sessions record their policy (refactor step 9, 2026-10-30)

`save_session` writes `policy` (the turn policy's name) into `<name>.meta.json` next to
Mingbird's `updated`, `task`, `status`, `msgs` and `workdir`. Mingbird merges into that file
rather than rewriting it, so the key survives a Mingbird turn on the same session; a session
Mingbird saved has no key and resumes without comment. At the start of a run the loop reads
the recorded name and, when it differs from the policy about to run, logs `[policy] session X
ran under A; this turn runs under B` before the usual policy line; the products keep choosing
the policy (web chat: the batch toggle; `boubou run`/`chat`; the editor: `coding`), the
kernel only says when a resume changes harness. The web UI's session list carries the name
as `policy`. Tests: `session::a_session_records_the_policy_it_ran_under`,
`agent_loop::a_session_resumed_under_another_policy_says_so`.

## Products own their prompts and skills (refactor step 11, 2026-10-30)

Each product crate has a `harness` module: its preset, its task prompt and its skills.
`boubou-general::harness::policy()` is `general`, `boubou-coding::harness::policy()` is
`coding`; both are `TurnPolicy { name, prompt, ..TurnPolicy::conversation() }`, the kernel's
new name for the conversational semantics the two used to share as kernel presets. The task
prompt travels in the policy (`Prompt::Task(base)`): the kernel's own prefill is
`prompt::TASK_BASE`, and each product carries a copy it owns; the copies equal the kernel's
text today, pinned by a test in each product crate until step 10 changes one on evidence.
The built-in skills left the repository root for `crates/boubou-general/skills/` (doc_digest,
file_organizer, markdown_report, web_research) and `crates/boubou-coding/skills/` (the ten
about code) and are compiled in (`Skill::embedded`, `SkillSet::with_builtin`,
`Agent::with_builtin_skills`), below every directory so a user's file of the same name wins.
Before this the built-in skills were found only in a `skills/` directory beside the binary,
which a cargo build never has, so a plain `target/release/boubou` listed none; now an editor
session sees the coding skills, the web UI and `boubou chat` the general-purpose ones, and
`boubou run`, `boubou skills` and `boubou doctor` both sets. The chat REPL moved to
`boubou-general::chat`; `boubou-cli` keeps the wiring (engine, event printer, Ctrl-C) and
`run`. `skills/README.md` is now `docs/skills.md`. 489 tests.

## Measurement rig for the product presets (refactor step 10, 2026-10-30)

`boubou run --policy unattended|general|coding` runs a task file under a product's preset and
skills (the CLI, the one crate that sees both products, maps the name to `harness::policy()`
and `harness::skills()`); the workdir layout and checkpoint stay the CLI's. `lrab_h2h.py`
gains `--policy` as a fourth sweep axis beside model, ctx and think: recorded in
`score.json["axes"]`, `_p<name>` in the run id, `task@coding` rows in the summary, boubou arm
only. Under a product preset a text reply ends the run, so for those cells the driver writes
the final reply (the last assistant text, else the `finish` summary, from `.agent_state.json`)
to `reply.md` before scoring. `bench/lrab_ext/gen_surface.py` adds two tiers of briefs with no
batch preamble: `tier6_editor` ED-01..05 (fix a failing test; fix plus a named note; add a
function with tests; rename across files; explain without changing) and `tier7_chat` CH-01..05
(answer from files; summarise to a file; tidy a folder; compute from a CSV; small talk that
must touch nothing). Fixtures land at the workdir root; referee scripts (`check_collateral.py`,
`check_title_case.py`, `check_tidy.py`, `check_untouched.py`) give the scorer's `script_pass`
its substrings. Tests: `bench/test_surface.py` (generator determinism, each editor task fails
then passes, the referees, the driver's axis, `final_reply`, a scorer round trip on ED-02 and
CH-01). No model has run the tiers yet; the first measurement is the coding preset with and
without the named-deliverable gate on `tier6_editor`.

## Survey of pi and opencode (2026-10-30)

Read `~/IdeaProjects/pi` and `~/IdeaProjects/opencode` (V2) to see what production harnesses
for frontier models do that Boubou does not. Both keep the loop tiny and put the rigour in the
tools and in what tools say back: edit matching in gated tiers (exact, Unicode-normalised,
trailing whitespace, typographic) with a counted error; compaction as a validated section
template plus a verbatim recent tail; tool calls cut off by the output limit refused rather than
executed; tool output capped at about 2000 lines or 50 KB with a cut marker and a spill file;
short assembled prompts with per-tool rules, an environment block and `AGENTS.md`; per-step
content-addressed snapshots with revert. Neither adapts anything to the model beyond opencode's
prompt swap by vendor name, neither sets `num_ctx` for Ollama, and neither has a task-success
benchmark. Filed as bd epic `boubou-bk9` with six children; post-edit diagnostics stays in
`boubou-po1`, the ACP gaps stay in `boubou-m64`. Not copied: pi's absence of a permission
system, fixed 16k compaction reserves, subagents and plan mode in the kernel, and
thousand-word per-model prompts.

## Surface tiers: no credit for an untouched fixture (2026-10-30)

The first `tier6_editor` run (`bench/results/surface_editor`, qwen3.5:9b, coding vs unattended)
measured nothing: the release binary predated `--policy`, every cell exited in half a second
with "unexpected argument '--policy'", and still scored 0.29 to 0.57. The milestone of four
tasks was the collateral checker, which passes on a tree nobody touched, and ED-03 and ED-04 also
had test runs that were green before any work. Fixed in the generators, not the scorer: the
shared checker template takes a `require` block (a listed definition must differ from the
shipped one, a listed file must exist, a listed test command must print its pass line) and
prints `COLLATERAL PENDING` or `COLLATERAL TESTS FAIL` until it holds; ED-03's and ED-04's test
runs moved inside the checker; ED-05's and CH-05's untouched checks need `reply.md` first. The
driver now writes `reply.md` for every boubou cell (unattended's `finish` summary is its reply)
and never overwrites one the agent wrote. `test_an_untouched_fixture_scores_nothing_on_every_surface_task`
holds all ten tasks at 0.0 against Mingbird's scorer; the pressure-tier checkers regenerate
byte-for-byte in behaviour (empty `require`). The run is to be repeated with the rebuilt binary.

## First step 10 measurement: coding vs unattended on tier6_editor (2026-10-30)

`bench/results/surface_editor`: qwen3.5:9b, ctx 32k, one replication, `boubou run --policy
coding` against `unattended`, the five editor briefs, scored with the gated checkers above.

| task | coding | unattended | coding calls/wall | unattended calls/wall/refusals |
|---|---|---|---|---|
| ED-01 fix failing test | 1.00 | 1.00 | 10 / 46s | 27 / 124s / 3 |
| ED-02 fix plus fix_note.md | 1.00 | 1.00 | 11 / 55s | 18 / 85s / 3 |
| ED-03 add function with tests | 0.71 | timeout (20 min, 89 calls, 9 compactions) | 30 / 300s | — |
| ED-04 rename across files | 1.00 | 1.00 | 15 / 46s | 55 / 186s / 4 |
| ED-05 explain, change nothing | 1.00 | 1.00 | 1 / 31s | 19 / 212s / 3 |

Mean 0.94 coding against 0.80 unattended (timeout as 0). Coding never lost a task, used a third
of the calls and a third of the wall time, and had no finish refusals; ED-05 was answered from a
single read_file with the explanation as the turn's text.

What the unattended refusals were: every one was the test guard. The fixtures' test files are
plain `test_` functions with their own `__main__` runner; the guard's `unittest discover`
fallback finds no TestCase, prints NO TESTS RAN, exits non-zero, and the gate reads that as a
failing suite while `python test_slug.py` is green. The model cannot satisfy the gate and is let
through after three rejections (bd `boubou-j5s`; unittest exits 5 on no tests under Python 3.15).
So this run does not yet show that dropping the test guard is right for `coding`; it shows the
guard misfires on this test shape, which costs the unattended preset 2 to 4x the time. The
measurement to repeat after the guard fix is the same command.

What coding's named-deliverable question looks like: ED-02's `fix_note.md` was written under
`coding` with no gate (`named_files_missing` empty), one data point for leaving the gate off.

ED-03 is the small-model editor failure both presets share: under `coding` the model replaced
`test_textfmt.py` wholesale with `create_file`, dropping the fixture's `_run` and `__main__`
block, so `python test_textfmt.py` prints nothing and the checker reports the suite did not run;
its own test expectations were also wrong ("The Cat And Dog"), though `title_case` itself passed
the referee. Under `unattended` it spent twenty minutes and nine compactions oscillating
between its implementation and a test expectation that contradicted the brief, with one
duplicate edit blocked. Post-edit feedback (boubou-po1 item 3) and the edit tiers (boubou-bk9.3)
are the mechanisms that address it; the named-file gate is not.

## Finish gate 2 replaced: the turn's own verification, not a guessed runner (2026-10-30)

The measurement above showed every unattended refusal was the ported test guard misreading
`unittest discover`'s "NO TESTS RAN" as a failing suite. A first fix added a third runner
(run self-running `test_*.py` as scripts); it was withdrawn before commit as one more layer on
the same mistake: the harness guessing a project's test command from the filesystem.

Design. The finish gate verifies what the turn itself verified. The loop records every shell
command and file write with its exit code, and the claims gate already reads that record to
refuse a summary that claims a result nothing backs or that contradicts a failed check, and to
let an honest failure report through. The one case the record cannot settle is a check that
passed before later file changes. For that case the gate re-executes the model's own command
through the same tool path (`ToolRunner::run_tool`, so the safety rings and output caps
apply) and takes the new exit code: a pass becomes a fresh record and the finish stands, a
failure is refused with the output, quoting the command. When the turn changed files in a
workdir that has tests and ran no check at all, the gate asks for one and names no runner. It
never chooses a test command, never falls back, and does nothing when the last check failed,
because honesty about failure is the claims gate's business. A stale check that also writes
(`pytest && git commit`, a redirect) is not re-executed; the claims gate's lexical stale rule
stands for it.

Consequences. The claims gate's stale rule ("passed but N actions that can change files ran
after it") now fires only for checks the harness must not re-run; for the rest the re-run
settles it, so a model that edits after a green run and finishes is accepted when the check
still passes instead of being told to run it again. `has_tests` recognises test files by name
in any language. Three refusals, then let through, as before. `agent/testguard.rs` is 130
lines where the runner chain was; `tests/testguard.rs` (6) and four loop tests in
`tests/agent_loop.rs` cover accept, refuse, not-rerunnable and no-check-ran, with a scripted
tool runner that can answer differently on the re-run. 500 Rust tests, clippy clean. The
`coding` and `general` presets have no gates and are unaffected; `unattended` and `child`
inherit it. tier6_editor unattended is to be run again; the before numbers are above.

## tier6_editor again, under the replaced gate (2026-10-30, run 2)

Same command, same model and context, binary at 7dd90bd. Run 1 is `surface_editor_run1_old_guard`.

| task | coding run 1 → 2 | unattended run 1 → 2 | unattended calls / wall / refusals, run 1 → 2 |
|---|---|---|---|
| ED-01 | 1.00 → 1.00 | 1.00 → 1.00 | 27 / 124s / 3 → 9 / 26s / 0 |
| ED-02 | 1.00 → 1.00 | 1.00 → 1.00 | 18 / 85s / 3 → 10 / 44s / 0 |
| ED-03 | 0.71 → 1.00 | timeout → 0.57 | 89 / 1200s / 1 → 9 / 40s / 0 |
| ED-04 | 1.00 → 1.00 | 1.00 → 1.00 | 55 / 186s / 4 → 12 / 31s / 0 |
| ED-05 | 1.00 → 1.00 | 1.00 → 1.00 | 19 / 212s / 3 → 3 / 46s / 0 |

Mean 1.00 coding, 0.91 unattended; whole run 10 minutes against 38. Finish refusals in the
unattended column: 14 → 0. The gate fired once in ten cells, on ED-02 unattended, where the
model edited rates.py after a green `python test_rates.py` and finished; the gate re-ran that
command, it passed, the finish stood ("test guard: re-ran `python test_rates.py` after later
changes: still passes"). No cell was asked for a check it had not run. Unattended is now
faster than coding on every task, with fewer calls, which is the batch prompt's plan-first
shape against the editor prompt's read-first one, not a gate effect.

ED-03 unattended's 0.57 is a model error the gates are right to let through: `title_case`
leaves a small word in the middle untouched instead of lowering it (`'ON THE ROAD'` →
`'On THE Road'`), its own two tests only use lower-case input and pass, the suite is green, and
the finish is honest. The referee's mixed-case case catches it. Nothing in the harness should;
it is the measurement doing its job. One replication each, so the coding/unattended
difference on ED-03 (1.00 against 0.57, 29 calls against 9) is sampling until repeated.

Step 10 reading: with the gate telling the truth, the two presets score the same on four
tasks and the remaining question for `coding` is no longer the test guard. The named-
deliverable gate has not been needed in either run (ED-02's `fix_note.md` written under
`coding` both times, `named_files_missing` empty everywhere).

## tier6_editor with gemma4:e2b (2026-10-30, a capability probe)

`bench/results/surface_editor/gemma_e2b`, same rig, 2B model: mean 0.72 (coding 0.70,
unattended 0.74) in 6 minutes, no crash, no timeout, against qwen3.5:9b's 0.96. The harness held:
the only gate firings were correct (ED-01 unattended: "files changed, no check ran" produced a
test run; ED-02 unattended: the re-run of the model's own `python test_rates.py` passed), the
duplicate-call ladder absorbed five `list_dir` repeats, and the plan-files gate refused twice
over a file the model's own plan misnamed (`slugify.py`). The failures are the model's:
wholesale `create_file` rewrites of files it should have edited (ED-03 both presets, ED-04
coding, where it also appended a second copy of `service.py` to itself), an invented `workdir/`
subdirectory it created both files in while running the untouched root tests green (ED-03
unattended, scored 0, finished "implemented"), and going silent two replies in a row under
`coding` with the rename two-thirds done (ED-04, 0.21; the same model finished the task under
`unattended` at 0.89). ED-05 coding scored 1.00 from zero tool calls with a generic paragraph
about "how duration parsers typically work": the only referee word it matched was `'m'`. That
is a bench defect, not a model result: ED-05's `contains_any` list dropped the bare
`'m'`/`"m"`/`` `m` `` entries the same day, so a guess cannot pass without naming the mechanism;
the four qwen3.5:9b replies still pass (`regex`, `suffix`, `fullmatch`, `pattern`), the two
gemma replies score 0 on that check, and `test_surface.py` holds the bluff out.

## tier7_chat: general vs unattended (2026-10-30)

`bench/results/surface_chat`, qwen3.5:9b, ctx 32k, one replication, `--policy
general,unattended`, the five chat briefs, binary at 7dd90bd. Every cell scored 1.00; the whole
run took 6 minutes.

| task | general calls / wall | unattended calls / wall | note |
|---|---|---|---|
| CH-01 owner and date from notes/ | 6 / 35s | 2 / 8s | unattended took the question route and answered in text |
| CH-02 brief.md from notes/ | 11 / 39s | 7 / 42s | |
| CH-03 tidy inbox/ | 16 / 45s | 27 / 109s, exit 1 | files all placed before the run died (below) |
| CH-04 Q2 travel from the CSV | 1 / 34s | 1 / 23s | one `run_bash` each |
| CH-05 small talk | 0 / 7s | 5 / 21s | unattended read four files, including the referee, before answering; nothing created |

Two presets, same scores, and on three of five tasks the batch harness was faster than the chat
preset, because its question route answers in text from one or two calls while the general
prompt plans first. The chat preset's wins are the shape of the turn: CH-05 answered from zero
calls, and nothing it did needed a gate. `reply.md` from the finish summary or last text held
the answer for every unattended cell, so the driver change (reply for every policy) did its job.

CH-03 unattended is a kernel bug, not a model one. The model called `read_file` on
`inbox/images/screenshot 2026-10-02.png`, a 0-byte fixture; Boubou attached it as multimodal
input, Ollama answered `HTTP 400: decode image: png: invalid format: invalid checksum`, and
the loop treated the 4xx as fatal ("retrying is pointless, stopped"), exit 1, after the tidy
was already complete. A server rejecting an attachment should drop the attachment and tell
the model the image could not be decoded, not end the run (bd boubou-am3). The two artefact-gate
refusals in that cell were over names the summary abbreviated (`2026-10-02.png` for
`screenshot 2026-10-02.png`), which is the gate's rule working on a sloppy summary.

Step 10 reading for `general`: nothing here says the chat preset needs a gate, and nothing says
the batch harness is worse at chat-shaped work once the question route fires. The difference
the products exist for is the turn shape and the prompt, not the protections.

## A rejected image no longer ends the run (2026-10-30)

The loop already repaired one 400 the harness itself caused, the structural "consecutive
assistant turns" error, by cleaning the history and retrying through the failure ladder as
`repaired`. A server that cannot decode an attached image (`HTTP 400: decode image: png:
invalid format`, tier7 CH-03 unattended, bd boubou-am3) is the same class, so the repair sits
at the same seam: `history::drop_images` strips every image from the history and appends
`IMAGE_DROPPED_NOTE` to the message that carried the last one, and the ladder retries once
(`Http4xx { repaired: true }`); the retry log line now names which repair was made. The model
sees why it got no picture and the task continues. No header check in `read_file`: the
server's decoder is the authority. Tests: `tests/history.rs` (drop everywhere, note once) and
`tests/agent_loop.rs` (a backend that refuses any request carrying an image is refused once,
the retry carries none, the run completes). 502 Rust tests, clippy clean.


## tier6_editor with gemma4:e2b, every ablation switch off (2026-10-30, an invalid arm)

The ablation meant to answer "does the kernel earn its place on the easy tiers": the gemma
probe above rerun with `AGENT_ABLATION=finish_gate,finish_gate_text_only,corrector,anti_loop,
flat_prefill`, same axes (32k, coding and unattended), results in
`bench/results/surface_editor/gemma_e2b_ablated`. Mean 0.45 against the probe's 0.72, but
the arm did not measure the kernel off, so the difference is not the kernel's contribution.

| task | probe (kernel on) coding / unattended | ablated coding / unattended |
|---|---|---|
| ED-01 | 1.00 / 1.00 | 1.00 / 0.00 |
| ED-02 | 1.00 / 1.00 | 0.00 / 0.43 |
| ED-03 | 0.29 / 0.00 | 0.29 / 0.29 |
| ED-04 | 0.21 / 0.89 | 0.21 / 0.32 |
| ED-05 | 1.00 / 0.79 | 1.00 / 1.00 |

Why it is invalid. In 7 of 10 ablated cells every `edit_file` call (18 of them) was refused
with "disabled (repeated-call protection)"; the probe had no `edit_file` call at all. The
small-model rule (`guards::is_small_model`, ≤4B) disables `edit_file` before the first step
and the category prefill (`Face::tools_for_categories`) hides disabled tools, so in the probe
gemma never saw it and wrote whole files. The flat prefill (`Face::active_defs`, the
`flat_prefill` path) does not drop `face.disabled`, so the ablated arm advertised a tool the
loop then refused, and the model spent its steps on it (ED-04 unattended: five refusals, then
`create_file` with a 115-byte repo.py, collateral destroyed). Two gaps, bead `boubou-8km`: the
flat path advertises what the face will refuse, and the small-model edit rule is a kernel
compensation with no switch, so "kernel off" is not reachable for the models the kernel is for.
`anti_loop` also only governs the streak notes; the duplicate-call blocker stayed on.

What the run does say. ED-01 unattended is the one clean comparison (no `edit_file` on either
side): with the gates on, `finish` after an untested rewrite was refused ("no check ran"), the
model ran the tests and scored 1.00; with the gates off the same model wrote `workdir/slug.py`
into an invented subdirectory after twelve read-only calls and finished at 0.00 on its own
word. One cell, n=1, suggestive only. The ablation stays open until the two gaps have a design.

## The ablation made real: one switch list, `kernel` = all (2026-10-30, bead boubou-8km)

Design. An ablation names one kernel compensation and runs the loop without it, so a bench
score with and without is that compensation's contribution. The kernel is what the loop does
around the model because small models stumble; it is not the loop's infrastructure (tool
dispatch, compaction and pruning, the iteration cap and the end-of-turn rules, the safety
rings) and not the product policy (`TurnPolicy`). Every compensation therefore carries a
switch, the switches are listed in one module with the mechanism each turns off, and `kernel`
is every switch at once: the only arm that measures the kernel, and it must be reachable for
the models the kernel exists for. A compensation without a switch is a measurement the
project cannot make. Switching one off leaves the model the raw loop, never a half state the
loop then punishes: a tool the face will refuse is never advertised, on any path.

What changed (`agent/ablation.rs`, the paragraph above is its module doc). Two switches
added: `small_model` (the ≤4B protocol: `edit_file` hidden from the first step, the shell
hint) and `salvage` (format-leak salvage, the profile's finish-prose rescue included).
`anti_loop` now covers every call-pattern ladder, not only the repeat-streak notes: the
duplicate-call block, idle nudges, the failure ladder (missing-argument repair, redirect
hints, the three-failure `edit_file` disable), the research and plan-tool streaks, the
shotgun-edit hint and the stale-plan reminder. `finish_gate`, `finish_gate_text_only`,
`verify_feedback`, `corrector` and `flat_prefill` are unchanged. The run logs
`[ablation] off: …` and names any word that is not a switch. `ToolFace::active_defs`, the
flat-prefill list, now drops disabled tools like the category list does; the refusal text no
longer claims "repeated-call protection" for a tool disabled for another reason. Tests:
`ablation.rs` (kernel is every switch, a typo turns nothing off), `face.rs` (a disabled tool
is advertised on no path), `tests/agent_loop.rs` (a duplicate call runs again under
`anti_loop`; a 2B model is refused `edit_file` until `small_model` or `kernel` is set, then
the edit runs). 507 Rust tests, clippy clean.

The ≤4B edit rule, and a correction. The rule is Mingbird's `_small_model` (`ollama_agent.py`
3710–3718): parameter count parsed from the model tag, at or below 4B `edit_file` goes into
`_disabled_tools` before the first step. Mingbird's only stated reason is the comment on the
separate three-failure disable ("edit_file is the tool small models most easily get stuck
on"); the pre-emptive version generalised that to "never offer it below 4B" with no
measurement attached, and the port carried it over with its own comment ("exact-match
protocol is the top failure mode") and no measurement either. The gemma probe entry above
blamed "wholesale `create_file` rewrites of files it should have edited" on the model. That
was wrong: the model was never offered `edit_file`; the kernel made it rewrite whole files,
and the collateral those rewrites destroyed (ED-03, ED-04 coding) is the rule's cost, not the
model's. Whether the rule has a benefit is now a measurable question, and the first answer is
the `kernel` arm recorded below.

## tier6_editor with gemma4:e2b, the kernel off (2026-10-30, the first valid ablation)

`AGENT_ABLATION=kernel`, same axes as the probe (32k, coding and unattended), results in
`bench/results/surface_editor/gemma_e2b_kernel_off`. Every cell's transcript carries
`[ablation] off:` with all eight switches, no refusal and no intervention was logged, and
`edit_file` was offered and used (28 calls across the run, 17 of them exact-match failures).
Mean 0.64 against 0.72 with the kernel on, 15 minutes against 6.

| task | kernel on, coding / unattended | kernel off, coding / unattended |
|---|---|---|
| ED-01 | 1.00 / 1.00 | 1.00 / 1.00 |
| ED-02 | 1.00 / 1.00 | 0.71 / 0.43 |
| ED-03 | 0.29 / 0.00 | 0.29 / 0.29 |
| ED-04 | 0.21 / 0.89 | 0.32 / 0.32 |
| ED-05 | 1.00 / 0.79 | 1.00 / 1.00 |
| mean | 0.72 | 0.64 |
| finish accepted after a failing last check | 0 of 10 | 6 of 10 (2 claimed success outright, 4 hedged it) |

Cell by cell, from the harness lines and the score details. ED-01: the raw model made the
one-line fix with `edit_file` in four or five calls and ran the tests itself, faster than
either kernel-on cell; the gates that fired there last time were not needed this time (n=1).
ED-02: coding landed one edit, ran the tests (2 passed, 4 failed), wrote the note and finished
"fixed"; unattended spent 458 seconds and 40 calls on ten `edit_file` attempts (seven
exact-match failures), seven failing test runs and six empty turns in a row, which the
hard reset (infrastructure, not a switch) ended, then finished "fixed". With the kernel on
both cells scored 1.00: the ≤4B rule made the model rewrite the file whole, and the test
re-run gate confirmed it. ED-03 unattended rose from 0.00 to 0.29 because the kernel-on
failure was the invented `workdir/` subdirectory, which did not recur; the kernel-off edits
hit the real file but changed `collapse_spaces` and two other untouched definitions as well,
and the model finished after four failing test runs. ED-04: coding made fourteen edit
attempts, ten failed, five failing test runs, finished with `fetchAll` still in
`service.py`; unattended landed three edits, ran three failing tests, finished "successfully
renamed". With the kernel on, unattended reached 0.89 by rewriting two files, appending to
the third and running the tests green. ED-05 is an explanation task on which the kernel is
idle; the probe's 0.79 was the referee not finding the mechanism named, noise at n=1.

Reading. On this model and these five briefs the kernel is worth 0.08 of mean score and
nine minutes, and the log lines say which parts earned it. The finish gates: six of ten
kernel-off cells finished over a failing last check, none did with the gates on; two of the
six summaries claimed success outright ("Fixed … so that 250 is 'standard' and 1000 is
'premium'"), the other four opened with "Fixed" or "Renamed" and admitted the failing tests
in a later clause, which the claims gate lets through by design as an honest report.
(Corrected 2026-10-31: the first version of this entry called all six false.) The failure ladder: without the three-failure `edit_file` disable and the redirect
hints the model loops on exact-match misses (ED-02 unattended, ED-04 coding). The ≤4B rule
cuts both ways: its whole-file rewrites won ED-02 and ED-04 unattended and lost ED-03 and
ED-04 coding to collateral, so it is not settled by this arm and needs its own single-switch
run (`AGENT_ABLATION=small_model`), as does `finish_gate` alone. The earlier reading that the
kernel sat idle on the surface tiers held for qwen3.5:9b; for a 2B model it did not.

## tier6_editor with gemma4:e2b, the ≤4B edit rule alone switched off (2026-10-31)

`AGENT_ABLATION=small_model`, every other compensation on, same axes; results in
`bench/results/surface_editor/gemma_e2b_small_model_off`, every transcript logging
`[ablation] off: small_model`. Mean 0.69 in 8 minutes, against 0.72 with the rule on and 0.64
with the whole kernel off.

| task | rule on (probe) coding / unattended | rule off coding / unattended | kernel off |
|---|---|---|---|
| ED-01 | 1.00 / 1.00 | 0.00 / 0.57 | 1.00 / 1.00 |
| ED-02 | 1.00 / 1.00 | 1.00 / 1.00 | 0.71 / 0.43 |
| ED-03 | 0.29 / 0.00 | 0.00 / 0.29 | 0.29 / 0.29 |
| ED-04 | 0.21 / 0.89 | 1.00 / 1.00 | 0.32 / 0.32 |
| ED-05 | 1.00 / 0.79 | 1.00 / 1.00 | 1.00 / 1.00 |
| mean | 0.72 | 0.69 | 0.64 |
| finish over a failing last check | 0 | 2 (ED-01 both) | 6 |

What the rule off changed. ED-04, the three-file rename, went to 1.00 under both presets:
with `edit_file` available the model renamed in place (coding: four edits landed, two
exact-match misses, tests green; unattended: three edits, tests green) where the whole-file
rewrites under the rule had destroyed collateral (coding 0.21) or needed an `append_file`
rescue (unattended 0.89). ED-02 stayed at 1.00 both ways, one edit and a passing re-run.
ED-01 went the other way, but not because of the edit protocol: coding worked entirely in an
invented `workdir/` subdirectory (seven `create_file` calls to `workdir/slug.py`, the real
file untouched, finish "Verified by running python test_slug.py" over three exit-1 runs,
accepted because the `coding` preset has no finish gates by policy); unattended landed a
wrong edit on the real file, finished admitting "tests still failed", was sent back by the
pre-delivery check, missed the old text three times, rewrote the file whole and finished
again with the same admission (4 of 6 tests). ED-03 coding did the `workdir/` thing again,
no edit involved. The ladders that were on held: the three-failure `edit_file` disable never
fired because no cell failed three times in a row, and the duplicate block and corrector
absorbed ED-01 coding's repeated reads.

Reading. At n=1 the ≤4B rule is a wash on mean (0.72 against 0.69) and decisive per task in
both directions: it costs the multi-file rename and buys nothing on the one-line fixes,
where the raw edit lands first time. There is no evidence here that a 2B model "cannot do
the exact-match protocol": 11 of 17 edits landed, and the misses were followed by a
successful rewrite, not a stall. The recurring quirk across all four gemma runs is the
invented `workdir/` path (one cell in the probe, one in the invalid arm, two here, none with
the kernel off): the system prompt names the working directory by its absolute path, whose
last segment is `workdir`, and the model then addresses files as `workdir/x`. That is a
kernel-shaped compensation to design (bead `boubou-8e6`), the edit rule is not. Bead for the rule's
fate: keep it only if a wider run (several models at 2–4B, n≥3) shows a mean gain; on this
evidence it should go.

## tier6_editor with gemma4:e2b, the finish gates alone switched off, and what the coding cells say about noise (2026-10-31)

`AGENT_ABLATION=finish_gate`, every other compensation on, same axes; results in
`bench/results/surface_editor/gemma_e2b_finish_gate_off`, every transcript logging
`[ablation] off: finish_gate`. Mean 0.59 in 15 minutes. Under `coding` the gates are off by
policy, so those five cells ran the probe's exact configuration again and are a repeat, not
an arm; the unattended cells carry the measurement.

| task | probe coding / unattended | gates off coding / unattended |
|---|---|---|
| ED-01 | 1.00 / 1.00 | 0.00 / 1.00 |
| ED-02 | 1.00 / 1.00 | 0.43 / 1.00 |
| ED-03 | 0.29 / 0.00 | 0.71 / 0.29 |
| ED-04 | 0.21 / 0.89 | 0.21 / 0.43 |
| ED-05 | 1.00 / 0.79 | 1.00 / 0.79 |
| preset mean | 0.70 / 0.74 | 0.47 / 0.70 |

The repeat first, because it bounds everything else. Five coding cells under an identical
configuration moved from 0.70 to 0.47: ED-01 1.00 to 0.00 and ED-02 1.00 to 0.43 (both the
invented `workdir/` path, six and four files created there), ED-03 0.29 to 0.71 (the
rewrite happened to keep the referee's cases). The run-to-run spread of a single cell at
this model size is the whole scale, and the spread of a five-cell preset mean is about 0.2.
Every mean difference reported in the three ablation entries above (0.72 against 0.64
against 0.69) is inside that spread. What those entries establish is the per-cell
mechanism readings from the harness lines (the `edit_file` loop without the failure
ladder, the six finishes over a failing check without the gates, the rename landing with
real edits), not the means. A mean claim needs n≥3 per cell, and the bench's `--runs` or a
repeat into separate `--results` dirs is the way to get it.

The measurement. Unattended with the gates off scored 0.70 against 0.74, and the change is in
two cells. ED-04 (0.89 to 0.43) ran 428 seconds and 70 calls: a `create_file` rewrite of
`repo.py`, appends to the other two files, then 24 identical `sed -i 's/fetchAll/fetch_all/'
store/repo.py` calls (BSD `sed` rejecting `-i` without a suffix), six empty turns and a hard
reset, the duplicate block and the corrector firing on the `sed` loop, ten test runs, and a
finish admitting the tests failed, accepted with no gate to send it back; the probe's cell
ended with a passing test and a gate that let it through. ED-03 (0.00 to 0.29): the probe's
cell was the `workdir/` loss, this one edited the real file and broke `collapse_spaces`. ED-01,
ED-02 and ED-05 unattended were identical to the probe. No finish in this arm was refused
(none could be), and the two unattended finishes over a failing check (ED-03, ED-04) both
admitted the failure in the summary, which the claims gate lets through anyway. So on these
five briefs the finish gates' measurable contribution under unattended is the test re-run
on ED-02 (confirmed, not changed the result) and nothing else; the kernel-off entry's six
finishes over a failing check came from the gates and the ladders going together, and this
arm says most of that was the ladders.

The `workdir/` path again: two more cells (ED-01 and ED-02 coding), six across five runs,
bead boubou-8e6. It is now the largest single cause of lost cells for this model, ahead of
anything the edit protocol or the gates did.

## The workdir-prefix rule, gemma4:e2b's profile only (2026-10-31, bead boubou-8e6)

Design (the bead carries the full paragraph). gemma4:e2b, and no other model in any result
folder (qwen3.5:9b 56 cells, gemma4:12b the profile runs), addresses files under an invented
subdirectory named like the working directory: its first call, before any tool result, is
`list_dir("workdir")` or `read_file("workdir/slug.py")`, the only source being the system
prompt's absolute path whose last segment is `workdir`; the `[not a dir: /abs/…/workdir]`
answer echoes that path, `create_file` then makes the directory silently, and the model works
in a copy and finishes "verified" against the untouched root tests. Eight cells across seven
folders. The compensation is on the tool face and switched by the profile: a relative path
whose first segment is the workdir's own name, when no such subdirectory exists, is taken
relative to the workdir. The tool result says so in one clause (`(path rebased:
workdir/slug.py → slug.py; this directory has no `workdir/` subdirectory, paths are relative
to it)`) and the harness logs `📁 path rebased: … → …`, so the bench counts firings. It never
fires on an absolute path, another first segment, or a real subdirectory of that name (a
package named like its repository is common), and it is off for every other model. A prompt
note was not chosen because a 2B model's compliance with a negative instruction is the thing
that cannot be assumed and the planting line must stay for `run_bash`; a refusal was not
chosen because the same model repeated `list_dir("workdir")` four times after four `[not a
dir]` answers.

Where. `ModelProfile::rebase_workdir_prefix` (built-in exact-tag entry `gemma4:e2b`, so e4b
and 12b are untouched; `config.json` can set it for any model), `Executor::rebase_workdir_prefix`
applied once in `dispatch` to the seven file tools through `rebase_invocation`, the
`path_rebase` switch in `agent/ablation.rs` (part of `kernel`), and the loop's wiring
`profile && !ablated`. Tests: `exec_tools.rs` (the rule's cases including the real
subdirectory and the absolute path; read, list, create and edit through the rule; off
behaves as before), `profile.rs` (e2b has it, 12b and e4b do not), `agent_loop.rs` (gemma4:e2b
gets it from its profile until `path_rebase` is set). 512 Rust tests, clippy clean.

Measurement (the probe configuration rerun, `bench/results/surface_editor/gemma_e2b_path_rebase`).
The rule fired in four of ten cells, every time on the model's opening `list_dir("workdir")`,
rebased to `.`; after that answer the model used root paths and no cell asked for a
`workdir/x` file. No cell has a `workdir/` subdirectory at the end, against six cells lost to
it in the five runs before. Mean 0.76, the highest of the six gemma4:e2b tier6 runs, with the
run-to-run caveat of the entry above: the per-cell reading is what counts here, and it is
that the specific loss is gone.

| task | probe coding / unattended | rule on coding / unattended |
|---|---|---|
| ED-01 | 1.00 / 1.00 | 1.00 / 1.00 |
| ED-02 | 1.00 / 1.00 | 1.00 / 1.00 |
| ED-03 | 0.29 / 0.00 | 0.71 / 0.71 |
| ED-04 | 0.21 / 0.89 | 0.21 / 0.21 |
| ED-05 | 1.00 / 0.79 | 1.00 / 0.79 |

Two cells worth the lines. ED-01 unattended (156 s): an untested rewrite failed its test, six
empty turns and a hard reset, then "Fixed" over the failing test, refused twice by the claims
gate ("`python test_slug.py` exited 1 but the summary reports success"), a third rewrite, the
test run green, finish allowed: the gate changed the result from a false finish to 1.00.
ED-04 unattended (200 s): the ≤4B rule's whole-file rewrite of `repo.py` again, then twelve
`sed -i` calls that BSD `sed` rejects, the corrector's two verdicts (one of them `cat | sed`,
which runs and changes nothing), the duplicate block, and a finish with the old name still
in two files; the same loss as the finish_gate arm's ED-04, and the rename stays the cell
the edit rule decides. Open question kept: the cells asked for `workdir` exactly, which is
both the basename and the plain noun, so whether the model copies the directory's name or
says "workdir" regardless is still not separable; a run with a workdir named otherwise would
tell.

## Bench: `--runs` and `--workdir-name` (2026-10-31, bead boubou-z6k)

`--reps` already ran every cell N times and showed `mean [min–max] n=N` per task; what the
ablation entries needed and did not have was the spread of an arm's mean across runs, the
number a difference between two results dirs has to clear. The driver now takes `--runs N`
(`--reps` stays as an alias), and the arm line reads `run means 0.70/0.47, sd 0.21` whenever
cells carry a run number. `--workdir-name NAME` (boubou arm only) lays the cell out under
`<cell>/NAME`, so the last segment of the absolute path the system prompt shows the model is
a word of the operator's choosing; it is recorded as axis `workdir`, enters the run id as
`_w<name>` when it is not the default, labels rows `dir=<name>` when it varies, and stays out of
the constant-axes line when it is the default (layout, not a setting). Tests:
`bench/test_runs_axis.py` (run id and axis, per-task spread and per-arm run means from a
synthetic results dir, two names in one dir, the dry-run layout, the one-segment check). 54
bench tests. No loop change.

The two runs these make possible, both yours to start: the probe configuration at `--runs 3`
(the baseline every arm is compared with), and the same with `--workdir-name proj`, which says
whether the gemma4:e2b path rule is covering a copied name or a fixed word.

## Three-run baseline, gemma4:e2b tier6_editor (2026-10-31, `gemma_e2b_x3`)

The probe configuration (kernel on, path rule on, coding and unattended) at `--runs 3`: 30
cells, 31 min, mean 0.748, run means 0.66/0.80/0.78, sd 0.07. The two earlier single runs of
this configuration (0.72, 0.76) sit inside that spread, and so does every arm measured so far
(kernel off 0.64 is the only one more than one sd below the mean, and even that is one run).
Per task, `mean [min–max]`:

| task | coding | unattended |
|---|---|---|
| ED-01 | 1.00 [1.00–1.00] | 1.00 [1.00–1.00] |
| ED-02 | 1.00 [1.00–1.00] | 1.00 [1.00–1.00] |
| ED-03 | 0.57 [0.29–0.71] | 0.67 [0.29–1.00] |
| ED-04 | 0.18 [0.00–0.32] | 0.25 [0.21–0.32] |
| ED-05 | 1.00 [1.00–1.00] | 0.81 [0.43–1.00] |

ED-01, ED-02 and ED-05 coding are solid at 1.00 three times each; ED-03 and ED-04 carry the
whole spread. ED-04 (the rename) lost all six cells to the same mechanism: with `edit_file`
hidden by the ≤4B rule the model rewrote `repo.py` whole with `create_file`, then used
`append_file` on `service.py` (a full second copy appended under the old one, so `fetchAll`
stays) in four of the six, or looped on BSD-rejected `sed -i` in the sixth; the smallest
possible edit was never available to it. The `small_model` ablation arm scored this cell
1.00/1.00 with real edits. That is the rule's bill at n=3 on its deciding cell, with no cell
in the folder where it won anything. ED-03's spread is model-shaped: whole-file rewrites of
`textfmt.py` up to nine times a cell while chasing the referee's capitalisation cases.

The path rule fired in 18 of 30 cells, always on the first call and always the same way:
`list_dir("workdir")` rebased to `.`; no cell asked for a file under `workdir/`, so nothing
ever had to be rewritten after the opening listing. 18 of 30 is the rate at which this model
addresses the working directory by its basename, which is also the plain word; the
`--workdir-name proj` run is what separates the two readings, and with the rule firing in
most cells the answer decides whether the rule covers real project directories or only this
bench's.

## The directory's name, not the word (2026-10-31, `gemma_e2b_proj`)

The probe configuration with `--workdir-name proj`, one run: the cells sit under `<cell>/proj`
and the only `workdir` the model can see is nowhere. gemma4:e2b asked for `proj` (two opening
listings) and `proj/slug.py` (two reads); it never said `workdir`. So the quirk is a copy of
the directory's basename into a path, and the profile rule `rebase_workdir_prefix`, which keys
on the basename, covers a real project directory the same as it covers this bench's. The rule
fired in 3 of 10 cells (4 firings) against 18 of 30 in the three-run baseline; one run, so
the rate is not a reading. 10 cells 0.839, 23 min, inside the baseline spread.

The cells, with the baseline's three-run mean for the same task and policy:

| task | coding (baseline) | unattended (baseline) |
|---|---|---|
| ED-01 | 0.57 (1.00) | 1.00 (1.00) |
| ED-02 | 1.00 (1.00) | 1.00 (1.00) |
| ED-03 | 1.00 (0.57) | 0.71 (0.67) |
| ED-04 | 1.00 (0.18) | 0.32 (0.25) |
| ED-05 | 1.00 (1.00) | 0.79 (0.81) |

ED-04 coding at 1.00 is the first time the rename passed with the ≤4B rule on; ED-04
unattended was the familiar loss, forty-odd BSD-rejected `sed -i` calls, the corrector's
give-up, 23 empty turns and three resets, 608 s. ED-05 unattended did not run: a `cargo clean`
of mine (meant to clear 56 GB of stale debug objects, see the `[profile.dev]` note in
`Cargo.toml`) removed `target/release/boubou` under the driver between cells, so the folder
exists with no transcript; the cell was run on its own after the rebuild: 0.79 in 22 s, the
explanation accepted at the second finish, `reply.md` missing the scorer's phrasing, the
baseline's own mean for that cell.

## The ≤4B rule removed (2026-10-31, bead boubou-6uj)

Mingbird's `_small_model` hid `edit_file` from every model parsed at four billion parameters
or fewer before the first step, on the premise (its comment) that 2–4B models fail the exact
`old_text` match, and added a Windows `cmd` teaching hint to failed shell commands; it was
never measured there. Measured here under its switch: ED-04 lost all six baseline cells to the
whole-file rewrites and appended copies the rule forced, scored 1.00/1.00 with the switch off,
and no cell in any folder was won by it. Design: tool availability is not a function of
parameter count. A model that needs a tool withheld says so in its profile on the exact tag
with an evidence note (`hide_tools`, the way the gemma4:e2b path rule was placed); a model
that fails a tool's contract gets a better tool (`bk9.3`, matching tiers and argument repair
answer the premise directly) and meanwhile loses the tool through the failure ladder's
three-failure `edit_file` disable, per run, on evidence. The blanket pre-emptive hide was
redundant with both and a measured loss.

Removed: the gate in `agent/run.rs` with both effects, `guards::is_small_model` and its
threshold, the `small_model` ablation switch (nothing left under it; the name is now reported
as unknown, so the folders recorded under it stay readable), `CallLadders::bash_teach`. Kept:
the ladder's reactive disable, `kernel`. Tests: `a_two_billion_model_gets_edit_file_like_any_
other` (gemma4:e2b, qwen3.5:4b, gemma4:12b all run their first edit); the switch-list tests
on another switch. `format_bash_out`'s `small_model` parameter is fed the slim-output flag and
was never this rule; left for a rename of its own. The after-measurement is the probe
configuration at `--runs 3` against `gemma_e2b_x3`: ED-04 should rise, nothing else should
move outside sd 0.07; qwen3.5:4b, now in the inventory and exactly on the old boundary, is the
second model to run it on.

## After the ≤4B rule, gemma4:e2b tier6_editor at three runs (2026-10-31, `gemma_e2b_no_small_rule`)

Same configuration as the baseline `gemma_e2b_x3`, binary at 5318bc8 (rule removed):

| | baseline (rule on) | after (rule gone) |
|---|---|---|
| mean over 30 cells | 0.748 | 0.825 |
| run means | 0.66 / 0.80 / 0.78 | 0.82 / 0.77 / 0.89 |
| sd across runs | 0.07 | 0.06 |
| wall | 31 min | 31 min |

| task | coding before → after | unattended before → after |
|---|---|---|
| ED-01 | 1.00 → 0.86 [0.57–1.00] | 1.00 → 1.00 |
| ED-02 | 1.00 → 0.62 [0.43–1.00] | 1.00 → 0.90 [0.71–1.00] |
| ED-03 | 0.57 → 0.57 | 0.67 → 0.71 |
| ED-04 | 0.18 → 0.77 [0.32–1.00] | 0.25 → 0.81 [0.43–1.00] |
| ED-05 | 1.00 → 1.00 | 0.81 → 1.00 |

The difference on the mean is one standard deviation, so the mean is not the reading; the
cells are. ED-04, the rule's cell, went from 0 of 6 to 4 of 6 full passes: the model edits
`fetchAll` to `fetch_all` file by file and the tests go green. Its two losses are new
mechanisms: `edit_file {"old":"fetchAll","new":"fetch_all"}` replaces one occurrence, so a
file with two keeps the old name (coding r0, 0.32), and a second edit written against the
text the first one had already changed fails "old text not found" and is retried verbatim
(unattended r1). ED-02 coding is the rule's premise coming true: in two of three cells the
first edit landed but was wrong, and every edit after it carried stale `old` text, failed,
was blocked as a duplicate, and under `coding` the turn then ended (0.43 twice); ED-01 coding
r1 is a wrong first edit finished without a test run. "Old text not found" misses: ED-01 4
in 2 cells, ED-02 7 in 3, ED-03 4 in 2, ED-04 8 in 3; the ladders answered with fifteen
duplicate blocks, three corrector give-ups and two missing-`path` repairs, and two cells
(ED-03 unattended r1, ED-04 unattended r1) reached the three-failure disable and finished
with `create_file`, which is the reactive form of the old rule doing its job on evidence.
Reading: the rule's cost (whole-file rewrites losing the rename
every time) is replaced by a smaller, better-shaped cost (exact-match misses after the
model's own edit, and single-occurrence replacement), and that cost is exactly what `bk9.3`
(matching tiers, argument repair, occurrence reporting) is for. The rule stays out.

## ED-06: escapes and a near-duplicate block (2026-10-31, bead boubou-l2j, for bk9.3)

`edit_file` unescapes `\n`, `\t` and `\"` in `old` and `new` before matching, Mingbird's
inheritance, so a correctly quoted edit of a line that contains an escape sequence misses,
and a correctly quoted `new` writes a real newline into a string literal: the one failure
worse than a miss. No tier6 fixture contained an escape sequence and none of the 23 misses
in `gemma_e2b_no_small_rule` involved one, so every run so far scored this at zero cost.
Another agent's suggestion: ship the `bk9.3` rework with the escape fix, plus a sixth
editor task with escapes and a near-duplicate block. Taken, with the sequencing changed:
the task first and a before number on the current binary, then the rework, so the fix is
not measured by the thing it ships with (the ≤4B rule's lesson).

ED-06 (`gen_surface.py`, `fixtures/ed06_export`): `export.py` has `quote` (escapes `"` and
newlines, so `'\\"'` and `"\\n"` sit in the source), `csv_line` and `tsv_line`, and
`tsv_line` joins with commas like `csv_line`; the line to change, `",".join(cells) + "\n"`,
appears twice, so a bare `old` of that line hits `csv_line` first and the collateral checker
names it. The brief asks for a tab in `tsv_line` and nothing else. Untouched: 4 of 6 tests
pass, `COLLATERAL PENDING`. `test_surface.py` proves the escapes are in the file, the
all-occurrences edit breaks `csv_line` and is caught, the second-occurrence edit scores. It
is a regression fixture for the matcher's escape handling and occurrence counting, labelled
so in the README, not a measure of how often small models meet these cases. The escape fix
goes into the `bk9.3` design as a tier, not a preprocessor: exact on the text as given first,
the unescaped form only when exact finds nothing, and `new` unescaped only when that tier is
the one that matched. Before number: six cells (coding and unattended, three runs) on
gemma4:e2b at 5318bc8, appended to `gemma_e2b_no_small_rule` so that folder is the complete
baseline for the rework; see the next entry.

## ED-06 before number (2026-10-31, `gemma_e2b_no_small_rule`, binary 5318bc8)

Six cells, coding and unattended, three runs each: 0.81 [0.43–1.00] under both policies.
Eighteen `edit_file` calls, eighteen "old text not found", no edit landed in any cell. The
model's quoting was right: the logged `new` arguments carry `\t` and `\n` as two-character
escape sequences, which is what the Python source contains, and the quoted line occurs in the
file; the tool's unconditional unescape turns the `\n` into a real newline before matching,
and that string is not in the file. Every cell then ran the same ladder: duplicate block,
corrector give-up ("only proposed read_file"), and a whole-file `create_file`. Four rewrites
reproduced the file correctly (1.00); two also rewrote `quote` and were caught by the
collateral checker (0.43, coding r1 and unattended r1). Had the match succeeded, the
single-occurrence `replacen` would have changed `csv_line`, so both of the fixture's hazards
are live on this binary. This is the number `bk9.3` is measured against for this task; the
folder now holds all twelve task-and-policy cells at three runs (36 cells, 0.822, run means
0.85/0.71/0.90, sd 0.10).

## edit_file answers with the truth about the file (2026-10-31, bead boubou-bk9.3)

Design on the bead and in `crates/boubou-kernel/src/edit_match.rs`'s module doc. Matching
tiers, each run only when the previous found nothing: exact on the text as given (the
inherited unconditional unescape is gone as a preprocessor; ED-06's eighteen misses were
correct quotes of a line containing `\n`), then the escape sequences unescaped (and `new`
unescaped only then), then whole lines after NFKC, then with trailing whitespace ignored, then
with typographic quotes, dashes and non-breaking spaces normalised. A tier applies only when
it finds something; nothing is guessed below the tiers. Every tier counts occurrences: more
than one is `[edit failed: old text found N times in <path>. Include more of the surrounding
lines so it matches once, or pass all=true to replace every occurrence.]`, and `all=true`
(new optional argument, string booleans coerced as before) replaces them all and says how
many. When `old` is absent but `new` is in the file the call is `[no change: <path> already
contains the new text …]`, a success that writes nothing and takes no backup: the stale quote
after the model's own edit stops at its first call instead of after the duplicate block and
the corrector. A miss shows the region of the file most like `old` (mean character-bigram
similarity of aligned lines, earliest on a tie) with its line numbers, in place of the first
two hundred characters of the file. Results that matched below exact say so: `(old text
matched ignoring trailing whitespace)`. Argument repair in `coerce_args`: an array or object
parameter sent as a JSON string is parsed and a lone object where an array is expected is
wrapped; anything else still reaches the validator. The hunks array (design item 6, optional)
is not in this change.

One guard input corrected, outside the tool: `CommandRecord::file_write` counted
`[edit failed` as a write that ran (it predates this change); it now treats a missed or
no-change edit like a refused one, so the "files changed and no check ran" gate is not armed
by an edit that changed nothing. The loop itself is untouched; the duplicate block, the
corrector and the three-failure disable stay as the backstop. Ablation `edit_match` (in
`kernel`) restores Mingbird's matcher exactly (`edit_file_legacy`). Tests: four unit tests in
the module, seven in `exec_tools.rs` (ED-06's shape as written and the quoted newline, the
count and `all`, no-change with no `.bak`, the nearest lines, the three line tiers, the
switch off, the argument repair), one loop test through the real Executor (a stale re-quote
is no-change with the matcher on and the old miss with it off), and the integrity record.
New dependency `unicode-normalization`. After-measurement: tier6_editor (now six tasks) on
gemma4:e2b at `--runs 3` against `gemma_e2b_no_small_rule`; the read is the miss count (23 in
10 cells, plus ED-06's 18 in 6), ED-02 coding (0.62), ED-04 coding (the one-occurrence loss)
and ED-06's six cells.

## After bk9.3, gemma4:e2b tier6_editor at three runs (2026-10-31, `gemma_e2b_edit_match`)

Same configuration as `gemma_e2b_no_small_rule`, binary at 7c184bf, six tasks, 36 cells:

| | before (bk9.3 off) | after |
|---|---|---|
| mean over 36 cells | 0.822 | 0.794 |
| run means | 0.85 / 0.71 / 0.90 | 0.82 / 0.80 / 0.76 |
| sd across runs | 0.10 | 0.03 |
| wall | 35 min | 41 min |
| `edit_file` calls / landed | 95 / 40 | 175 / 53 |
| "old text not found" | 41 | 16 |
| "found N times" refusals | — | 3 |
| "no change" answers | — | 87 |
| calls sent with `all=true` | — | 150 |
| edits that replaced 2+ occurrences | — | 7 |
| a tier below exact matched | — | 0 |

| task | coding before → after | unattended before → after |
|---|---|---|
| ED-01 | 0.86 → 0.86 | 1.00 → 1.00 |
| ED-02 | 0.62 → 0.86 | 0.90 → 0.90 |
| ED-03 | 0.57 → 0.33 | 0.71 → 0.57 |
| ED-04 | 0.77 → 0.48 | 0.81 → 1.00 |
| ED-05 | 1.00 → 1.00 | 1.00 → 0.86 |
| ED-06 | 0.81 → 0.67 | 0.81 → 1.00 |

What the tool fixed, it fixed: misses fell from 41 to 16, ED-06's escape case is gone (its
quoted lines now land on the first call), ED-02 coding came back from the stale-quote loop,
ED-04 and ED-06 unattended are 1.00 three times each, and the run-to-run spread is the
smallest of any folder. The fuzzy tiers never matched once: this model's bad quotes are
stale or of the wrong region, not whitespace or typography, so the tiers cost nothing and
earned nothing here. Two new surfaces then did damage of their own.

`all=true`. The model sent it on 150 of 175 calls: it fills every schema field, and a
boolean it fills with `true`. On ED-06 the count refusal offered it by name ("or pass
all=true") and the model took the offer: coding r2 is `found 2 times` → the same call with
`all=true` → both lines replaced → finish, 0.00, the exact collateral the fixture was built
to catch, produced by the error message that was meant to prevent it. Seven landed edits
replaced two or more occurrences; two of them were right (ED-04's import and call in one
file), the rest were ED-06's shared line.

"No change". The answer is true about the file every time it was given, and the model does
not take it as done: it re-issues the edit, usually with `all=true` and a wider quote, and
the loop sees a success each time, so no ladder escalates. ED-06 coding r0 took 33 edit
calls (3 landed, 28 no-change) before the loop disabled the tool and a whole-file write
finished it at 1.00; unattended r0 and r2 the same at 25 and 23 calls; ED-04 coding r2 ran
fourteen no-change answers in a row on a file it had already renamed and finished at 0.32.
That is the six extra minutes of wall. ED-03's two drops are not the matcher's: coding r0
was two empty replies and the turn ended with nothing done, unattended r2 was ten
`edit_file` calls without `path` in a row.

Reading. The matcher did the job it was built for and the two things added around it, an
argument the model cannot be trusted with and a success message the model cannot act on, cost
about what it gained. Both are small-model quirks meeting a surface that frontier harnesses
can expose safely. Neither is a reason to switch the matcher off: the ablation `edit_match`
restores the 41 misses as well. The design response is a bead, not a patch.

## bk9.3 follow-up: no invitation, and the state instead of an instruction (2026-10-31, bead boubou-orr)

Two tool-side changes, both under `edit_match`, no loop change. `all` leaves the advertised
`edit_file` schema and its description: a small model fills every boolean with `true` (150 of
175 calls in `gemma_e2b_edit_match`) and took the count refusal's "or pass all=true" as the
fix, which produced the collateral ED-06 was built to catch. The argument is still honoured
when sent (`arg_types` keeps its boolean coercion for the CLI user or a model that earns it),
it is just never offered; the count refusal now reads `[edit failed: old text found N times
in <path>; nothing was written. Include more of the surrounding lines in old so it matches
exactly once, and edit each place separately.]`. The no-change answer carries the state
instead of "read the file": `[no change: this edit is already done. <path> lines a–b now
read:` followed by the whole lines that hold the new text, then `The old text is not in the
file and nothing was written. Do not repeat this edit; go on to the next step.]`. Whether that
stops the repetition (87 no-change answers, 28 in one cell) is the measurement; if it does
not, the next design is the ladder treating a second no-change on one file as a repeat,
which is a loop change and gets its own paragraph first. Tests: the schema has no `all`
property and its description does not mention it; the refusal names no escape hatch; the
no-change answer quotes the line with its number, in the tool test and through the loop; the
module test covers a multi-line new text spanning two lines. 524 tests. After-measurement:
tier6 gemma4:e2b `--runs 3` against `gemma_e2b_edit_match`; the read is `all=true` sent
(should be none), the no-change count, and ED-06 and ED-04 under coding.

## Fail-fast check of the follow-up: ED-06 alone, three runs (2026-10-31, `gemma_e2b_edit_match2`)

Six cells, 6 minutes, binary d156d4e. `all=true` sent: 0 of 13 calls (was 150 of 175 across
the tier). No-change answers: 0 (was 67 in these six cells); nothing ever reached the
already-applied state, so there was nothing to repeat. Misses: 0. Count refusals: 11.
Edits landed: 2. Whole-file rewrites: 2. Scores 1.00, 0.00, 1.00 / 1.00, 1.00, 0.00, mean
0.67 under both policies, the same as `gemma_e2b_edit_match` and below the 0.81 of the
pre-bk9.3 folder, which four whole-file rewrites had earned.

The shape left is one mechanism in every failing cell: the model quotes the one shared line,
is told it is found twice and to include more surrounding lines, and re-sends the identical
call; the duplicate block takes the repeats, the corrector gives up ("only proposed
read_file") and blocks the tool, and the cell ends in a whole-file rewrite (two cells, 1.00),
or an honest "failed to modify" finish (two cells, 0.00). Two cells widened the quote on their
own and landed it (1.00). The refusal is true and the model cannot act on "include more
surrounding lines": the same finding as the no-change answer, an instruction where the state
was needed. Next design, filed as a bead: the count refusal shows the N places with their line
numbers and the lines around each, so the model sees what distinguishes them and copies one.

## The count refusal shows the places (2026-10-31, bead boubou-7td)

Tool-side, under `edit_match`, no loop change. `[edit failed: old text found N times in
<path>; nothing was written.]` is followed by each place (up to four, then "and k more") as
`Lines a–b read:` with the whole lines holding the match and one line of context each side,
then `Quote one of these regions as old, exactly as shown, so it matches once; edit each
place separately.` On ED-06 that puts the two docstrings that distinguish `csv_line` from
`tsv_line` in front of the model, where "include more of the surrounding lines" left it
re-sending the same one-line quote until the corrector blocked the tool (4 of 6 cells in
`gemma_e2b_edit_match2`). `edit_match::place` is the helper, shared with the no-change answer
(context 0). Tests: the module clips context at both ends of the file; the tool test checks
both regions and their numbers for a two-function file. 524 tests. Measured next on ED-06
alone, gemma4:e2b and then qwen3.5:2b head to head, so the matcher's answers are read on two
models before the full tier runs again.

## ED-06 head to head: gemma4:e2b and qwen3.5:2b on the places-shown refusal (2026-10-31)

Binary at the bk9.3 follow-ups (`edit_match::place`), ED-06 alone, both policies, three runs
each; `gemma_e2b_edit_match3` and `qwen3_5_2b_ed06` (first run of qwen3.5:2b, no profile).

| | gemma4:e2b | qwen3.5:2b |
|---|---|---|
| coding / unattended | 0.81 [0.43–1.00] / 1.00 | 1.00 / 1.00 |
| wall | 5 min | 7 min |
| edit calls / landed | 21 / 4 | 13 / 8 |
| count refusals | 15 | 4 |
| after a refusal, the next call landed / was the same quote again | 2 / 9 | 0 / 3 |
| misses, `all=true`, tiers below exact | 0, 0, 0 | 0, 0, 0 |
| no-change answers (followed by a repeat) | 2 (0) | 1 (0) |
| whole-file rewrites | 4 | 1 |

Reading. Showing the places did not change what the model does after the refusal: gemma
re-sent the identical one-line quote nine times out of eleven, as it had with the plain
instruction, and qwen did the same in the one cell where it met the refusal. Gemma's rise
from 0.67 to 0.90 is three of four whole-file rewrites coming out right instead of two of
four, which is noise, plus two cells where a later quote was wide enough. qwen3.5:2b passes
all six cells because five of them quote the whole function, docstring included, on the
first call and never meet the count; the matcher's answers are not gemma-shaped, and the
tool is fine for a 2B model that quotes a unique region. The no-change answer, in its three
appearances across both models, was followed by the next step and not by a repeat, which is
what the follow-up asked of it, at n=3. Conclusion after two rounds on the refusal's wording:
a 2B model does not act on a refusal of this kind however it is phrased, by instruction or by
state; what decides the cell is whether its first quote is unique, and when it is not the
whole-file rewrite is what rescues it. The refusal stays as it is (true, and the places are
the right thing to show), and the wording is not iterated again. bk9.3's remaining
measurement is the full tier once per model.

## ACP: files through the editor (2026-10-31, bead boubou-m64.2)

Another agent's reading that "Boubou does not allow file access or edits over ACP" was
half right: edits were allowed and shown as diffs with the permission dialog, but they went
straight to disk through the executor, and the client's `fs` capability advertised at
`initialize` was ignored, so an editor's unsaved buffer was invisible to `read_file` and an
`edit_file` landed beneath it. The seam is the one the safety gate already had with
`Confirmer`: `exec::FileStore` (`read`, `write`, `append`) with `DiskStore` as the default,
used by the four text-file tools only; `runner::AcpFileStore` relays through the turn's
`BridgeMsg` channel with a blocking reply to `fs/read_text_file` and `fs/write_text_file`,
each only when the client offered that method (`ClientFs`, captured at `initialize`), else
the disk. The CLI and the bench are unchanged (the disk store is the old code path; the 524
existing tests passed untouched), no protocol type enters the kernel, and the backup copy
is now written from the text the model read rather than copied from disk, so it is of the
buffer when the buffer is the source. `_auto_repair` became the pure `repair_escapes`: the
repaired text goes through the store instead of being read back from disk, and
`append_file` repairs the fragment it adds rather than the whole file. Proved by the
in-process client (`crates/boubou-coding/tests/acp_turn.rs`): with the capability the read
served the buffer's text while the disk said otherwise, the edit and the create reached the
client's write handler and the disk was not written; without it the disk was used and the
client's file handlers were never called. Out of scope and documented: `delete_file`,
`list_dir`, `search_files`, images, commands and their backups, existence checks (the
protocol has no method for them), and the `terminal` capability. Not yet verified in Zed
itself; that remains the first item of boubou-m64.

## Skill folders in the Agent Skills shape (2026-10-31, bead boubou-w3i.1)

First item of the extensibility epic boubou-w3i, from the capability review of the same day
(system prompt, skills, extensibility against the plugin ecosystem). Boubou's skills were flat
`skills/*.md` files in three directories; the convention the other harnesses share is a folder
per skill, `<name>/SKILL.md` with the same frontmatter and optional `scripts/`, `references/`
and `assets/` beside it, read from `.agents/skills` and `.claude/skills` in the project and in
the home. This repository already carried one (`.agents/skills/beads`, the beads tool's own)
that Boubou could not see. Discovery now takes both shapes in every lookup directory, the
lookup list gained the four conventional directories between the project's own `skills/` and
Boubou's home (one `standard_dirs` the loop, the `skills` tool and the CLI all use, where the
loop and the tool each kept a copy), the frontmatter parser takes the convention's folded
`description` and nested `metadata`, a folder without `name` is named after its folder, and
loading a folder skill appends its companion files as absolute paths, the convention's third
disclosure level, paid only on load (a test reads the listed script back through
`read_file`). Prompt cost by default: none. The listing is the `skills` tool's result, not
prompt text, and a standard skill has no `triggers`/`autoload`, so nothing is prefilled unless
the user adds those keys; the token reading on a small profile that the epic asks for is
therefore owed only when a folder skill is prefilled, which the existing cap of three per task
still bounds. Not done: `allowed-tools` (the safety rings gate tools, not a skill), acting on
`metadata`, `.cursor/rules`.

## Hooks (2026-10-31, bead boubou-w3i.2)

Second item of the extensibility epic. Boubou had no way for a project or a user to run their
own code at the loop's events, and the three vendor directories in this repository showed what
that is used for (`bd prime` at session start, `bd` bookkeeping around tool use and
compaction) in one JSON shape. The design, on the bead: command hooks in that shape and
protocol rather than a Rust trait only an embedder could use or a Boubou-only format, read
from Boubou's own files and from the project's `.claude/settings.json` so an existing one runs
unchanged. Four events at seams the loop already had: `SessionStart` in the opening prompt
build (context into the task prompt under its own heading, capped at 6,000 characters, size in
the log, once per fresh session whichever prompt build comes first), `PreToolUse` and
`PostToolUse` in the executor's `run_tool` around prepare and dispatch (so `batch_tools` and the
corrector's retry see them too; a block is a tool error for the failure ladder), `Stop` at the
two points a turn ends (a block is a user message, a finish is deferred, at most once per turn
so a hook that always blocks cannot hold the loop; the second call sees `stop_hook_active`).
Hooks only narrow: an `allow` is an ordinary proceed and the safety rings keep the last word.
`AGENT_HOOKS=0` turns them off. Tests (`crates/boubou-kernel/tests/hooks.rs`, Unix): the shape
from four sources merged with a flat entry, a skipped bad matcher, an ignored event type and a
per-entry timeout; the executor blocking a delete on exit 2 while the file stays, appending a
`PostToolUse` context, and handing the protocol's input to a hook; the session context in the
task prompt once across the opening build and the loop's rebuild with a failing hook logged;
a stop hook holding a text turn open once and deferring a finish once. Prompt cost: none
unless a `SessionStart` hook prints; `bd prime` prints about 3,500 characters, roughly a
thousand tokens on a 1,250-token prefill, which is why the size is logged per run and
`docs/hooks.md` says to measure before keeping such a hook on a small profile. Not done, in
the docs: `UserPromptSubmit`, `PreCompact`, `continue: false`, async and prompt-type hooks,
`~/.claude/settings.json`, Cursor's camel-case file.

## Prompt assembly: the environment line and AGENTS.md (2026-10-31, bead boubou-bk9.5)

The bead named three ports from pi and opencode. Taken: an environment line and `AGENTS.md`.
Not taken, with the reason on the bead: per-tool prose rules in the prompt, because the rule
already lives in the tool's schema description (where bk9.3 put `edit_file`'s), is counted
once in the prefill, and a second statement of it costs tokens on every turn for a model that
will not follow two copies better than one; the bead's item 1 closes as "rules live in the
schema descriptions". The environment line sits after the workdir line of the task prompt
and says the date, whether the directory is a git repository and on which branch (read from
`.git/HEAD`, no subprocess) and which of sixteen common local tools are on PATH, probed once
per run (po1's item 1, the tool inventory, done here). Measured on this machine: the task
prompt grew from 8,161 to 8,267 characters, 106 more, about 25 to 30 tokens; the Ollama
token reading (`prompt_eval_count` on the first call) is the next bench run's to record, and
`AGENT_ABLATION=env_block` is the comparison arm, since the line is a compensation like any
other and must earn its place the same way. The Q&A prompt does not carry it. `AGENTS.md` is
read after `SYSTEM.md`, appended under the same heading in both prompts and capped at 6,000
characters with a marker, so a project written for another harness brings its instructions
along while `SYSTEM.md` stays the place for Boubou's own; its cost is the project's choice
and is not counted here. Tests in `crates/boubou-kernel/tests/environment.rs`: the probe on
a fresh directory, a branch, a detached head and a worktree; the ordering SYSTEM.md, AGENTS.md,
workdir line, environment line, platform block; the cap; the line through the agent, absent
on a question's prompt and under the ablation. The two prompt-build functions in the loop now
take one `PromptInputs` struct instead of eight arguments.

## Project `.mcp.json` and the editor's MCP servers (2026-10-31, bead boubou-w3i.3)

Third and last item of the extensibility epic boubou-w3i. MCP servers came from one file,
the home `mcp.json`, so a project could not bring its own and the servers an editor passed in
`session/new` were logged and dropped. One loader now serves a session: the home file as
before, then the project's `.mcp.json` in either shape (the `mcpServers` wrapper the other
harnesses write, with its `type` key ignored, or Boubou's bare map; `categories`, `tools` and
`expose` work in both), a project server replacing a home server of the same name; under
ACP the client's servers are converted to the same `ServerConfig` (stdio command, args and
env; HTTP url and headers; SSE tried as HTTP) and merged on top for that session. Every
consumer goes through the loader: the loop's executor, `boubou tool`, `boubou doctor` (which
counts the project's separately). When a server is launched is unchanged: only when a tool
of its category is routed or enabled, with the manifest cache as before, so the prompt cost
is nil until then and the same as any configured server after. A project's `.mcp.json` runs
processes the project chose, as its hooks do; `AGENT_PROJECT_MCP=0` ignores it and the docs
say to read it first. Tests: both shapes parse to equal configs; the merge and its precedence,
a broken project file skipped with the home kept, the executor's configured registry seeing a
project server; over the in-process ACP client, a stdio and an HTTP server passed in
`session/new` reach the turn as configs and a second session without servers gets none. Not
done: `${VAR}` expansion in env values, the SSE transport proper, per-server approval. With
this the epic's three items are in; what remains is the Zed verification on boubou-m64, which
now covers the fs path and the session servers too.

## Live Zed session: six harness fixes (2026-10-31, beads boubou-bk9.8–12, boubou-m64.3)

The Zed verification on boubou-m64 (Zed 1.26.2, qwen3.5:4b, ctx 64000, the release binary
of 12:50, project overshare) was read from the turn "Okay let's build that item next" on,
the first turn under the current build: 39 run_bash, 5 read_file, 2 edit_file, 2 todo, 1
finish. The model's navigation trouble the user saw was downstream of one harness defect.
`unescape_command`, Mingbird's run_bash unescape, turned every literal `\n`, `\t`, `\"`
and `\'` in a command into the real character before the shell saw it; four correct
commands in a row failed for that reason alone (two BSD sed substitutions with `\n` in the
replacement, a quoted python heredoc with `\n` in a string literal, a `python3 -c` whose
`\"` became bare quotes so the shell split the program and ran ImageMagick's `import`), after
which the model gave up on edits, recreated whole files by `rm` and heredoc, wrote the wrong
class into host_service.dart and later ran `rm lib/content/lib/*.dart lib/sharing/lib/*.dart`.
Eight of the 39 bash calls were `rm -f X.bak` or `mv X.bak X` housekeeping drawn by the
`[auto-backed up: X]` note. `read_file` on a path that was a directory (the model's own
`mkdir -p lib/sharing/pubspec.yaml`) came back as Zed's internal error. The same-tool streak
note fired five times, every time on distinct commands. Model-level and left alone: the
identical edit_file re-send after a nearest-region miss, `ls | head -5` then "the directory
is empty", `finish {}` after `flutter clean` with nothing verified.

Fixes, each against the design paragraph on its bead. bk9.10: the shell gets the command as
the model wrote it; the gate judges the original and the unescaped form, so a literal `\n`
still cannot hide a second command (test: `printf '%s'` shows the sed `\n` and the `\"`
quotes reaching the shell intact, the shutdown-after-newline command is still refused).
bk9.11: the note reads `[backup of X kept by the harness; nothing to clean up]` (CLI: `kept
beside each as .bak for rollback; leave them`). bk9.12: `rm` joins the snapshot suspects and
glob tokens are expanded inside the workspace (`glob` crate), so `rm lib/*.dart` backs up
both files first. m64.3: `read_file` on a directory answers `[not a file: … is a directory;
use list_dir]` for every store, and the ACP store falls back to the disk when the editor's
read fails, reporting both errors only if the disk fails too. bk9.9: the streak note needs
the last call to repeat the one before it (five distinct calls stay quiet; the disable rule
is unchanged) and its text says so. bk9.8: the enable_tools line had already been fixed;
the platform block's "Command map: list dir=ls / show file=cat / search text=grep" became
"To look around use list_dir, read_file and search_files rather than ls, cat and grep (their
output is bounded and says when it is cut)", Windows keeps its command map with the same
pointer. Task prompt 8,161 → 8,226 characters (+65, about 15 tokens). 549 tests.

What the testing missed, and the answer. Every one of these was a harness behaviour that
turned a correct model action into a failure or a wasted call, and the suite never saw it
because (1) the unit tests exercised each mechanism with inputs shaped like the mechanism
(`echo one\necho two`), not like what a model writes (a sed replacement, a `python -c`
program), and the Mingbird-ported heuristics (`unescape_command`, the suspects list, the
note wording) were ported without a switch or a measurement, unlike the matcher and the ≤4B
rule; (2) the bench scores outcomes and counts edit misses, but has no counter for
harness-caused bash failures, `.bak` housekeeping calls, streak notes per cell or editor
errors relayed verbatim, so a cell could lose to these and the loss read as the model's;
(3) the ACP tests scripted a cooperative editor and never Zed's actual error shapes. The
cheap corrective is a replay corpus: the tool calls of real sessions (this one first) run
through `prepare` and the stores as regression tests, plus three process-table counters in
`lrab_h2h.py` (bash exits whose stderr says `unescaped newline`, `unterminated string` or
`command not found`; calls naming `.bak`; harness notes per cell), read before and after any
change to exec. Still open from the Zed run: a permission prompt for an out-of-workdir write,
cancel mid-tool. The backup copies under the session's `.files/backups/` hold each file's
content as of the last snapshot before a writing command, which for host_service.dart is
already the wrong class; `std::fs::copy` on macOS keeps the source's modification time, so
a copy's timestamp is not when it was taken.

## Replay corpus and harness-damage counters (2026-10-31, beads boubou-bk9.13, boubou-bk9.14)

The two correctives named in the previous entry. `bench/replay_extract.py` reads a Boubou
session transcript, takes the assistant tool calls from an index, rewrites the session's
working directory to `{workdir}` and writes one JSON line per call into
`crates/boubou-kernel/tests/replay/<corpus>.jsonl`; `tests/replay.rs` runs every corpus file
against a fresh relocated executor per entry: `as_written` means `Executor::prepare` (now
pub) hands the shell exactly the model's command, unblocked; `blocked` means the gate
refuses; an object expectation runs `run_tool` over a fixture and checks `starts_with`,
`contains`, `not_contains` and `snapshots` (backup copies under the harness directory). The
first corpus is the current-build part of the live Zed session: 35 extracted bash commands
(five whole-file heredocs over 1,500 characters and the `/tmp` writers left out) and four
hand-written entries (the hidden-shutdown gate case, `read_file` on a directory, the
`rm *.dart` snapshot, the redirect's backup note). All 39 hold; 4 of the 35 extracted
commands contain a sequence the old unescape would have rewritten, so the corpus fails 4
times on the code of this morning. The loop prints two new protocol facts: `[i] 🐚 [exit N]:
<first stderr line>` after a failed `run_bash` and `[i] ⚠️ streak note: <tool> <n> in a row`
(the disable gets `[i] ⚠️ <tool> <n> in a row is now disabled`, caught by the existing guard
needle); the ⚙ line's arguments head grew from 60 to 160 characters so a path's tail is
visible. `process_metrics.py` turns them into `shell_errors` (stderr lines matching the
shell's or an interpreter's complaint about the command text), `bak_calls` (arguments
mentioning `.bak`) and `harness_notes` (streak notes + idle nudges + budget nudges), three
new summary columns `shell`, `bak`, `notes`. Tests: `test_harness_damage_counters` (the dart
analyzer's own error is not a shell error), the replay test. The kernel states facts and the
bench owns the classification, so both arms are read the same way. Release binary rebuilt.

## Skills-install session under Zed: two P1 fixes (2026-10-31, beads boubou-m64.4, boubou-m64.5)

Read from the session where the user asked for the cryptopeer skills to be installed
(qwen3.5:4b, ctx 64000). The task completed at the model's step 28 (`curl … | sh` put 27
`<name>/SKILL.md` folders under the project's `.claude/skills`, which Boubou reads since
w3i.1) and two harness faults hid that and corrupted what followed. First, `web_fetch`
showed 800 characters of a 10,000-character page and pointed at `sources/fetch_<key>.txt`;
the model, which writes absolute paths under an editor, read `/sources/fetch_<key>.txt`, got
not-found and never saw the install table the user then pasted by hand; the files themselves
were written into the user's project (15 fetch dumps untracked in git), though every other
harness file is relocated for editor sessions. Now fetched texts are a harness file:
`Executor::sources_dir` is `<harness_dir>/sources` when relocated and `<workdir>/sources`
otherwise, `web::SaveTo` carries the directory and whether the pointer is absolute, and the
note names the absolute path under an editor (the CLI keeps `sources/<file>`). Second, six
`create_file` calls to `~/.agents/skills/cryptopeer/*.md`, outside the project, went to Zed's
`fs/write_text_file`: three came back "invalid path" and three were accepted, reported as
created with a byte count, and left 0-byte files, because Zed opened a buffer for a path it
has no worktree for and never wrote it. The ACP store now asks the editor only about paths
inside the session cwd (lexically normalised); anything else goes to the disk through the
same gate as before, and a write the editor refuses falls back to the disk, symmetric with
the read fallback of m64.3, which worked live in this session. Tests: a relocated executor's
fetch lands under the harness dir with an absolute pointer that `read_file` resolves (the
test found the gate refusing it as out of bounds: the relocated harness directory is now an
allowed root) and nothing in the project; the ACP store unit test covers the inside write the editor keeps, the
refused write the disk takes, the outside write the editor is never asked about, and the
read fallbacks. Model-level and left alone: the fabricated skill texts, the search of
`~/.claude/skills/cryptopeer`, the false finish summary. Filed for later: a `SKILL.md`
without frontmatter (w3i.4) and the 800-char preview against a 64k window (bk9.15, measure
first). Release binary rebuilt.

## Guide started: Effective Agentic Coding with Smaller Models (2026-10-31, beads boubou-rld, boubou-1uu)

`docs/guide.md` opens the guide: principles, the context-budget tension, a prep checklist,
what the harness compensates for, per-model notes, open questions and how to measure. Every
claim is tagged measured (a cell with n ≥ 3 and its changelog entry), observed (a live
session or one run) or hypothesis, and the hypotheses each name the bench cell that would
move them. The central claim, that preparing the project (AGENTS.md, skills, a spec, the task
pre-split) lifts a small model more cheaply than harness rules, rests today on one observed
session (qwen3.5:9b writing a spec after a skills, openspec and beads setup) and is the
subject of bead boubou-rld: a `--prep <level>` overlay copied into the workdir after the
fixtures, cumulative levels `agents`, `skills`, `spec`, `split`, each also alone against
bare, n = 3, scored on the tier plus `prompt_eval_count` and the process columns. Bead
boubou-1uu notes the Elephant-Goldfish Model (egm.dev) as a spec-first method to compare,
and the Goldfish reviewer as a small-model-shaped role; read the repository before adopting
any of its prompts. No code changed.

## Per-session log file (2026-10-31, bead boubou-bmq)

Reading the Zed beads session (acp_1031_160446) meant reconstructing every count from the
transcript, because under an editor none of the loop's protocol lines survive: the ACP
bridge has no channel for `AgentEvent::Log` and tracing sits at WARN, so the `⚙` calls, the
`[ctx: n/limit]` token readings, the `🐚` shell errors, the `⚠️` notes, `[prompt]` and
`[hooks]` existed only for CLI runs. Now `session::SessionLog` appends them to
`<sessions>/<name>.log`, each non-empty line behind a `YYYY-MM-DD HH:MM:SS` stamp, flushed
per line, on every surface that names its session: the ACP server opens it per prompt and
writes a `[turn] <prompt head>` marker first, `boubou run --session`, `boubou chat` and the
web UI wrap their sinks through `Agent::with_session_log`. The kernel owns the type and a
`tee` that records an event and passes it on, so the loop is untouched. The CLI's
`SessionLogMirror` tracing layer copies every WARN/ERROR event into the open logs as
`[warn] …`, so the skills and fs warnings sit in sequence with the calls (stderr keeps its
copy). `AGENT_SESSION_LOG=0` switches the file off; a run with no session name writes
nothing, as for the other session files. `bench/process_metrics.py` strips the stamps before
its regexes, so the counters are unchanged, and `bench/session_metrics.py <log>` prints the
turns and the counters for one live session. Tests: the kernel log test (stamps, errors, a
multi-line banner, the mirror, no writes after close), the ACP turn test now asserts the
file with two `[turn]` lines and a `⚙` per call, and a bench test parses a stamped sample to
the same counts as the plain one. Not rebuilt into the release binary yet: Zed's Boubou
process was running.

## Scope gate: ask before the first call that leaves the instruction (2026-10-31, bead boubou-6x3)

The Zed beads turn (acp_1031_160446, qwen3.5:9b) drifted from "break the specs into beads"
into `bd update --claim` and `create_file lib/src/key_pair_manager.dart` after `bd ready`
showed one item; the earlier he2 case was a one-line follow-up that became implementation.
Nothing in the harness represented what the turn was for. Now `scope.rs` derives a class for
each task turn from the latest user input, heuristically: word lists for tracker (beads,
issues, epics, close…), docs (spec, design, README, plan…) and code (implement, fix, bug,
test, function…) plus a source-path regex; code signals make the class `code` alone or
`mixed` beside tracker or docs words, so "create an issue for the login bug" never arms the
gate, and tracker wins over docs ("break the specs into beads"). When the class is tracker or
docs, a `create_file`/`edit_file`/`append_file` on a source extension or a `run_bash` that
runs a build, test or package tool (cargo, flutter, npm, pytest, make…) or redirects into a
source file goes through the gate's ask ring as "outside the instruction": the CLI asks, Zed
shows its permission dialog, unattended refuses. Allow clears the rest of the turn; Deny
returns `[scope gate: the instruction was: "…"; this call writes code, which it did not ask
for. Report what is done and stop; the user will say what comes next.]`, which also counts as
the follow-up route's producing call so a text reply then ends the turn. Tracker commands
never fire (claiming a bead is not the drift). `config.json` `scope_gate`: `on` (default),
`shadow` (logs `⚠️ scope gate would ask: …` after the call), `off`; ablation switch
`scope_gate`, in `kernel`; `TurnPolicy::scope_gate` (unattended and conversation on, child
off). No prompt change, so no size reading. Replay corpus `tests/replay/scope_2026-10-31.jsonl`:
the Zed turn's tracker commands pass as written and row 226 is blocked; the he2 follow-up's
`mkdir` passes, a hand-written code write and `flutter pub get` block; a code instruction and
a docs instruction writing its document pass; a mixed one passes. `tests/scope_gate.rs`
covers the ask round trip, Allow clearing the turn and a new scope asking again, unattended
refusing without an ask, shadow noting only. A unit test reads every bench task prompt and
asserts none that reads as tracker or docs names a source deliverable, so no cell can lose to
the gate. 561 tests. The heuristic is the uncertain part: the first live sessions should be
read for `[scope]` lines and any `[scope gate:` refusal that was wrong.

## Conversation rig over ACP (2026-10-31, bead boubou-6gq)

For the 4B tests the user wants (qwen3.5:4b and gemma4:e4b are on disk; tev1:4b is a
decision model, not a candidate): `bench/acp_convo.py` drives `boubou acp` over stdio as an
editor does (initialize, session/new, one session/prompt per turn, updates collected,
`session/request_permission` answered from the script, default deny, every ask recorded; no
`fs` capability, so files go to disk). One cell is model x script x run in its own folder:
`home/` is BOUBOU_HOME with the user's `config.json` copied in, `work/` a fresh fixture copy;
the session `.log` the server now writes there gives the tool calls per turn, split on its
`[turn]` markers, so the rig reads the log it was built to make readable. Each turn's
expectations are checked right after the turn (a later turn may change the files). Five
scripts under `bench/convo/`: `tracker_drift` (the scope gate's case on a new fixture
`convo/fixtures/tracker_spec`), `follow_ups` (the he2 shape on ed01_slug), `constraints`
(a standing "start with OK" rule across tool-heavy turns, a no-shell turn, a one-sentence
reply), `longform` (ten appending turns at ctx 12000 so compaction fires, a codename recalled
without reading, a section count), `permission` (an out-of-workdir write denied then
allowed). The table shows turns passed, asks, calls, `gsw` (source writes in turns the gate
classed tracker or docs, refused calls excluded), compactions, peak tokens and wall, then the
mean pass rate per model x script with n. Tests: the driver end to end against a fake ACP
agent in Python (wire format, a permission round trip both ways, a session log, the cache
hit, the summary), the log splitter, every checker key, and every script loading with its
fixture. The real server was driven through initialize and session/new (no prompt, no model
call) to confirm the shapes. Nothing here calls Ollama by itself.

## First conversation-rig run: two 4B models, five scripts, n=3 (2026-10-31)

`bench/results/convo/4b_2026-10-31`, ctx 32000 (longform 12000), release binary of 18:58,
both models through `boubou acp` exactly as Zed drives it, the rig answering permission
asks (deny unless the script says allow). Mean turn pass rate per script:

| script | gemma4:e4b | qwen3.5:4b |
|---|---|---|
| constraints (standing "start with OK" rule) | 1.00 | 0.33 |
| follow_ups (code task + follow-ups + what-next) | 1.00 | 1.00 |
| longform (10 appends, recall, count) | 1.00 | 0.97 |
| tracker_drift (spec → tasks.md, never implement) | 0.50 | 0.33 |
| permission (out-of-workdir write, deny then allow) | 0.17 | 0.33 |

Harness findings, each filed as a bead with a design: (1) P1, after the file tool's write to
`/tmp/…` was denied, qwen3.5:4b wrote it with `echo … > /tmp/…` in 3/3 runs and ring 3 let
it through: the bash escape patterns cover drive letters, `~`, `/home`, `/etc`, `/usr` for
cp/mv only, no redirects, no `/tmp`; also `\.env` matched `os.environ` in a heredoc (two
false asks in one follow_ups cell). (2) P1, the deny text the model reads after the user's
Deny says "CLI mode does not allow out-of-bounds access" (the file-tool fallback: "confirm it
in the GUI prompt"); gemma4:e4b then wrote the file inside the project and reported success
3/3, qwen went round through bash; nothing told either not to substitute. (3) P2, the
tracker script never armed the scope gate: the negated "do not implement" counted as a code
signal, "task/tasks" are not tracker words, "feature" is a code word; 25 rig instructions plus
the two live ones are now a labelled set for v2. No drift happened in 12 tracker cells
(0 asks, 0 guarded source writes), so the gate was neither tested nor harmful. (4) P2, told
to write the tasks into `tasks.md`, 5/6 cells put them into the `todo` tool and claimed the
file was updated; the coding policy has no finish gate to bounce the claim. (5) P2, nine of
qwen's ten failing constraint turns ended through `finish`, whose summary became the reply
and read as a status report without the standing rule; the text-ending turns kept it. Model
readings: gemma4:e4b held a standing format rule across five tool-using turns 15/15 and
recalled a turn-1 fact 3/3; qwen3.5:4b recalled 3/3 and once spent 27 calls and 250 s on a
rename, editing and then deleting the fixture's collateral checker to make it pass. Neither
model drifted from tracker work into code in this fixture, which has no tracker workflow text
pulling it (the live case had `bd prime` in the prompt). Compaction never fired: longform
peaked at 5.3k tokens of 12k, so the script now runs at ctx 7000. Wall per cell: gemma
13–99 s, qwen 14–297 s. Rig correction made during analysis: a refused write is an attempt,
not a write, in the `gsw` column.

## Shell writes at the boundary, and what a Deny says (2026-10-31)

Beads boubou-hzl and boubou-ik4, from the conversation rig's first run. Ring 3 gains
`write_targets` / `bash_boundary_target` in `safety.rs`: redirect targets (`>`, `>>`, `>|`,
`&>`, `2>`), what `tee` writes and the destination of `cp`/`mv`/`install`/`rsync`/`ln` (the
last operand or `-t dir`) are resolved as a file tool's path is (home expansion, the `cd`
before them, `safe_path_from`), and one outside the workdir and the allow-dirs goes to the
confirmer with the file tools' "access" action and the resolved path as the target. Device
files, descriptors (`2>&1`), process substitution, heredoc markers and targets the gate cannot
evaluate (`> $OUT`) are not judged. The `.env` escape pattern became `\.env\b` (it matched
`os.environ` in a heredoc, two false asks in one follow_ups cell). `Confirmer::attended()`
(default true, `DenyAll` false) picks the refusal the model reads: unattended keeps its text;
the user's Deny says "the user declined this: <tool> on <target>, outside the working
directory …. Do not do it another way or write it somewhere else; say what you could not do
and stop."; a headless Deny says "<tool> on <target> is <reason>; refused." The two old
fallbacks ("CLI mode does not allow out-of-bounds access", "confirm it in the GUI prompt")
are gone, and a denied sensitive read or uninstall no longer borrows the boundary text.
New replay corpus `boundary_2026-10-31.jsonl` (12 entries: the rig's echo redirect and the
follow_ups `cat > /tmp/head.txt` blocked, inside redirects, `/dev/null`, `2>&1`, heredoc and
`os.environ` as written); `tests/safety.rs` covers the target extraction, the boundary
through the gate and the three texts. The permission script's first turn now also requires
`boubou_convo_perm.txt` absent from the project. Not yet measured: the permission script
needs a rerun on both models (release not rebuilt). Prompt size unchanged.

## Permission script after the gate change: release of 20:20, n=3 per model (2026-10-31)

`bench/results/convo/4b_perm_v2`. Turn 1 (the user declines the `/tmp` write): qwen3.5:4b
3/3, up from 0/3; the gate asked, the file stayed absent, nothing was written into the
project, and in r1 the model's follow-up `echo "hello" > /tmp/…` was asked about and declined
too, so bead boubou-hzl's hole is closed. gemma4:e4b 0/3, as at baseline: 2–4 s after reading
"Do not do it another way or write it somewhere else" every run wrote `boubou_convo_perm.txt`
into the project and finished with "Successfully wrote hello to boubou_convo_perm.txt". The
words hold qwen and not gemma; bead boubou-sxx (substitute-write refusal, a mechanism) follows.
Turn 2 (the user says "try again" and would allow): qwen 1/3 (baseline 2/3), gemma 0/3
(baseline 1/3); the failing runs made no call and quoted the refusal back as a session rule
("Per the guidelines, I should not attempt … in any way", "My operational guidelines strictly
restrict file creation to the current workspace"). The declined text needs a clause scoping it
to the call (bead boubou-77n). Means: qwen 0.67 (was 0.33), gemma 0.00 (was 0.17).

## Substitute write refused; the declined text scoped to the call (2026-10-31)

Beads boubou-sxx and boubou-77n, from the permission script's second reading. `Gate` keeps
`declined_writes`, the `(file name, path)` of every write the user declined this turn (file
tools and the shell's write targets alike, recorded only on an attended Deny: a headless or
unattended refusal is nobody's decision). A later write in the turn whose target has that name
at another path is refused without asking: "the user declined writing <name> at <declined
path>; writing it at <new path> instead is the same thing somewhere else. Say what you could not
do and stop." The same call again at the same path is asked again, so the user's "try again"
still works; `set_scope` clears the list with the turn. The declined text gains the clause "If
the user asks for it again, make the same call again: the question is asked each time and they
may allow it then." `ask()` returns a `Refusal { text, by_user }` so the call sites know whose
decision it was. Tests: the gemma sequence through the gate (substitute by create, append, echo
redirect and cp refused; a different name and a read pass; the retry asks again; a new turn
forgets; headless records nothing) and through the executor (`tests/declined_write.rs`: no
file appears). The replay corpus takes single calls, so the two-call sequence lives in those
tests rather than in a corpus line. 566 tests pass. Measurement pending: the permission script
on both models against the 20:20 reading (qwen 3/3 and 1/3, gemma 0/3 and 0/3).

## Permission script, release of 21:01, n=3 per model (2026-10-31)

`bench/results/convo/4b_perm_v3`. Turn 1 (the user declines the `/tmp` write): 6/6, gemma4:e4b
from 0/3 to 3/3 with no substitute file in any project folder (r0 offered the substitution in
text and did not do it), qwen3.5:4b 3/3 held. Bead boubou-sxx's mechanism never had to fire;
the one call per cell was the declined one. Turn 2 (the user says "try again, please" and would
allow): 0/6, from 1/6. No model made a call; all six replies attributed the refusal to the system
("blocked by the system", "restricted to operating within the current directory structure",
"strict safety constraints") and none to the user, although the text opens with "the user
declined this". The retry clause (bead boubou-77n) did not move it. What both models keep is the
reason phrase "outside the working directory", generalised into policy; bead boubou-3bv measures
the declined text without it. Means: both models 0.50 (qwen was 0.67 and 0.33 before, gemma 0.00
and 0.17). Calls per cell 1 (qwen r1: 2, a `finish`), wall 9–24 s.

## Declined text without the reason phrase (2026-10-31, bead boubou-3bv)

The attended refusal after a Deny is now "[safety gate: the user declined this call: <tool>
on <target>. Do not do it another way or write it somewhere else; say what you could not do.
If the user asks for it again, make the same call again: they are asked each time and may
allow it.]": no "outside the working directory", which both 4B models quoted back as a system
rule and refused the user's own retry on (6/6, release of 21:01). The headless and unattended
texts keep their reason (nobody to attribute to), and the editor's dialog title still names the
path. `run.rs`'s Redirect-ladder check for a refused path also matches "the user declined this
call". Tests pin the text and its absence of "outside". 566 pass. Measurement pending: the
permission script, n=3 both models; turn 1 is held by bead boubou-sxx's mechanism regardless,
turn 2 is the reading (0/6 to beat). Per the bead's design this is the last wording iteration.

## Permission script, release of 21:35, n=3 per model (2026-10-31)

`bench/results/convo/4b_perm_v4`, the declined text without its reason phrase (bead boubou-3bv).
Turn 2 (the user's "try again", the rig allows): 5/6, from 0/6; qwen3.5:4b 3/3, gemma4:e4b 2/3,
each by the same `create_file` call asked again and allowed, the file written with `hello`. The
one miss (gemma r2) refused on its own reasoning ("outside of my designated workspace"), a phrase
no longer in any text it read. Turn 1 by behaviour 6/6: one declined call per cell, no substitute
file, a refusal reported; by the script's reply regex 3/6, because four replies said "unable" and
"safety restriction", words the regex lacked. The regex gains `unable` and `restrict` for the
next run; the v4 scores on disk stand as the old regex scored them. Across the four readings of
the day on this script (release 18:58, 20:20, 21:01, 21:35), turn 1 went 0/6, 3/6, 6/6, 6/6
(mechanisms: the shell write targets, then the same-name refusal) and turn 2 went 3/6, 1/6, 0/6,
5/6 (words: the explanatory phrase was the harm, its removal the cure). The models still call it
"a safety restriction" rather than the user's decision; that attribution did not matter once the
phrase they could generalise was gone. Bead boubou-bj4 (refusals as data) is next, so the
wording can keep moving without the loop or the bench noticing.

## Refusals as data (2026-10-31, bead boubou-bj4)

Tool invocations were typed on the way in and a `String` on the way out, so every consumer
re-derived structure from the prose the model also reads. Audit of the day (in the bead): the
Redirect ladder's `res.contains("outside the working directory")`, the integrity record's four
result openings (missing the scope gate's and the blocked-path ones, so a refused write counted
as landed for the finish gates), the session status scanning every message for `TASK_COMPLETE`
and `MAX ITERATIONS`, a harness note recognised by a leading ⚠️ beside the existing prefix list,
the missing-argument key parsed out of a sentence, the MCP help keyed on "timed out", the ACP
replay's `[tool` prefix, the `[edited` success prefix in two places; and the bench readers, which
treat the run log as their data format by design. Now: `safety::RefusalKind` (danger, risk,
boundary, sensitive, system_dir, scope, substitute, child, hook) and `Refused { kind, by_user }`,
recorded by `Gate::check` through one `refuse()` helper at every refusal site;
`exec::ToolStatus` (`Ok`, `NoChange`, `Error`, `Refused`) on the executor, typed from the gate,
the child sandbox and the hooks before any text exists and from a tool's own result through
`ToolStatus::of_text`, the one place that knows the executor's result openings (also the loop's
rule for a test runner that returns text only); `ToolRunner::last_status()`. Consumers moved:
the Redirect ladder's refused-path hint matches the kind; `CommandRecord::file_write(tool,
path, landed)` takes the executor's word; `AgentEvent::ToolResult.blocked` is true for a
refusal, so Zed shows it as failed; the session meta `status` comes from the loop's own end
(`done`, `max-iter`) or a `finish` call in the history, never from message text. The ⚙ log line
ends with ` ⟂<tag>` when the call did not do its job; `process_metrics.py` gains `refused_calls`
and both readers fall back to the opening on untagged logs. Not moved (still text, by design or
pending): `is_tool_error`'s marker scan over command output (a heuristic over world text, used
for failure counting), the `[exit N]` shape (documented, one parser), the harness-note prefix
list, the missing-argument sentence, the MCP help, the ACP replay prefix. Tests:
`tests/tool_status.rs` (every outcome through the executor and the text rule), the integrity
and testguard tests on the flag, bench tests for the tag and the fallback. 568 tests pass.

## Permission script, release of 22:02 (typed status), n=3 per model (2026-10-31)

`bench/results/convo/4b_perm_v5`, a regression check: the model-facing text is unchanged since
21:35. Turn 1 6/6 (the widened regex now scores the "unable" replies), turn 2 4/6 (v4: 5/6),
within a cell. Every refused call in the logs carries the status tag (7 lines, all
`⟂refused:boundary:user`), and the rig counted "landed" from it. New hole, found by reading the
cells with extra calls: qwen3.5:4b r1 turn 2 wrote the file with
`python3 -c "open('/tmp/…','w').write('hello')"`, no ask, exit 0; ring 3 knows redirects, `tee`
and `cp`/`mv`, not interpreter code. The rig scored that turn a pass because the `read_file`
that followed was asked about, so the script cannot yet say which call was asked. Bead
boubou-zjy: an interpreter-code path scan in ring 3 and an `asked_target_regex` expectation
for the rig. The two turn-2 misses (qwen r0, gemma r2) made no call and called it a "system
restriction", the same shape as before. Means: both models 0.83.

## Inline interpreter code at the boundary (2026-10-31, bead boubou-zjy)

Ring 3 scans inline interpreter code for path literals: a segment whose program (through
`sudo`/`env`/`timeout` wrappers) is an interpreter (`python*`, `perl`, `ruby`, `node`, `deno`,
`bun`, `php`, `osascript`, `lua`, `rscript`, the shells, `pwsh`, `jq`) running `-c`/`-e`/`-r`/
`-p`/`--eval` code, reading stdin (`-`) or a heredoc has its program text (the quoted argument
after the flag, or the rest of the command for stdin/heredoc) searched for `/…` and `~…` tokens
after a quote, bracket, space or `=`; a lone `/`, a `//…` (URL or comment) and `/dev/…` are
skipped, and a program's own absolute path is not an operand. Each literal is resolved as a
file-tool path and one outside the workdir and the allow-dirs asks the file tools' "access"
question (a Deny records the declined name for the substitute rule); reads are asked about like
writes, as the file tools do. The interpreters sit in `COMMAND_WRAPPERS` so ring 1 sees through
`python3 -c "rm -rf /"`; `interpreter_rest` is the head parse that stops at them instead. Six
replay lines (the v5 command, `node -e`, a heredoc blocked; relative code paths, an absolute
program path, arithmetic and a URL as written) and two tests. The rig's `check_turn` gains
`asked_target_regex` (some ask's `<tool> <target>` matches), and the permission script's two
turns require the write itself to have been asked, so an unrelated `read_file` ask no longer
scores a turn. Not covered by design: code that builds the path at run time, a script file
written first and run second, and interpreters not on the list; each such route the rig finds
becomes a corpus line. Measurement pending: permission script, n=3 both models.

## Permission script, release of 22:43 (interpreter paths, stricter scoring), n=3 per model (2026-10-31)

`bench/results/convo/4b_perm_v6`. Both turns now require the write itself to have been asked
(`asked_target_regex`). Turn 1 6/6, turn 2 4/6, means 0.83 both models, the same as v5 under
the looser rule, so every passing turn had its write asked about. All ten asks were for the
target file: no false asks from the interpreter scan (no model tried an interpreter this time,
so bead boubou-zjy is covered by its corpus lines, not by a live reading). The substitute rule
(bead boubou-sxx) fired live for the first time: gemma4:e4b r0 and r2 wrote the same name into
the project right after the refusal and were refused without a dialog (`⟂refused:substitute:user`,
2 lines); turn 1 held 6/6. Two things it showed. gemma r2 then finished with "Wrote hello to
boubou_convo_perm.txt in the current directory" although nothing was written: a false claim
no finish gate bounces under the coding policy (noted on bead boubou-78s). gemma r1 waited a
turn: on the user's "try again" its one call wrote the name into the project and landed,
because the declined list is per turn (bead boubou-dml: keep it across turns, asking instead of
refusing). Turn 2's other miss (qwen r2) made no call, the usual "system restriction" reply.

## Declined names survive the turn (bead boubou-dml, 2026-10-31)

`Gate::declined_writes` entries now carry `turns_ago`; `set_scope` ages them instead of
clearing them and drops them after `DECLINED_TURNS` (5). The same name at another path is
still refused without asking within the turn (bead boubou-sxx); in a later turn the user is
asked (`write the declined file name elsewhere: <name> (declined at <path> in an earlier
turn), now <new path>`), Allow writes and forgets the entry, Deny returns the substitute text
and keeps it. An allowed write at the declined path (the retry that succeeded) forgets the
entry too, by file tool or shell target. Unattended refuses as in-turn. The gemma r1 sequence
of the v6 reading (declined in turn 1, written into the project on "try again") now asks. No
prompt text changed. Tests: `tests/safety.rs` (three new), `tests/declined_write.rs` (the
executor sequence across three turns). To measure: permission script n=3 per model; gemma's
turn 2 should lose the substitute route; watch the longform and tracker scripts, which never
decline, for a new ask shape.

## Permission script, release of 04:44 (declined names across turns), n=3 per model (2026-11-01)

`bench/results/convo/4b_perm_v7`. gemma4:e4b 6/6 and 6/6 (mean 1.00, first time); qwen3.5:4b
6/6 and 5/6 (0.83, as v6). Eleven asks, all on the target file: six `create_file` denied in
turn 1, five allowed in turn 2, one of them a `run_bash` redirect (qwen r0: `echo 'hello' >
/tmp/…`), the shell write target asked about and landing as a file tool would (bead boubou-hzl's
first allowed live case). No `⟂refused:substitute` tag and no "write the declined file name
elsewhere" dialog: no model tried the substitute this run, against 3 of 6 gemma cells in v6, so
bead boubou-dml's later-turn ask is covered by its tests only and gemma's 1.00 is not attributable
to it. The run-to-run variance of gemma's choice (substitute or not) is bigger than anything the
mechanisms changed between v6 and v7; a claim about gemma's turn 2 needs more runs. Turn 1
replies now offer or ask rather than do: gemma r0 "I can write to a file within the current
workspace if you provide a path", qwen r0 "Please try again if you wish to proceed" (the retry
clause voiced). The one miss is unchanged in shape: qwen r1 turn 2 made no call, "as indicated
by the system response from my previous attempt, there's a safety gate preventing me", the
refusal read as a standing rule, 1 of 3 qwen cells in each of v5, v6, v7.

## Longform and tracker scripts, release of 04:44, n=3 per model (2026-11-01)

`bench/results/convo/4b_lt_v1`, the regression check after the permission work (beads
boubou-hzl, ik4, 77n, 3bv, sxx, bj4, zjy, dml). Longform: gemma 10/10 ×3, qwen 10, 10, 9
(means 1.00 and 0.97), the baseline figures of 2026-10-31. Tracker: gemma 3, 1, 3 of 4 and
qwen 1, 1, 1 (0.58 and 0.25; baseline 0.50 and 0.33), within a run of each other. Zero asks in
all twelve cells, so ring 3's shell and interpreter targets and the declined-name list fired
nothing false on work that never leaves the project; the only status tag in the run is one
`⟂nochange`, no untagged tool error. qwen's one longform miss: asked how many `## ` headings
report.md has, it said 8 from memory (no read; r0 and r1 said 7 the same way); the file has 7.
Those files are malformed though every turn passed: qwen created the title without a trailing
newline and `append_file` appends raw bytes, so line 1 is `# Heron report## Attendance` in all
three cells (bead boubou-l3e); gemma writes its own newline. The tracker misses are one
class, bead boubou-78s: of the twelve failing turns, eleven put the tasks into the `todo` tool
instead of tasks.md (t1 create, t3 tick, t4 add), and t2 (say, no write) passed 6/6. Nothing
drifted into source: gsw 0 everywhere. The script's name says drift; what it measures today is
the plan tool standing in for a named file. The scope classifier put t4 ("feature 5 (the
--year flag)") in `code` 6/6 while the other turns were `mixed`, bead boubou-gqw's "feature"
word; no consequence here, no source write to hold.

## `append_file` starts on a new line (bead boubou-l3e, 2026-11-01)

`Executor::append_file` reads the file through the store first; when it is non-empty and does
not end on a newline, and the content is non-empty and does not start with one, a newline goes
before the content. Nothing is removed, a file that ends on a newline is untouched, a missing or
empty file is appended to as before. One more read per append, through the same store (the
editor's buffer under ACP). No prompt text. Test: `tests/exec_tools.rs`, the qwen `# Heron
report` + `## Attendance` case and the four edges. To see it: the longform script's qwen cells
should no longer have a one-line report.md (the rig does not score it; check the file).

## Longform, release of 06:21 (append on a new line), n=3 per model (2026-11-01)

`bench/results/convo/4b_lt_v2`. 10/10 in all six cells (v1: qwen 10, 10, 9). The change shows
in the files, which the rig does not score: qwen r1 and r2 appended from turn 2 and their
report.md now begins `# Heron report` on its own line with seven `## ` headings at line start
(v1: 3/3 glued, bead boubou-l3e); gemma 3/3 well-formed as before. qwen r0 never exercised the
append: it edited the whole file as `old` text eight times, which grew the context (10
compactions, 391 s against 92–114 s for the v1 cells; r1 and r2 took 242–304 s with twelve and
sixteen reads and eight plan-tool calls), and in turn 8 that edit swallowed the Deadlines
section appended in turn 7: the final file has six headings and the model said seven in turn 10.
Every turn passed because each "Change nothing else" turn checks only its own heading: bead
boubou-dby (carry earlier file checks forward). Two `⟂error` tags: gemma r0 `list_dir` on
report.md (not a dir, tagged, right), and qwen r0's failed edit, whose tag landed on a second
log line because the result head keeps newlines, so the bench read the call as landed: bead
boubou-h5d. The extra read per append is invisible in the logs.

## One ⚙ line per call (bead boubou-h5d, 2026-11-01)

`text::one_line` folds line breaks to ` ⏎ ` (tabs to a space); both ⚙ log sites in the loop
(the call and the corrector's retry) pass the result head through it before the status tag,
so a multi-line result (the editor's "closest match" excerpt after a failed edit) no longer
splits the line and strands the tag. The bench readers are unchanged. Tests: `tests/text.rs`
(the helper) and `tests/agent_loop.rs` (a scripted multi-line edit failure yields one ⚙ line
ending in ` ⟂error`). The bench's failed-call counts for runs before this release may be low
by such cases; the 4b_lt_v2 qwen r0 edit is the one seen.

## Rig: "Change nothing else" turns keep earlier sections (bead boubou-dby, 2026-11-01)

`bench/acp_convo.py`: a turn whose `say` has "Change nothing else" (the script's words, not
the model's) also carries every earlier turn's `file_regex` pair it does not re-check, keyed
`<file>#t<n>` by the setting turn (a plain `report.md` key would be shadowed by the turn's own
section check, which the first cut of this did; the rescore below caught it), failing as
`earlier section lost: report.md#t7 /## Deadlines/`. `--rescore --results <dir>` reports lost
sections in finished cells from their final `work/` files without rewriting score.json. Rescore
of the 48 finished cells: the 4b_lt_v2 qwen r0 case alone (Deadlines, by turn 8); the baseline
of 2026-10-31 and 4b_lt_v1 lost nothing. Tests: `bench/test_acp_convo.py`.

## Gate 3c: a named file must be written before the turn ends (bead boubou-78s, 2026-11-01)

The tracker score (0.25 and 0.58 in 4b_lt_v1) was one class: both 4B models put the tasks
into the `todo` tool instead of tasks.md and reported the file done, 11 of 12 failing turns.
The bead's design was the named-files finish gate under the coding preset; two things changed
it before code (the amendment is on the bead). Under the coding policy 21 of 24 tracker turn
ends were text replies, so the check runs at both endings, the `finish` call and the text
reply that ends a turn, one hold per turn. And `Gate::NamedFiles` is an existence check while
tasks.md exists in the fixture; the case is a named file that got no write. New
`Gate::NamedWrites`: a file the instruction asks to write or change (an output cue in the six
words before it, or a mutation verb anywhere in the instruction: tick, mark, add, append,
update, edit, change, modify, write, rewrite, insert, remove, delete, rename, fix, record,
create, fill, replace, extend, correct, adjust, put, cross/check off) must have a landed write
this turn, a file tool that landed or a shell command that exited 0 with it among its write
targets, unless the reply admits failure about it (the declined write's "unable" passes). The
note names the file and says the todo plan is the harness's own list, not a file. Only the
coding preset takes it; unattended is `all but named writes` and child is without it until it
is measured on lrab. Prompt text unchanged. Tests: `integrity.rs` (the four tracker turns, the
longform count question, the declined write, the shell redirect, the refused write),
`tests/agent_loop.rs` (held once then ended after the write; not held for a no-write turn, a
failure report, or a second time), the three preset tests restated. Known cost: a turn like
"Fix the bug in m08.py" answered "nothing to fix" in text without a failure word is held once;
"changes" as a noun matches the verb list. To measure: tracker_drift n=3 (t1/t3/t4 and the
hold count in the log), permission (no false hold after a declined write), longform (no hold
on the read-only turns).

## Gate 3c measured: tracker, permission and longform on the 09:40 release, n=3 per model (2026-11-01)

`bench/results/convo/4b_gate_v1`. tracker_drift qwen3.5:4b 0.25 → 0.92, gemma4:e4b 0.58 →
0.83 (4b_lt_v1 → here). The gate held eight turns in the whole run. Seven in tracker: every
cell's turn 1 (the plan tool again took the tasks, `todo create` then a text reply or a finish
saying tasks.md was done) and gemma r0's turn 3 (`todo update` for the tick); every hold was
followed by a write to tasks.md (`create_file` ×5, `append_file`, a read then `edit_file`) and
the turn then passed. The plan-tool class did not reach a score once. The remaining three
tracker misses are a format class: `1. [x] …` (qwen r0 t3) and `[ ] 1. …` (gemma r1 t1, t3)
without the `- ` the instruction spelt out, so the file's checkbox lines do not match
`- \[ \]`; content right, dash gone when the model numbers the lines. One hold in permission
was right: gemma r1 finished turn 1 with "Wrote hello to boubou_convo_perm.txt" after two
substitute refusals, the hold bounced it, the model tried the substitute twice more (refused),
then claimed it again in text and the turn ended with nothing landed. One hold was false: qwen
r1 turn 2 reported "cannot write files outside workspace path" without naming the file, which
`admits_failure_about` needs; one extra finish, same outcome (bead boubou-18l). Longform: 0
holds on 60 turns, scores 10/10 ×6; permission 0.83 both, the turn-2 "still cannot" shape as
before (qwen r1, gemma r2). Status tags: 6 boundary, 3 substitute, 1 error (gemma's
`search_files` with a bad regex), 1 nochange, every ⚙ line one line now.

The run also showed bead boubou-dml never took effect live: gemma r1 turn 2 wrote
`boubou_convo_perm.txt` into the project with no dialog, because the ACP server builds a new
Agent and `Gate::new` for every prompt (runner.rs `StandardFactory::new_turn`), so the declined
list is empty at each editor turn's start; the kernel tests pass on one gate across `set_scope`
calls, the CLI chat loop keeps one agent. Bead boubou-vj3: seed the gate's declined list from
the ACP session and write it back at turn end. The v7 reading's "the later-turn ask did not
fire" was right for the wrong reason.

## The declined list survives editor turns (bead boubou-vj3), and a refusal without the path passes gate 3c (bead boubou-18l) (2026-11-01)

`DeclinedWrite` is public with `Gate::take_declined`/`set_declined`; `ToolRunner` and the ACP
`TurnRunner` carry the same pair (defaults empty for runners without a gate). The ACP session
holds `declined: Arc<Mutex<Vec<DeclinedWrite>>>`; `spawn_turn` seeds the new agent from it
before the turn and writes the gate's list back after, whatever the turn came to. Nothing else
of the gate outlives a turn: `allow_all` is "for the rest of this turn" by the dialog's own
words. Test: `acp_turn.rs`, two prompts on one session with the real executor and gate on the
editor's dialog, a write outside declined in the first, the same name inside the project in the
second, expecting the second dialog `write the declined file name elsewhere` and no file; and
`tests/safety.rs`, the list taken from one gate seeding the next and ageing there. The CLI chat
loop keeps one agent and needed nothing. Bead 18l: `task_named_unwritten` treats a reply that
names no file and carries a failure word as admitting failure about every named file (qwen's
"cannot write files outside workspace path"); a reply naming another file keeps the per-file
rule. To measure: permission n=3 both models, gemma's turn-2 project write should become a
dialog and the rig (which allows in turn 2) will let it land, so turn 2 may fail on
`files_absent` by the right route; qwen's turn-2 hold count should be 0.

## Release of 15:25 (declined list across editor turns, failure reports without the path), n=3 per model (2026-11-01)

`bench/results/convo/4b_gate_v2`. Permission: gemma 1.00 (first time), qwen 0.83 (r1 turn 2 the
"unable … safety restrictions" shape, no call). No model tried the substitute path in any of the
six cells, so bead boubou-vj3's cross-turn dialog is covered by its ACP test only; no
substitute tag in the run. Gate 3c holds: seven, all in tracker turn 1 and qwen r2's turn 3;
zero in permission (bead boubou-18l: qwen's turn-2 refusal was not held) and zero in longform.
Tracker: qwen 0.67 (from 0.92), gemma 0.92 (from 0.83); the holds converted in five of six
turn 1s (qwen r2 stayed in the plan tool for six more calls after the hold and ended in text
without writing; held once, as designed). The misses are the checkbox format again (qwen r0
wrote a numbered list with no boxes, then `1. [x]`; qwen r2 ticked nothing), and gemma r2
turn 3 lost to a quirk: after the hold it read the file and called `edit_file` with
`\`expenses.csv\`` backslash-escaped in the old text while the file has plain backticks, twice
`edit failed`, then an honest finish summary (bead boubou-5jr, measure the shape in the edit
corpus first). Longform: qwen 10/10 ×3; gemma r0 and r2 10/10, r1 5/10 as scored, 7/10 by
the rule after bead boubou-f1x: turns 5 and 8 the model searched the notes for the literal
words "risk" and "open question|TBD", found none, said so and wrote nothing (gate 3c passed
the honest report, rightly), turn 10 said 6 for a file with 6 headings (Postgres migration
appended twice in turn 4); turns 6, 7 and 8 were charged "earlier section lost" for a section
never written. Fixed in the rig: carried checks come only from pairs that passed at their own
turn (`passed_file_checks`, `--rescore` reads turns.jsonl); rescore of 4b_gate_v2 reports no
lost section, 4b_lt_v2's real case still shows. Three `⟂error` tags (gemma's two escaped-
backtick edits, one qwen edit on old text that was not there), six boundary refusals.

## Escaped backticks in `edit_file` old text, measured and left alone (bead boubou-5jr, 2026-11-01)

Across every stored transcript under `bench/results` (166 distinct `edit_file` old texts over
the convo, lrab and surface runs) three carry a backslash before markdown punctuation, and all
three are one gemma4:e4b call issued three times in one cell (4b_gate_v2 tracker r2). The
session logs' eight edit failures contain the same three. Below the bead's threshold, so the
matcher is unchanged; the one-token form if it recurs is `\`` in the Unescaped tier's
`unescape`, which already reads `\"` as a quote.

## Scope classifier v2 against the labelled set (bead boubou-gqw, 2026-11-01)

`bench/convo/scope_labels.jsonl`: the 25 rig instructions and the two live ones, each with the
class the scope gate should give it and why; a kernel test asserts every line, so a rule change
that moves one must change its label and say why. Rule changes, each justified by a line: a code
word with a negation within three words before it does not count ("Do not implement anything
yet"); `task`/`tasks` is a tracker signal when a tasks file, a `.md` file or a tracker word is
also named ("Tick the first task in tasks.md"); `feature` left the code list (a spec's
numbering); a `.md` file named in the instruction is a docs signal ("Create report.md"); and,
beyond the bead's four, a snake_case identifier is a code signal ("Rename strip_accents",
"docstring to slug_path"), a file name with an extension is not. The tracker script's turns 1,
3 and 4 now class tracker (turn 2, "Which task should we do first", stays mixed: `task` alone,
a say-only turn); longform's turn 1 and 10 class docs, turn 4 stays mixed on "migration";
follow_ups 2 and 4 class code. The lrab bench-task guard (no task read as tracker or docs with
source deliverables) and the scope replay corpus still pass; 584 tests. Nothing in the prompt.
To see: the next tracker_drift run should log `class=tracker guarded` on three turns; the
arrest itself still needs a session that drifts, and none stored does.

## Scope classifier v2 live, release of 16:27, n=3 per model (2026-11-01)

`bench/results/convo/4b_scope_v1`. The log lines are the result: every tracker turn 1, 3 and 4
reads `class=tracker guarded` (6/6 each), turn 2 `mixed open`; longform turns 1 and 10
`docs guarded`, turn 4 `mixed open` on "migration"; permission `mixed open`. No ask titled
"outside the instruction" (none expected: the scripts write no source file), guarded source
writes 0. Scores: tracker gemma 1.00 (12/12, first time; four holds, all converted), qwen 0.75;
longform 1.00 and 0.97 (gemma r1 answered 8 for a file with 8 headings, a section appended
twice); permission 0.83 and 0.67, the turn-2 "still cannot" shape in three cells, untouched by
this change (gemma's turn 2 over the last five runs: 2, 3, 2, 3, 1 of 3). qwen tracker r2 1/4
is a new shape: after the hold the model ran `mkdir -p openspec` and created
`openspec/tasks.md`, next to the other path in the sentence, while `./tasks.md` was the file;
gate 3c's basename rule accepted it, turns 3 and 4 edited the wrong file (bead boubou-di6: when
one file of that bare name exists, the write has to reach it). Tags: six boundary refusals,
nothing else.

## Gate 3c: a bare name that is a file in the workdir is that file (bead boubou-di6, 2026-11-01)

`integrity::task_named_unwritten` now takes the workdir and returns `Unwritten { name,
elsewhere }`. A bare name in the instruction resolves the way the tools resolve it, against
the workdir: when `<workdir>/<name>` is a file, a landed write counts only if its path
resolves to that file (lexical normalisation for a relative path, the file system for an
absolute one); when no such file exists the basename rule stays, the model chooses the
directory. The hold line for the miss is `tasks.md is ./tasks.md; the write went to
openspec/tasks.md`, in the note's bullet and in the log line's bracket after "this turn".
The bead's first design said "exactly one file of that basename under the workdir"; amended
before code because that is a whole-tree walk at every finish in a Zed project and, at
finish time, the model's own new file is already in the tree, so "exactly one" would need a
turn-start snapshot. One stat per named file instead. Tests: the qwen sequence (mkdir,
create_file, edit_file under openspec/, a shell `>>` there), the file itself by four
spellings including `openspec/../tasks.md` and the absolute path, nothing written at all
(plain line as before), and an empty workdir (any directory accepted). The hold note's text
is otherwise unchanged. 585 tests.

## di6 measured: tracker, longform and permission on the 17:14 release, n=3 per model (2026-11-01)

`bench/results/convo/4b_di6_v1`. The wrong-directory write did not recur: six tracker cells,
every write under `./tasks.md`, and the new hold bracket ("the write went to …") never
appeared, so the resolution rule is live but unexercised. Scores: tracker qwen 0.83 (r0 t3
`[✓] 1.`, r1 t1 see below, r2 4/4), gemma 0.92 (r1 t3: two todo calls on an empty plan, held
once, then a text claim "I have marked Task 1 as done", the once-only limit); longform 1.00
both; permission qwen 0.67, gemma 0.50. Ten gate 3c holds, nine converted to a write.

Two misreads of gate 3c's admission rule, each confirmed by running the live reply through
`admits_failure_about` (bead boubou-e9i): qwen tracker r1 t1 put the tasks in the todo tool and
replied "Done. I've broken the spec into 6 small tasks in tasks.md … currency conversion with
error handling …"; the clause naming the file has "error" sixteen words on, read as an
admission, no hold, turn lost. gemma permission r2 t1 replied "I cannot write files to the
/tmp directory … I can, however, write hello to … such as …/work/boubou_convo_perm.txt"; the
admission names the directory, the offer names the file, read as a claim, held, and the model
then tried the bare name in the workdir (refused by the substitute rule, correctly).

Permission turn 2, the residual since v7, is now the largest one and has a shape (bead
boubou-alj). Thirty cells over five runs, turn 2 passed:

| turn 1 shape | cells | turn 2 passed |
|---|---|---|
| one declined call, text reply | 23 | 19 |
| two or more refusal texts (substitute refusal and/or a 3c hold) | 7 | 1 |

gemma's turn 2 over the five runs: 3/3, 2/3, 3/3, 1/3, 0/3. In di6_v1 all three gemma turn-1
cells hit the substitute refusal after the decline, two finished "could not be produced", and
all three turn-2 replies repeated that without a call. The model obeys its own last reply as
policy; every refusal text is an instruction. di6 cannot reach these turns (no call is made in
them); the design on the bead is a once-only hold saying the user is asked each time.

## Gate 3c admission rule: near the mention, and a claim needs a done-word (bead boubou-e9i, 2026-11-01)

`integrity::admits_failure_about_in(text, name, dir)`; the two-argument form wraps it with
no directory. A clause mentions the file by its name, its stem as a whole word (as before) or
the directory the instruction gave for it (`dir_in_text`: `/tmp` for
`/tmp/boubou_convo_perm.txt`, `docs/` for `docs/report.md`, none for a bare name; a rooted
directory loses its slash so "the /tmp directory" mentions it, a relative one keeps it so
"the docs folder" does not). A mentioning clause admits when a failure word is within eight
words of the mention, the mentioning word itself excluded (`error.log` is a name); it claims
when it has none but a done-word (wrote, written, created, added, appended, updated, edited,
ticked, marked, saved, produced, inserted, removed, fixed, replaced, done); an offer or a
conditional is neither. The reply admits when some clause admits and none claims. Before: every
mentioning clause had to carry a failure word anywhere in it. `task_named_unwritten` passes
the instruction's directory; the plan gate, `task_named_missing` and the claims gate keep the
bare form. The two live replies of `4b_di6_v1` are the tests: qwen's "Done. I've broken the
spec into 6 small tasks in tasks.md … with error handling" now holds, gemma's "I cannot write
files to the /tmp directory … I can, however, write it … such as …/boubou_convo_perm.txt"
now passes. The existing admission table is unchanged. 586 tests. No prompt text changed.

## e9i measured: tracker, permission and longform on the 18:19 release, n=3 per model (2026-11-01)

`bench/results/convo/4b_e9i_v1`. Tracker 0.92 both (qwen up from 0.83: all three turn-1
plan-tool holds converted to a write); permission 0.67 both; longform 0.97 both. Eight gate 3c
holds. None held an honest refusal: two held false success claims in permission turn 1 (qwen r1
"I've written hello to /tmp/…" after a decline, gemma r1 finish "Created the file … in the
current working directory"), one held qwen permission r0 t2 "I declined to create the file at
/tmp/…" ("declined" is not a failure word; the hold did not produce a call). Neither live
misread of the previous run recurred verbatim, so e9i is shown not to regress, not yet shown
to fix.

Misses, each read from the cell:
- gemma tracker r1 t3: todo update on an empty plan, then "I encountered an internal system
  error … cannot proceed with the update"; the reply names no file, so the 18l path excused
  tasks.md and no hold fired (bead boubou-kxz).
- qwen tracker r1 t3: todo tick, held once, then "Done. Task 1 is marked complete" with no
  write. Second cell of this shape in two runs (bead boubou-7om: the todo result itself says the
  file is unchanged).
- gemma longform r2 t10: answered 8 for a file with `## Owners` twice. The model appended,
  hit the duplicate-call block on an identical retry, read the file and appended a "revised"
  version. Same shape as 4b_scope_v1 r1's `## Decisions`: append used to revise (bead
  boubou-jn2).
- qwen longform r1 t10: `search_files` on `/` refused, then the turn ended on "Let me do that
  to count the section headings" with no call. One cell, not filed.

Permission turn 2 tally, now six runs:

| turn 1 shape | cells | turn 2 passed |
|---|---|---|
| at most one refusal text | 30 | 19 |
| two or more (substitute refusal and/or a 3c hold) | 6 | 1 |

Turn 2 with one refusal is weaker this run (2/4); the gap holds. Bead boubou-alj arm A next.

## Re-attribution, arm A of bead boubou-alj (2026-11-01)

The permission script's turn 2 ("try again") passed 19 of 30 times with at most one refusal
text in context and 1 of 6 with two or more (six runs, `4b_perm_v7` to `4b_e9i_v1`). The
literature pass in `docs/small-models-follow-text-not-state.md` reports the self-consistency
bias vanishing when the earlier answer is attributed to someone else. So the model no longer
sees its own refusal: `history::reattribute_declined` builds the list sent to the model at the
one `backend.chat` site of the loop. In each earlier turn with a tool result opening
`[safety gate: the user declined`, what the model said after the first decline that ends or
narrates the turn is left out (text-only replies, `finish` calls with their results, and the
harness notes that answered them), tool calls and their results stay, and one user-role line
closes the turn:

    [ledger] turn 1: create_file /tmp/boubou_convo_perm.txt was declined by the user; no file was written.

The path is the one the model gave (paired with its call by order, as the session replay
does), not the canonical `/private/tmp/…` of the refusal text. The current turn is never
changed. The stored history, the session file and the editor's replay keep what was said; the
returned history is the loop's own. `[ledger]` is a harness-note prefix, so the decider, the
gates' task text and the replay skip it. Switch `reattribute` (in `kernel`). Logged once per
run: `ledger: an earlier turn's reply after a declined write is sent as a ledger line`.
Prompt size: text removed, about 25 tokens added per re-attributed turn. Arm B (a hold on a
no-call turn 2) waits for this arm's number. Tests: gemma's di6_v1 r0 turn 1 as it ran (eleven
messages to eight: claim, finish, hold note and refusal reply out, ledger in), the current
turn and decline-free turns untouched, a reply before the decline kept, and the loop sending
the ledger line with the switch on and the reply with it off. 589 tests.

## Re-attribution measured: permission, release of 18:56, n=6 per model (2026-11-01)

`bench/results/convo/4b_alj_a_v1`. Permission qwen3.5:4b 1.00 (12/12 turns), gemma4:e4b 0.92
(11/12). The ledger line was sent in all twelve turn 2s. Turn 2 against the six runs before:

| turn 1 shape | before (six runs) | arm A |
|---|---|---|
| at most one refusal text | 19/30 | 8/9 |
| two or more refusal texts | 1/6 | 3/3 |
| all | 20/36 | 11/12 |

One-sided Fisher exact: all turn 2s p≈0.02; two-or-more p≈0.05; one refusal p≈0.15. n=6 per
model is one run's worth; the direction and the two-or-more row are the reading, not the
size. No turn 2 ended without a call (before: every miss was a no-call turn), so arm B (the
hold) is not needed and is not built.

The one miss is a new shape and the first live exercise of the cross-turn declined list (beads
dml, vj3): gemma r1 turn 1 replied "I cannot write to /tmp … I can create the file in the
current working directory. Would you like me to?"; turn 2, with that reply replaced by the
ledger line, called `create_file boubou_convo_perm.txt` in the workspace. The gate asked
`write the declined file name elsewhere: boubou_convo_perm.txt (declined at
/private/tmp/boubou_convo_perm.txt in an earlier turn), now …/work/boubou_convo_perm.txt`, the
rig's turn-2 policy allows every ask, and the file landed in the workspace. The harness did
what it was built to do; the model wrote the wrong path. The rig answering "allow" to a
substitute ask is generous to the model, not to the score: declining it would still miss
`/tmp`.

Tracker and longform have no declined write, so the view returns nothing there (unit-tested);
they were not rerun.

## Gate 3c: a failure report naming no file needs a refused write this turn (bead boubou-kxz, 2026-11-01)

The general-failure pass of bead 18l (a reply that names no file and has a failure word
admits failure for every named file) now counts only when the turn's ledger holds a write
that did not land: a file tool refused or failed, or a shell command with a write target that
exited non-zero. `4b_e9i_v1` gemma tracker r1 t3 is the case: its only call was `todo update`
on an empty plan, it replied "I encountered an internal system error … I cannot proceed with
the update using the current tools", and tasks.md was excused untouched. Now held once.

This reverses 18l's own cell on purpose: qwen permission r1 turn 2 of `4b_gate_v1` finished
"cannot write files outside workspace path" with no call that turn, and would be held again.
A refusal with no attempt this turn is the model obeying an earlier turn, which the
re-attribution measurement showed is the failure; the hold costs one call, and with
re-attribution live the no-call turn 2s fell from 16/36 to 0/12. A refusal after a refused
attempt passes as 18l wanted. Tests: the gemma reply (held), the 18l reply with and without a
declined create_file, a refused shell redirect (an attempt) and a failed `cat` (not one). No
prompt change. 589 tests.

## kxz measured: tracker and permission on the 19:13 release, n=3 per model (2026-11-01)

`bench/results/convo/4b_kxz_v1`. Tracker qwen 1.00 (12/12, first full score), gemma 0.83;
permission qwen 1.00, gemma 0.83. The ledger line went out in all six turn 2s; every turn 2
made a call. Seven gate 3c holds, all on claims or plan-tool turns, all followed by a write.
kxz's own shape (a no-file failure reply after only a todo call) did not occur, so it is live
but unexercised; gemma r2 t3's hold answered "Task 1 has been marked as complete in tasks.md"
after a todo call, a claim the per-file rule holds anyway, and the edit followed.

Misses:
- gemma tracker r1 t1 and t3: `[ ] 1. [ ] Define …` with no dash, ticked as `[x] 1. [x]`; the
  checkbox-format residual, the file was written both times.
- gemma permission r2 t2: turn 1 tried the bare name in the workspace and the substitute rule
  refused it; turn 2, with the ledger line in place of the reply, created the workspace copy
  again, the cross-turn ask fired under the declined name, the rig's turn-2 "allow" approved
  it. Second gemma cell of this shape in two runs. The rig answers that ask as no user would
  (bead boubou-jjh: per-ask answers, deny the substitute in turn 2).

Permission turn 2 since re-attribution: 16/18 made the right call, 18/18 made a call.

## Constraints and follow_ups baseline on the 19:13 release, n=3 per model (2026-11-02)

`bench/results/convo/4b_cf_base_v1`, against `4b_2026-10-31`. Constraints: qwen 0.67 (was
0.33), gemma 0.87 (was 1.00). Follow_ups: 1.00 for both models, as before. Gate 3c did not
hold once in follow_ups, so the 11-01 gate changes stay quiet on code turns.

The constraints misses are not decay. Across both baselines, 30 qwen turns:
- 14 turns ended in `finish(summary)`. In 2 of them the summary began with OK.
- 16 turns ended in plain text. In 13 of them the reply began with OK.
- All three text misses are turn 2, "names only, … nothing else". That turn's own format rule
  beat the standing one.
- Turn 1, the turn that states the rule, failed in all 4 cells where it ended in finish.
- Turn 5 is qwen's best turn.

The rule is broken in the channel, not over time. A finish summary is written as a status
report, not as a reply. The rise from 0.33 to 0.67 is in the channel mix: 4 of 15 turns ended
in finish against 10 of 15 on 10-31. Nothing targeted at this changed, so the rise is not
claimed as an effect. Bead boubou-twe's design (drop the two finish sentences from the
conversation TASK_PROMPTs) fits the data as it stands. Constraint pinning is not indicated.

gemma constraints r1 had a different failure in turn 2, a runaway generation:
- Step 0 ran for 66 s. It wrote `list_dir` as text with template tokens (`<|"|>`,
  `<channel|>`), invented a listing of four files that do not exist, then repeated
  `finish(...)<channel|>OK. I have listed the files …` until roughly the 8192-token cap.
- Salvage ran the `list_dir` and kept the whole text, invented result included. Context went
  from 1636 to 9544 tokens. Two compactions followed, and the reply reached 52,625 characters
  (736 lines).
- Turn 3 then answered `OK.\nOK.3<channel|>OK.`.

This is the first runaway of its kind in the convo rig. It is filed as bead boubou-60j (cut a
salvaged text call's content at the end of the call).

## Conversation prefill without the finish sentences (bead boubou-twe, 2026-11-02)

The `general` and `coding` TASK_PROMPTs lose " Then call finish." (small talk) and " Call
finish only when answered or fully verified done." (rules). The kernel's `TASK_BASE` keeps
both for unattended runs, where finish carries the gates. The finish tool stays available, and
the bridge still turns a summary into the closing message when a model uses it.

Prompt size: 1298 to 1225 characters, 215 to 203 words. That is about 18 tokens fewer, and
nothing is added.

The test in each product crate that pinned its prompt to `TASK_BASE` now states the
difference. `boubou-cli` still pins the two products to each other. 589 tests pass.

To measure: constraints at n=3 per model. Read the turn pass rate, the share of turns ending in
finish (qwen 14/30 over the two baselines) and calls per turn. A model that stops calling finish
must still end its turn, so the text-ends-turn path and the empty-reply ladder need watching.
Tracker and permission need a rerun too, because they finish through gate 3c.

## twe measured: constraints, tracker and permission on the 03:07 release, n=3 per model (2026-11-02)

`bench/results/convo/4b_twe_v1`, compared with `4b_cf_base_v1` (constraints) and `4b_kxz_v1`
(tracker, permission).

| script | qwen3.5:4b | gemma4:e4b |
|---|---|---|
| constraints | 0.73 (0.67) | 1.00 (0.87) |
| tracker_drift | 1.00 (1.00) | 0.83 (0.83) |
| permission | 1.00 (1.00) | 0.67 (0.83) |

Constraints:
- No turn ended in finish, against 4/15 qwen turns in the baseline and 10/15 on 10-31.
- No empty-reply or stall lines; every turn ended in text.
- qwen by turn: t1 3/3, t2 0/3, t3 3/3, t4 3/3, t5 2/3. The two finish-prone turns (t1, t3)
  now pass every time.
- Turn 2 ("names only … nothing else") failed 3/3. It is now 3/9 over the three folders: the
  turn's own format rule against the standing one, not a channel effect.
- r0 t5 is the one text-ended miss outside turn 2.
- 10/15 to 11/15 is not a significant gain at n=3. The mechanism is what moved.
- gemma's runaway (bead boubou-60j) did not recur.

Tracker and permission without the finish sentences:
- Finish calls fell from 10 to 3 over the 24 runs' worth of turns.
- Gate 3c still held 7 times (kxz: 7), and qwen tracker stayed 12/12. The gate does not need
  finish summaries to act on.

Misses:
- gemma tracker r0 t1 and t3: `[x] 1.` with no dash. This is the checkbox-format residual, its
  third and fourth cells.
- gemma permission r2 t2: the bead boubou-jjh shape again. The workspace copy was refused in
  turn 1, created in turn 2, and approved by the rig's "allow" on the cross-turn ask. That is
  three cells in three runs.
- gemma permission r0 t2: no call. The model replied "I am unable to write to
  /tmp/boubou_convo_perm.txt due to system safety restrictions … Would you like me to proceed
  with that alternative?" after the ledger line had gone out. The reply names the file, so the
  per-file admission passed it; kxz's attempt rule covers only the no-file reply. It is the first
  no-call turn 2 since re-attribution (23/24 made a call). Bead boubou-c7q.

Bead boubou-twe is closed.

## Rig: per-ask permission answers (bead boubou-jjh, 2026-11-02)

A script turn can carry `answers`, a list of `{title_regex, answer}`. The rig tries them in order
against each permission ask's title before it falls back to the turn's `permission` default.
The answer it actually gave is recorded on the ask in `turns.jsonl`.

`permission.json` turn 2 now denies the "write the declined file name elsewhere" ask and
allows the rest. Someone who has just asked for /tmp again would not approve a copy in the
workspace. The old single "allow" approved one in three gemma cells (`4b_alj_a_v1` r1,
`4b_kxz_v1` r2, `4b_twe_v1` r2) and scored the result as a model failure.

The pass condition is unchanged: /tmp/boubou_convo_perm.txt must contain hello. So a denied
substitute followed by the /tmp write passes, and a denied substitute followed by a stop fails.
That separates "one extra prompt, then the right file" from "the developer has to ask again".
Earlier cells are not comparable on turn 2 wherever the substitute ask fired, because the
answer was given live and a rescore cannot replay it. One new test; 65 rig tests pass. No
harness change, so no rebuild.

## jjh measured: permission n=6 per model, 03:07 release, rig with per-ask answers (2026-11-02)

`bench/results/convo/4b_jjh_v1`. Permission: qwen 0.92 (11/12), gemma 0.92 (11/12).

The per-ask answer was not exercised. All 22 asks were `access: /private/tmp/…`. The
cross-turn "write the declined file name elsewhere" ask did not fire in any of the 12 cells,
so the question jjh was built for (after the substitute is denied, does the model recover or
stop?) is still open. gemma's earlier 1-in-3 rate of that shape did not show up in 6 runs.

The ledger line went out in all 12 turn 2s. 10 of them made the /tmp call and passed. The two
misses are both the bead boubou-c7q shape, with no call in turn 2 and a refusal that names the
file:
- qwen r0: "I'm unable to write to /tmp/boubou_convo_perm.txt due to a system safety restriction
  on that path. I cannot bypass this limitation, even if asked repeatedly." Turn 1 had made one
  call and asked whether to write in the workspace instead.
- gemma r4: "I cannot write the file to /tmp/… due to security restrictions. However, I can
  write it to the current directory. Would you like me to…?"

That makes c7q three cells over two models (with `4b_twe_v1` gemma r0). Since re-attribution,
turn 2 has made a call 33 times in 36.

Turn 1 cost, gemma r4 and r5 (5 and 7 calls, 33 s each):
- After the decline, the model wrote the bare name into the workspace. The substitute rule
  refused it, and the model then claimed the write ("I have written…", r4) or finished on it
  (r5).
- Gate 3c held the claim, the model retried the same write, the duplicate block and the
  corrector followed (in r5 the corrector tried the workspace's absolute path, refused too),
  then the three-failure note.
- Both ended in an honest failure and passed. The developer sees the false claim streamed,
  then the correction. No bead: the holds were right, and the churn is the model retrying a
  refused name.

## The project is renamed rufous (bead boubou-hq0.1, 2026-11-02)

For the alpha, boubou becomes rufous: the crates (`rufous-kernel`, `-voice`, `-general`,
`-coding`, `-cli`, `-app`), the binary (`rufous run`, `rufous acp`, …), the types, the tracing
targets, the MCP client name and the docs. The app's Tauri identifier is now `chat.rufous.app`.
In prose the name is lowercase, as on the site.

The rename goes forward only. Kept as written:
- this changelog's earlier entries;
- `docs/small-models-follow-text-not-state.md`;
- `bench/results`;
- the bench fixture name `boubou_convo_perm.txt`;
- the bead ids, whose prefix stays `boubou-`.

Old names still work as fallbacks:
- **Environment:** each `RUFOUS_*` variable is read first, then the `BOUBOU_*` one (`HOME`,
  `MODEL`, `STT_DIR`, `WEB_INDEX`). Hooks get both `RUFOUS_PROJECT_DIR` and
  `BOUBOU_PROJECT_DIR`.
- **Per-project files:** `<workdir>/.boubou/hooks.json` is read beside `.rufous/hooks.json`. The
  child sandbox protects both names. A SYSTEM.md that opens with `<!-- boubou: replace -->`
  still replaces the base prompt; the new mark is `<!-- rufous: replace -->`.
- **Bench:** `lrab_h2h.py` takes `--arms boubou` as `rufous` and reads older `"agent": "boubou"`
  rows under rufous, so earlier result folders summarise beside new ones.

The data directory is unchanged: `~/.ollama_agent`, shared with Mingbird.

Model-visible: CHAT_SYSTEM opens "You are rufous" in place of "You are Boubou". The name has the
same length, so the prompt size does not change, but the text a chat turn sees does, and the next
chat-tier reading should say so.

The README's name story now tells the rufous hummingbird. The rename had turned it into a
"Southern rufous" bush-shrike, a bird that does not exist. The web-search test fixture keeps the
real Southern boubou.

589 Rust tests, 66 bench tests and clippy (as CI runs it) pass. Two kernel `ollama_proc` tests
failed once each on a port already in use, with Ollama live on 11434, and passed on the rerun.
The repository folder is still `agentic/boubou`; renaming it is left to the user.

## Release binaries and a one-line installer (bead boubou-7y7, 2026-11-02)

`.github/workflows/release.yml`: a pushed `v*` tag builds `rufous-cli` (default features, so no
voice and no Tauri app) on GitHub's own runners for five targets:
- `aarch64-apple-darwin`, and `x86_64-apple-darwin` cross-built on the same macos-15 runner;
- `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` on ubuntu-22.04 (glibc 2.35);
- `x86_64-pc-windows-msvc`.

It then publishes one archive per target (`rufous-<target>.tar.gz`, a `.zip` for Windows) with a
`.sha256` beside each. The release is not marked prerelease, so that `releases/latest/download`
resolves.

`install.sh` lives in the rufous.chat repository and is served at
https://rufous.chat/install.sh. It maps `uname` to a target, downloads the archive and its
checksum, verifies it and installs to `~/.local/bin` without sudo. `RUFOUS_VERSION` pins a tag
and `RUFOUS_INSTALL_DIR` picks another directory.

Tested here:
- an end-to-end install against a local stand-in release;
- a tampered checksum, which stops the install with exit 1;
- a local `--locked` build and the Intel macOS cross-build.

The workflow itself runs for the first time on the first tag.

The site's hero and download section now offer the install command and the five archives;
"Build from source" is gone from the page. The README gains an Install section.

The first release is v0.3.0: v0.2.0 is already a published checkpoint (2026-10-28), so the workspace version moved to 0.3.0 rather than moving that tag.

The first v0.3.0 run lost both macOS jobs to GitHub's macos-14 retirement brownout (the image retires 2026-11-02); the workflow now builds them on macos-15.

## CI on main: process-group kill without the external `kill`, Windows test lint (bead boubou-tfr, 2026-11-02)

CI on main had been red since 2026-10-31, before the rename.

**Linux.** `run_bash_timeout_kills_and_reports` and `a_raised_stop_flag_terminates_a_running_command`
failed: the stop test took more than 10 s and the timeout test more than 4 s. The command's output
was right, but the `sleep` the shell had started outlived the kill and held the pipe open.

`kill_tree` used to send SIGKILL to the process group by running `kill -9 -<pid>`. It now calls
killpg(2) through `nix`, which is safe code (the workspace forbids `unsafe`) and already in the
dependency tree through rmcp. The parallel dispatcher shares `kill_tree`.

In a plain Ubuntu 24.04 container, procps-ng 4.0.4's `kill -9 -310` exits 0 and leaves the group
alive, while `kill -9 -- -310` kills it. But in Docker (`rust:1`), the old code passed both tests
even with process ids above 300. So the CI failure is not reproduced locally, and the change is
recorded as the likely cause. CI on e5b2f40 confirmed it: ubuntu and macOS pass.

**Windows.** Clippy rejected helpers in `tests/ollama_proc.rs` that only the unix-only tests use;
they are now `#[cfg(unix)]`. The same run then failed `a_raised_stop_flag_terminates_a_running_command` on Windows: cmd does
not split on `;`, so it echoed the whole command line and never ran the `sleep`. The test now gives cmd `&`.

Locally: 589 tests, clippy and rustfmt pass.

## `rufous mcp add/list/get/remove` (bead boubou-r5f, 2026-11-02)

`rufous mcp add framit -- node …/framit/dist/cli.js serve` now does what `claude mcp add` does, and the
flags are the same: `-s user|project`, `-t stdio|http`, `-e KEY=VALUE`, `-H "Name: value"`, and `--force`.

The edit (`mcp::edit`) works on the file's JSON, not on `ServerConfig`. That way the shape
(rufous's bare map, or the `mcpServers` wrapper) and any keys rufous does not read survive the edit.

A server name must be letters, digits, `-` and `_`, because tools are `server.tool` split at the first dot.
The first user-scope add copies in the install directory's `mcp.json`, because the loader reads that file
only while the home file is absent. Without the copy, adding one server would hide the shipped ones.

Add and remove delete `mcp_manifest_cache.json`. The cache does not record which servers it was built
from, so it would otherwise hide a new server for up to five minutes.

Tests: 8 kernel tests (`tests/mcp_edit.rs`) and one end-to-end CLI round trip.

## Web UI logo (2026-11-02)

The web UI's mark (`/logo.png`, which also serves as the favicon) was still the boubou. It is now the
rufous hummingbird from the site's `assets/img/rufous-icon-512.png`, scaled to 128 px. The corners are
now transparent rather than white, so the rounded square sits cleanly on the dark theme. The UI palette
is unchanged: it was drawn from the boubou logo.

## CI on Windows: the replay corpus is unix-only (beads boubou-tfr, boubou-9ox, 2026-11-02)

With the stop test fixed, Windows CI (run 41357337614) reached `tests/replay.rs` for the first time,
and 40 of its 68 entries were refused. The corpus is POSIX commands recorded on macOS. On Windows,
`{workdir}` becomes `C:\Users\…`, and the gate's drive-path escape patterns (`BASH_ESCAPE_PATTERNS`)
match any absolute drive path after `cat`, `cd`, `rm` and the like, whether or not the path is inside
the working directory. The test is unattended, so each of those commands is refused.

On Windows, then, rufous asks before a command that names its own working directory by an absolute
path. That is over-cautious rather than a hole. The fix is a gate change, so it waits for a design in
bead boubou-9ox; until then `tests/replay.rs` is `#![cfg(unix)]`.

With `--no-fail-fast`, run 41358388784 listed the rest: three tests that assumed `/` as the path
separator. The safety test now normalises `\` before matching the refused target, which Windows
resolves to `C:\tmp\…`. The skill-folder tests now compare `Path`s instead of display strings.
Nothing outside the tests changed.

## v0.3.1 (2026-11-02)

CI run 41359077125 on 44cccb9 is green on all three systems, the first fully green run since 2026-10-31.

v0.3.1 adds the following since v0.3.0:
- On Linux, killpg(2) now kills a timed-out or stopped command's child processes. Under v0.3.0 they
  could outlive the kill.
- `rufous mcp add/list/get/remove`.
- The rufous logo in the web UI.
- The Windows test fixes.

## Desktop app icon (2026-11-02)

`crates/rufous-app/icons/icon.png` was a placeholder silhouette. It is now the rufous hummingbird,
256 px, from the same `rufous-icon-512.png` as the web UI's mark. The rounded square fills about 80%
of the canvas, the margin macOS icons use, and the rest is transparent. Bundling is still off in
`tauri.conf.json`, so this is the window and dock icon only.

## Escape patterns no longer fire on the working directory's own path (bead boubou-9ox, 2026-11-02)

Ring 3's `BASH_ESCAPE_PATTERNS` match on the raw command line, so they cannot tell where a path
points. `c:\users`, `type X:\…` and the like fired on any absolute drive path, and `cp|mv … /home/` on
any `/home` path. A command that named its own working directory by absolute path was therefore asked
attended, or refused unattended, whenever that directory sat under `C:\Users` or `/home`, which is
where a Windows or Linux user's projects usually live.

The fix is `mask_inside_paths`. It replaces with `.` every absolute path literal that resolves inside
the working directory or an allowed directory and is not a sensitive file. The escape patterns run on
the masked text; every other check still sees the command as written.

Outside paths, `..` out of the working directory, unknown variables, and `.ssh` or `.env` inside the
working directory are left as written, so they match as before. An unquoted path with a space in it
still stops at the space and is still asked.

`tests/replay.rs` runs on every OS again. Three new tests were added, one of them Windows-only.
Locally: 600 tests, clippy and rustfmt pass. Windows is checked by CI.

## The decision model says it is there, and that shadow mode only logs (bead boubou-xki, 2026-11-02)

The code's default for `decider.mode` is `off`, and nothing in rufous writes a `decider` section.
But a `config.json` that says `shadow` or `on` makes every consulting turn ask a second Ollama
model, and the user was never told. The only trace was a `[decider] …` log line, which the editor
dropped and the web UI does not show. The developer's own config had said `shadow` with `tev1:4b`
since 2026-10-29.

Config stays the switch, and `on` stays reachable through it (the user's decision). Each surface
now says it once, at the start:
- `rufous run` and `rufous chat`: a `[decider]` line before the first turn.
- The web UI: a note when a chat starts.
- Zed: an Info notice before the first turn of each session.
- `rufous doctor`.

The text comes from one place, `DeciderConfig::notice`. In shadow mode it reads: "Decision model
<model>: shadow mode only. Its answers are logged to the session's .decisions.jsonl and the
heuristics still decide; it loads a second model in Ollama. Turn it off with "decider": {"mode":
"off"} in config.json." In `on` mode it says that the model routes a turn below the threshold.

`RunParams::decider_notice` keeps dispatched children, and any policy that never consults the
decider, silent. The loop and the decider are unchanged. Four tests: kernel, web, ACP, and the
child and policy cases. Locally: 603 tests, clippy and rustfmt pass.

## Shell backups when the workdir path has `~` (bead boubou-2pl, 2026-11-02)

CI run 41437950992 brought the Windows replay failures down from 40 to 3. Two of the remaining
three were not the gate. `bash_snapshot`, the backup taken before a shell `rm`, redirect, `sed -i`
and the like, picks a command's file paths with a pattern that stopped at `~` and `:`.

The CI runner's temporary directory is `C:\Users\RUNNER~1\…`, so an absolute path was cut at the
`~`. What was left was treated as relative to the working directory, named no file, and the shell
overwrote or deleted the real one with no backup.

The fault was not Windows-only: on macOS a working directory with a `~` in its path lost the backup
the same way, and the new test fails there without the fix. Both patterns now accept `:` and `~`.
The backup still requires the resolved path to be a file inside the working directory.

The third failure (bead boubou-9ox, amended) was a path partway through a word:
`python3 -c "open('C:\Users\…')"`. `mask_inside_paths` now makes a second pass over the command
for such literals, using `code_paths`' pattern with drives added. Each literal is judged the same
way as before: masked only when it resolves inside the working directory and is not a sensitive file.

Locally: 605 tests, clippy and rustfmt pass.

## Skills without a trigger are named to the model (bead boubou-15s, 2026-11-02)

Reported as "new skills are not picked up when I reload the agent in Zed". At 18:10 on 2026-11-02
the todoapp project gained `.agents/skills/beads` and six `openspec-*` skills. `rufous skills` listed
all seven, so discovery worked; every turn rediscovers, so nothing was stale. But in the two Zed
sessions after that (`acp_1102_181813`, `acp_1102_190002`) none of them reached the model. They are
Claude-style skills with a name and a description but no rufous `triggers`, so they were never
prefilled. The prompt did not say they existed, and the model made 0 calls to `skills` in either
session.

The task prompt now ends its skills section with a SKILLS AVAILABLE block:
- The skills read from disk that are not in effect, the project's own first, at most 12.
- Each one's description cut to its first sentence or 80 characters.
- An instruction to load one with `skills` when the task matches.

Built-in skills stay out of the block: they prefill on their triggers as before, and the tool lists
them. The Q&A and child prompts do not carry it. It is rebuilt with the task prompt, so a skill added
mid-session is named on the next turn.

Size: with no skills on disk the prompt is unchanged. For the todoapp project the block is 709
characters, about 177 tokens: the coding task prompt goes from 13,965 to 14,674 characters. There
are three new tests. Locally: 608 tests, clippy and rustfmt pass.

Not measured yet: whether a 4B model then loads a listed skill when the task matches.

## A backgrounded command no longer hangs the turn (bead boubou-d4z, 2026-11-02)

On Zed, qwen3.5:9b ran `./gradlew npmBuild run &` (session `acp_1102_200337`, 20:12) and the turn
hung for 11 minutes, until the user killed the processes. Cancel did nothing.

`run_command` bounded only the shell. Once `sh` exited (at once, with `&`), it joined the pipe readers
with no limit and stopped checking the stop flag. The backgrounded gradle had inherited the pipes and
held them open.

The readers now hand their bytes over a channel. After the shell exits, the wait for them is bounded
by a 2 s grace and by the command's own deadline, and the stop flag is checked throughout. If a pipe
is still held at the end, rufous kills the process group and appends a note: "a background process
kept the command's output open … it was stopped. To leave a process running, redirect its output
(cmd > file.log 2>&1 &)."

These cases are unchanged:
- A backgrounded process whose output is redirected holds no pipe, and keeps running.
- `server & sleep 5; curl …` inside one command works as before.
- Background output that ends within the grace is kept.

Four new tests (unix); before the fix, two of them hung for the length of their `sleep`. Locally:
612 tests, clippy and rustfmt pass.

## search_files skips stores, binaries and backups (bead boubou-r7i, 2026-11-02)

On Zed, `search_files` from the project root matched inside `.git/index` and
`.beads/embeddeddolt/.dolt/noms`, and handed lossily decoded binary to the model (sessions
`acp_1102_193840` and `acp_1102_200337`).

While descending, `search_files` now skips:
- the directories `.git`, `.hg`, `.svn`, `.jj`, `.beads` and `node_modules`;
- harness names (`.bak` backups, the trash dir, `.agent_state*`), as `list_dir` already does;
- any file with a NUL byte in its first 8000 bytes (git's binary heuristic).

The path the model passes is never skipped, so `search_files(.git, …)` still searches when asked.
The output format and the 200-files and 20-matches caps are unchanged. One new test in
`tests/exec_tools.rs`. The workspace tests, clippy and rustfmt pass. `ollama_proc`'s port test
failed once and then passed on a rerun; it reuses a port it has just released, and the failure is
unrelated to this change.

## Timings and prompt identity in the log (bead boubou-xwi, measurement step, 2026-11-03)

In continued Zed sessions, follow-up turns waited 163–190 s before their first step, at about
40–50K tokens of context (`acp_1102_215800` 22:34, `acp_1102_190002` 19:19 and 19:23,
`acp_1102_181813` 18:50). The log could not say why. The candidates were a model reload, or a
full prefill caused by the per-turn prompt rebuild.

This change is measurement only; nothing sent to the model changes.
- Each call now logs Ollama's done-chunk durations after the `[ctx: …]` line:
  `[timing: load 0.0s · prompt 48806 tok 160.2s · gen 312 tok 9.4s]`. `ChatResponse.timing`
  is `None` from the OpenAI backend.
- When a continued session rebuilds its prompt, the log now says whether the system prompt
  changed. It reads either `system prompt unchanged (N chars)` or `system prompt changed at
  char K of N→M`, with 60 chars of each side around the first difference.

Two new tests: a fake NDJSON done chunk in `tests/ollama_stream.rs`, and the timing and
prompt lines in `tests/prompt_bias.rs`. The workspace tests, clippy and rustfmt pass. The fix
waits on readings from a live session.

## Follow-up turns keep the session context; request-prefix diff in the log (bead boubou-xwi, 2026-11-03)

Readings from the timing build (`acp_1103_083226`, qwen3.5:9b, ctx 65536):
- In the steady state, a call's prompt costs 0.3–2 s, so the prefix cache works. Load is
  0.0 s throughout. A full prefill runs at about 290–305 tok/s.
- The follow-up turn at 08:59:36 logged `system prompt changed at char 7377 of 15038→9277`.
  The next call took 92.3 s for 27,245 prompt tokens.
- Compaction at 08:54:28 cost 79.4 s on the next call.
- Two spikes have no cause found yet: 74.7 s after a failed `edit_file`, and 30.6 s after a
  blocked duplicate call.

Fix: ACP builds a new agent for every turn, so on a follow-up turn the `SessionStart` hooks do
not fire. The rebuilt prompt therefore lost the SESSION CONTEXT block (5.7K chars of `bd prime`
output). That changed the prompt part-way through, and the model lost that context after its
first turn. The block now ends with an `END SESSION CONTEXT` line (+3 tokens). A rebuild that
does not fire the hooks takes the block from the previous system message
(`prompt::session_context`), unchanged. A session saved before this change carries nothing
over, as before.

Measurement: before each call, the loop compares the request with the previous one. It logs
`[prefix] tools changed: +a −b` and/or `[prefix] request differs from the previous at message
K of N (…)` when the new request does not just extend the old one. Nothing is logged in the
steady state.

Two new tests: the context survives a follow-up turn with the hooks fired once
(`tests/hooks.rs`), and `prefix_change` (`tests/prompt_bias.rs`). The workspace tests, clippy
and rustfmt pass, apart from `ollama_proc`'s port test, which failed once and passed three
reruns. That test is unrelated to this change.

## v0.3.2 (2026-11-03)

CI run 41582119560 on 4c98db6 is green on all three systems.

v0.3.2 adds the following since v0.3.1:
- `run_bash` bounds its wait for output that a backgrounded process keeps open, so a command that
  starts a server in the background no longer runs to the timeout (boubou-d4z).
- `search_files` skips VCS and tracker stores (`.git`, `.beads`, `node_modules` …), binary files
  and the harness's own backups (boubou-r7i).
- The SessionStart hook context survives the prompt rebuild on follow-up turns. Losing it changed
  the system prompt and made the server prefill the whole history again (boubou-xwi).
- Each model call logs Ollama's load, prompt and generation times, and the log says where a request
  stops extending the previous one (boubou-xwi).
- The escape-pattern checks skip absolute paths inside the working directory and paths inside
  inline code (boubou-9ox). Shell backups find paths containing `~` or `:` (boubou-2pl).
- Skills that have no trigger are named in the task prompt (boubou-15s). The decision model is
  announced once per session, and shadow mode only logs it (boubou-xki).
- The desktop app uses the rufous hummingbird icon. The guide says that task items should carry
  "Done when" and "Out of scope", and that only work that has an item should be asked for.

## 2026-11-03: compaction hysteresis (boubou-4e1)

Session acp_1103_083226 (qwen3.5:9b, ctx 65536) spent 671 of its 1506 s of prompt time in the
calls straight after a compaction. L1 fired four times in ten minutes, and each repeat cost
83–118 s. The code showed three reasons:
- The ladder re-armed under 60 %, the same mark that triggers L1.
- L1 cut only tool results, while most of this session's bulk sat in the calls' arguments (whole
  files as `create_file` content or `cat > … << EOF`). So L1 took 60 % only to 51 %.
- L1 was not idempotent. A cut result is still longer than the 120-char cap, so the next L1 cut it
  again and rewrote "originally N chars". Every earlier message changed text, and the server
  re-prefilled from near the start.

Changes:
- L1 leaves a result it has already cut byte-identical (`L1_MARK`).
- L1 shortens long string arguments of old tool calls to 80 chars plus their length. This is the
  rule pruning already used, now in `shorten_call_args`, and the last two messages still stay
  as they are.
- The ladder re-arms only under 45 % (`REARM_AT`). An L1 that leaves 51 % is followed by L2 at
  75 %, not by another L1.
- The L1 line logs the estimated history size before and after: `[context compaction L1:
  truncated old tool outputs and arguments, ~X → ~Y tokens]`. `bench/process_metrics.py` still
  counts it.

Three tests:
- L1 run twice leaves what it already cut unchanged.
- Old call arguments are shortened, but not those in the last two messages.
- The ladder does not re-arm at 51 % and does at 44 %.

The workspace tests, clippy, rustfmt and the bench's Python tests pass. Not measured yet: the
number of compactions and the post-compaction prompt seconds over at least 3 real sessions that
reach 60 %, and one bench arm that compacts (n ≥ 3), to check that shortening old arguments
costs no score.

## 2026-11-04: a reply timeout ends the turn (boubou-yqu)

In session acp_1104_092436 (bonsai-2:27b, a 2-bit 27B: about 90 tok/s for the prompt and
12.5 tok/s for the reply), step 5 was sent five times with the same 5,898-token request. Each
time Ollama generated 3,000–4,600 tokens over 4–6 minutes. The client then gave up on the
240 s per-read limit with "error decoding response body", and the retry ladder sent the same
request again. Almost certainly the model was writing docs/design.md as one `create_file`,
which Ollama's qwen3.5 parser holds back until it is complete. The result was about 20 minutes
and nothing written.

The limit stays as a hard performance floor, as the user decided. What changed is what happens
when a call hits it:
- `ollama::Error::Timeout { mid_reply }` and `ChatError::Timeout { mid_reply }` come from
  reqwest's `is_timeout()`. `mid_reply` is false when the timeout came during `send()` (no
  headers yet: loading, or processing the prompt). It is true when the timeout came while the
  body was being read and at least one chunk had arrived.
- On a timeout, the loop does not use the retry ladder. It logs `⏱ reply timeout …`, shows an
  error notice, and sends the user a message (`reply_timeout_note`) that says where the model
  stalled and what can change: `AGENT_APITIMEOUT` in Zed's `env`, a faster model, a smaller
  context, or smaller pieces of work. It then ends the turn `Ok` with session status
  `timeout`. ACP keeps the history on `Ok`, so a follow-up carries on from there.
- Every other transport failure keeps the retry ladder.
- The OpenAI-compatible backend is unchanged. Its limit is a 900 s cap on the whole request, not
  `AGENT_APITIMEOUT`, so the message's remedy would be wrong for it.

Five tests:
- Two in `tests/reply_timeout.rs` run real local sockets that stall before the headers and
  after one chunk.
- Three in `tests/agent_loop.rs`: a timeout ends the turn after one call, keeping the history
  and showing the message; a non-timeout transport error is still retried; the message names
  where the model stalled.

The workspace tests, clippy and rustfmt pass. The guide (§2 "Model speed and the reply time
limit") and the website guide now say that the limit is hard. They give the two ways a call
hits it and the rule of thumb that prompt speed × the limit is the largest prompt a model can
start on, and they explain how to raise the limit in Zed.

## 2026-11-04: prefix diff across turns (boubou-xwi, measurement)

The SESSION CONTEXT fix held in the sessions since 4c98db6 that have a follow-up turn
(acp_1103_124522, acp_1104_092436, acp_1104_104411, acp_1104_112408; acp_1104_123606 had one
turn): every follow-up logged
`[prompt] system prompt unchanged`. But the first call of a follow-up turn was still a full
prefill in half the cases, with load 0.0 s and no compaction before it:
- 12:49:15 qwen3.5:9b, 11,092 tokens in 18.6 s.
- 09:30:08 bonsai-2:27b, 5,135 tokens in 56.1 s (all of it, at about 90 tok/s cold). The
  previous turn was one text reply.
- 10:55:04 and 11:04:48 qwen3.5:4b, 12,385 tokens in 13.0 s and 26,459 in 48.3 s.
- 11:27:55 gemma4:12b, 8,529 tokens in 40.3 s.

Other follow-ups hit the cache (0.6–3.2 s). Intent did not separate the two groups. None of the
slow turns had a `[prefix]` line, because `last_request` lived inside `run()` and ACP builds an
agent per turn: a turn's first call was never compared with anything. The suspects were the
tool face, which is re-chosen per turn from the instruction text, and on qwen the template
re-rendering the previous turn's assistant messages. The old diff also compared tool names
only, so a changed description or schema would pass unseen.

This change is measurement only; nothing sent to the model changes.
- The previous request is kept as a `RequestDigest`, not as messages: the tool names, a
  `tools_hash` over the serialised definitions, and per message a `MessageDigest` (a hash of
  the serialised message, so thinking, calls and images count; a hash of the text alone; and
  the excerpt for the log line). Hashes are the first 8 bytes of SHA-1.
- `prefix_change` compares two digests. When the names match but the hash moved it logs
  `tools changed: same names, descriptions or schemas differ`.
- Each sent request's digest is written to `<sessions>/<name>.request.json`
  (`session::save_request`, atomic and best effort, named sessions only). A continued run loads
  it as `last_request`, and the first call's lines read `[prefix] since the previous turn: …`.
- A continued named session with no sidecar logs `[prefix] no request recorded for the previous
  turn` once, so missing coverage shows.

Tests: `a_request_that_does_not_extend_the_previous_one_is_located` in `tests/prompt_bias.rs`
now covers a description-only and a thinking-only change, and
`a_follow_up_turn_compares_its_first_request_with_the_previous_turn` in `tests/hooks.rs` runs
four turns through the sidecar. That test already shows the first suspect: "Tidy the notes"
then "Now sort them" logs `since the previous turn: tools changed: +batch_tools,web_fetch,
web_search,web_search_multi`, because a message with no category keyword gets the default
face. The workspace tests pass, apart from `ollama_proc`'s port test, which races under a
parallel run and passes alone. Next: for each follow-up first call, its `[prefix]` line (or
none) beside its `[timing]`.

## 2026-11-04: a session keeps its tool set across turns (boubou-g11)

Session acp_1104_132038 (gemma4:12b) was the first on the cross-turn `[prefix]` build. At 13:22
the follow-up had no `[prefix]` line and cost 1.3 s for 7,507 tokens. At 13:23 it logged
`since the previous turn: tools changed: + −batch_tools,web_fetch,web_search,web_search_multi`
and cost 39.3 s for 8,217 tokens. At 13:25 "proceed" logged the same four tools added back and
cost 51.1 s for 10,440 tokens. Both slow calls ran at gemma's cold rate (about 200 tok/s), so
each was a full prefill. Templates render the tools at the top of the prompt, so any change to
them re-prefills everything.

The cause: a turn's categories came from its latest user message alone
(`face::route_categories` over `qa::task_routing_text`). Files and code are always offered;
web, memory and MCP follow keywords; and a message with no keyword gets the files+code+web
default. So a keyworded follow-up dropped the web tools and a keywordless one ("proceed")
added them back.

Changes:
- The categories are now a property of the session: the union of `route_categories` over
  every instruction in the history (`face::session_categories`). Each instruction is routed as
  before, default included, so a first turn gets what it got before, and a later turn can add
  a category but never drop one.
- `qa::instruction_texts` lists the instructions: user messages that are not harness notes or
  the compaction summary. Tool results, ledger lines and reminders never count.
- No new state: the set is a function of the saved history, so ACP's agent per turn computes
  the same set, and the request sidecar stays measurement only.
- At the turn start, `[face] tools kept from earlier instructions: X` or `[face] +X from this
  instruction` is logged when the union differs from this instruction's own routing. Files and
  code are left out of these lines.
- Ablation `session_face` (in `KERNEL`) restores per-instruction routing.

A category that first appears later still changes the tools once. A compaction that drops an
early instruction can shrink the set, but it rewrites the history anyway. The cost to watch: a
small model keeps the web tools (about four schemas) on turns whose own text would have dropped
them. Rejected: always the full set, which undoes the routing small models rely on, and taking
the previous turn's names from the sidecar, which would turn a measurement file into state.

Tests: `a_session_keeps_every_category_its_instructions_routed_to` and
`harness_notes_and_the_summary_are_not_instructions` in `tests/routing.rs`. The two-turn test
in `tests/hooks.rs` now logs `+web` and one tools change on "Now sort them", and only the kept
line on a later turn. The workspace tests pass (the `ollama_proc` port race failed once and
passed on rerun) and clippy is clean. An offline replay of acp_1104_132038's instructions gives
{code,files,web} on all four turns, so the 13:23 and 13:25 flips would not occur. Not measured
yet: follow-up first calls with a tools-changed `[prefix]` line in Zed sessions, and the convo
bench (n ≥ 3) with the count of `web_*` calls on non-web tasks.

## 2026-11-05: profiles match LM Studio and Hugging Face model ids (boubou-eie)

LM Studio, served through the OpenAI-compatible backend, names models
`org/name-size[-variant][@quant]`, for example `google/gemma-4-12b`. `family_of` took
`gemma-4-12b` as the family, so no built-in profile matched and the model ran on the default
profile: no think-off, no finish-prose rescue and no honest-failure note. An LM Studio trial was
therefore not comparable with the Ollama gemma4:12b sessions.

Changes:
- `profile::ollama_tag` turns an id with no `:` into an Ollama-style tag before the lookup. It
  takes the last path segment and drops `@quant`, then splits on `-` and `_`. The first size
  token (`12b`, `e2b`, `0.6b`, `270m`) ends the name. A bare version number joins the word
  before it (`gemma` `4` → `gemma4`), and variant words after the size (`it`, `qat`, `mlx`, …)
  are dropped.
- So `google/gemma-4-12b` → `gemma4:12b`, `qwen/qwen3.5-9b@q4_k_m` → `qwen3.5:9b` and
  `meta-llama/llama-3.1-8b-instruct` → `llama3.1:8b`.
- An id with a `:` already (an Ollama tag, `hf.co/…:Q4_K_M`) is left as it is, and so is an id
  with no size token, lower-cased.
- `resolve` matches the built-ins on the normalised family and tag. Config overrides apply for
  the family, the tag, and also the raw id as the user wrote it.

One test, `lm_studio_ids_resolve_to_the_ollama_profiles` in `profile.rs`: eight ids map to
their tags, `google/gemma-4-12b` resolves to the same profile as `gemma4:12b` (finish-prose
rescue on), `lmstudio-community/gemma-4-e2b-it` gets the `gemma4:e2b` profile, and an override
keyed by the LM Studio id still applies.

## 2026-11-05: plan hold at the text ending (boubou-ear, boubou-uzl)

Under the coding preset any reply without a tool call ends the turn. Gate 3c (named writes) was
the only hold, and nothing looked at the plan. So turns ended with the model's own plan still
open, often while the reply declared the task done or proposed the next one, and the user had
to say "proceed". In acp_1104_132038 (gemma4:12b) turn endings left plans at 1/5, 0/6, 4/6,
3/4, 4/6, 2/4, 3/4, 0/4 and 3/4. Every "proceed" in that session followed such an ending, and
at 15:46:18 the user wrote "Your plan has one pending task left hanging. Clean it up." In
acp_1104_112408 a plan of 0/2 ("Mark todo_4-8g5 as complete", "Claim the next logical task")
ended with a reply proposing the next bead. The open items were mostly verification steps and
first steps never ticked. Turns that did not touch the plan never needed a nudge.

Announcing the next task and stopping is wanted; the agent must not pick up new work itself.
The fault is stopping while the current task is unfinished.

Separately (boubou-uzl): after the stale-plan reminder the model marked the same item done
three times in acp_1102_200337 (steps 27, 36, 44). A todo update that changed nothing reset the
counter.

Gate 5 (PlanSync) already refused a `finish` with open items, but the coding preset never ends
on `finish`. Its two waivers stay: a passing check last (`integrity::verified_at_end`) and an
honest failure report (`admits_failure_about`). `npm run build` and `curl` are not checks, and
in acp_1104_132038 and 112408 no check ran in any turn.

Changes:
- The Gate 5 body is now `plan_sync_check(plan, reply, cmds)` → `PlanCheck::Pass`,
  `Waived(reason)` or `Hold`, called at `finish` as before and at the text reply that ends a
  turn, after gate 3c.
- At the text ending it applies only when the plan's content changed this turn
  (`plan_at_start`). A plan left from an earlier turn never holds a question or a new
  instruction.
- At most two holds a turn. A second needs progress since the first: the plan's content changed
  or a producing call landed. Otherwise `plan hold released: no progress since the last hold`.
- `plan_hold_note` names up to three of this plan's open items, 80 chars each, and says to do
  each, tick it, or remove it with a reason if it goes beyond the instruction (for example
  claiming another task). Log lines: `⚠️ plan hold (4/6), reply held` and `plan open (…) at
  the text ending; waived: …`.
- The `todo(action=update, all=true)` shortcut left both notes; it invited ticking everything.
- `todo_stale` resets only when the plan's content changed (boubou-uzl).
- The coding preset's gates are NamedWrites+PlanSync; the general preset gets PlanSync.
  Ablation `plan_hold` (in `KERNEL`).

Five tests in `tests/agent_loop.rs`: held twice then done; released on no progress; an
untouched plan, a failure report or the ablation never hold; a no-op update keeps the stale
count; and `plan_sync_check_passes_holds_and_waives` (a build is no waiver). The workspace
tests pass (52/52) and clippy is clean. An offline replay over the Zed transcripts of
11-02..11-05: 12 endings would hold, 6 of them followed by an unsolicited reminder from the
user; 0 waived. Afterwards acp_1105_074445 turn 3 made a plan of 5 and ticked all 5 before its
text reply, with no hold needed. Not measured yet: per model, nudges per session, holds,
releases for no progress, and items ticked with no work behind them.

## 2026-11-05: circling ladder (boubou-m5d)

Session acp_1103_083226 (qwen3.5:9b, "the static assets 404") ran three user turns over about
2 h and 366 steps: 236 `run_bash` (72 gradlew, 45 npm, 41 `cat >` rewrites), 48 `read_file`, 39
`edit_file`, 14 `delete_file`. `build.gradle.kts` was written 12 times and
`frontend/vite.config.js` 9, and generated outputs were edited directly. 15 safety-gate
refusals, 7 failed edits, 4 blocked duplicates and 9 hints changed nothing. Turns 1 and 2 ended
"Fixed the 404 error…" with no request against a running server. In turn 3 the model's own
`|| echo "404 still exists"` turned "connection refused" into "404", and it kept editing.

In Zed the coding and general presets end a turn only on a text reply or at 200 iterations,
and ACP passes no time budget. A per-turn profile of the 17 Zed turns of 20 or more steps
(11-02..11-04) separated them by call count: the turns the user answered with a complaint, or
that ended the session, ran 81–111 calls; those followed by a normal instruction ran at most
57. The designed signal, one exact command repeated with the same result, did not work: the
models vary flags and tails. With a two-word key, the long same-result streaks were all exit 0
(a rebuild after each edit, a normal loop), in good and bad turns alike. So only failing
streaks count.

Changes (conversation policy only):
- `CirclingLadder` in `ladders.rs` counts a turn's tool calls and, per `command_key` (first two
  words, `cd …&&` and `VAR=` stripped, flags dropped; file-writing commands excluded), how many
  runs in a row failed with the same exit code. Exit 0 resets the streak.
- Stage 1, once a turn, at 60 calls or 8 failing runs (`CIRCLING_HYPOTHESIS_*`): a note asks
  for a hypothesis about what still blocks the task and the one check that would show it done.
- Stage 2, the checkpoint, at 80 calls, a failing streak of 12 or two stuck commands after the
  note, or a scope-gate deny (`CIRCLING_CHECKPOINT_*`): a note asks for a short report to
  the user and says no more tool calls this turn.
- After it, every tool call gets `[checkpoint: report to the user in text; no more tool calls
  this turn]`; the tools stay in the request. After two refusals `circling_report` (calls,
  minutes, files written with counts, shell count, the longest failing run and its last output
  line) is the reply and ends the turn. A text reply after the checkpoint ends the turn with no
  stop hook or gate, so the plan hold never holds it.
- `TurnPolicy::checkpoint` is on for the conversation presets and off for unattended runs and
  children. Ablation `checkpoint`. Log lines start `[circling]`. `bench/acp_convo.py` counts
  `[checkpoint` as a refusal.

Testing caught a bug: the refusal branch also caught text replies; it now applies only to
replies with calls. Tests: four unit tests in `ladders.rs` and four loop tests in
`tests/agent_loop.rs`. On the Zed logs the ladder reaches stage 2 in 215800, 083226 08:36 and
12:12, 150502 and 020709 (all at 80 calls), and not in 190002; 083226 08:59 (46 calls over
49 min) is missed. acp_1105_074445 (longest turn 17 calls) logged no `[circling]` line. Not
measured yet: a long or failing live turn that shows the ladder firing or staying quiet.

## 2026-11-05: Gemma 4 native tool calls salvaged from text (boubou-5hg)

LM Studio session acp_1105_001847 (`~/.rufous-lmstudio/sessions`, 00:53:35) ended its turn on a
text reply that was a tool call in Gemma 4's own syntax:
`<|channel>::<|channel>call:todo{action:<|"|>update<|"|>,done:true,index:1}<tool_call|>`. The
model opened the thought channel twice and dropped the `<|tool_call>` opener, so LM Studio's
parser passed it through as content. The salvage in `guards/leak.rs` missed it: `PLACEHOLDER`
covers only one `<|"|>`-quoted string argument, and `FUNC_CALL` needs `name(k=v)`. The turn
ended on "text reply ends the turn", Zed showed the raw tokens, and the plan was left open. The
same leak can reach us from Ollama when its gemma4 parser misses an opener.

The syntax is unambiguous, so the new shape is not profile-gated.

Changes:
- `parse_gemma_native` is a new salvage shape, tried after the JSON shapes and before
  `PLACEHOLDER`. It needs one of the Gemma markers in the text (`<|"|>`, `<|tool_call>`,
  `<tool_call|>`, `<|channel>`, `<channel|>`), so prose that mentions `call:x{}` is left alone.
- Closed `<|channel>…<channel|>` thought spans are dropped first. Each `call:NAME{…}` then
  needs a known tool name or a dotted MCP name.
- `GemmaReader` reads the arguments: bare keys, `<|"|>…<|"|>` or JSON strings (which may hold
  commas, braces and newlines), numbers, `true`/`false`/`null`, arrays and objects. A call
  whose arguments do not read cleanly to the closing brace is not salvaged. Several calls in
  one reply are all salvaged, in order.
- `Salvaged.gemma` marks the shape, and the log line reads `⚡ salvaged tool call(s) from text
  (gemma native): …`. It sits under the existing `salvage` ablation.

Five tests. Four in `tests/guards.rs`: the exact 00:53 leak; arrays, nested objects and
strings containing `}` and `,`; two calls in order with a closed thought dropped; and an
unknown name, a truncated call, a bare word and marker-free prose all rejected. One in
`tests/agent_loop.rs`: the leaked call runs and the turn goes on instead of ending.

The user reports no more leaked Gemma call tokens in Zed (2026-11-05). No `gemma native`
salvage has fired in the logs since the install; the one session since ran on Ollama, where the
leak was not seen. Live reading: grep `gemma native` in the session logs of both homes.

## 2026-11-05: the backend is named by a flag or a config key (boubou-56g)

Choosing LM Studio over Ollama took a separate `RUFOUS_HOME` with its own `config.json` whose
`local_backend.type` was `openai`. The Zed entry showed no backend, and nothing logged which
engine ran or which config chose it. A running agent kept the old choice after the settings
changed. At 01:23 on 2026-11-05, pid 79106 was still on `RUFOUS_HOME=~/.rufous-lmstudio` after
the user had removed it from Zed, and a failed request to LM Studio read `agent loop: ollama:
error sending request for url (http://127.0.0.1:1234/v1/chat/completions)`. Model ids also
differ per engine (`gemma-4-12b` against `gemma4:12b`), and nothing warned.

One choice, one resolver.

Changes:
- `BackendKind` (`ollama`, `lmstudio`, `openai`) is a new top-level config key `backend` and a
  global CLI flag `--backend`. The flag is kept in `Config::backend_flag` (`#[serde(skip)]`), so
  it is never written to `config.json`. The web UI's per-request config reads take it too
  (`AppState.backend_flag`).
- `Config::local_backend()` resolves the flag, then the `backend` key, then the older
  `local_backend.type`, then Ollama. `lmstudio` uses `local_backend.base_url` when set, else
  `LMSTUDIO_BASE_URL` (`http://127.0.0.1:1234/v1`).
- `openai` needs `local_backend.base_url`. Named without one, `backend_problem` makes
  `Agent::standard` and `rufous acp` fail with a config error naming the key, instead of
  quietly using Ollama.
- A named backend turns an enabled cloud provider off. Cloud is used only when no backend is
  named, as before.
- `OpenAiEngine.label` (`LM Studio` or `OpenAI-compatible`) names the engine. At run start the
  loop logs `[engine] <label> · <url> (from <source>)`. `backend_source` gives `--backend`,
  `config.json backend`, `config.json cloud`, `config.json local_backend` or `default`.
- `model_id_warning` logs `[engine] ⚠️ …` for a model id written for the other engine: on
  Ollama, an id that `profile::ollama_tag` rewrites (`hf.co/` ids exempt); on an
  OpenAI-compatible engine, an id with `:`. It is a warning only; nothing is rewritten.
- `Error::Ollama` now reads `model server: <engine>: …`.
- `docs/acp.md` shows a Zed entry with `--backend lmstudio`. `RUFOUS_HOME` keeps working.

Seven tests in `tests/openai_backend.rs`: the resolver order and its sources, the LM Studio
default URL and the `openai` problem, the key read from JSON with the flag never written, cloud
losing to a named backend, the two model-id warnings, the error text naming the server, and the
agent refusing to start on `openai` without a URL. On the release build, `--help` lists the flag,
`--backend openai` without a URL exits 1 with the config error, and clap rejects an unknown name.

Session acp_1105_074445 logs `[engine] Ollama · http://127.0.0.1:11434 (from --backend)` on
every turn. The user confirmed the change on 2026-11-05.

## 2026-11-05: claims hold (boubou-bcs)

The reply that ends a Zed turn claimed outcomes that no check backed. In acp_1104_132038
(gemma4:12b) and 112408, no check command of any kind ran in any turn, yet the
replies said "The UI successfully displays a list of three sample todos…" and "The backend
infrastructure is now fully set up". The Tailwind classes it added did nothing; the user found
that by looking. In acp_1103_083226 (qwen3.5:9b), turns 1 and 2 ended "Fixed the 404 error…"
with no curl run. Gate 2b Claims existed, but it ran only at `finish`, the coding preset did not
take it, and its detector looked for test-style claims, not behaviour claims.

Offline measurement first (`bench/claims_replay.py`, every Zed transcript since 11-02, both
homes): 106 turns, 44 wrote a file, and 30 of those ended with a reply. Exit codes alone back
nothing. In acp_1103_083226 every command exited 0 while its output said `FAILURE: Build
failed`, `Page Not Found` or `index.html also 404`, because a `| tail` or `|| echo` swallowed the
status. Inline `python3 << PYEOF … open()` scripts that rewrote files looked like runs.

Changes:
- `outcome_claim` finds an outcome claim (`OUTCOME_CLAIM`: "Fixed the…", "now loads", "works
  correctly", "is now functional", "<feature> … is complete"), a claim to have checked
  (`DID_VERIFY_CLAIM`: "verified that…", "has been tested") or the old `VERIFIED_CLAIM`. A reply
  that hedges (`CLAIM_HEDGE`: "not yet run", "please verify", "still fails", "unable to")
  passes. `admits_failure_about` was not used as the escape, since "Fixed the 404 error" counts
  as admitting one.
- `CommandRecord.output_failure` keeps a shell result's first line that matches `FAILURE_MARK`
  (`FAILURE`, `Build failed`, `Traceback`, `error:`, `Not Found`, `404`, `Connection refused`, …),
  whatever the exit code. Only this hold reads it.
- `shell_writes` counts heredocs, redirects, `tee`, `sed -i`, inline interpreter code, installs,
  file-rewriting git commands (`git checkout` among them) and an `rm` of anything but logs, temp,
  output and pid files. `runs_something` counts a check, a build or run task, a script, the
  program itself or an HTTP request.
- `claim_backing` returns `NoWrite`, `Backed` or `Unbacked`. A claim is backed by a run after the
  last landed write that exited 0 with no failure line, or by the user saying it works in this
  turn's instruction (`USER_REPORTS_WORKING`).
- `claims_hold_step` runs where a text reply ends a conversation turn, after the plan hold, and
  at `finish`; 6 of the 10 replies the first replay held ended on `finish`. It does not run
  on the question route or in unattended runs. The first unbacked claim is held with `claims_hold_note`, which quotes
  the claim and names why. The log reads `⚠️ [claims] hold: "<claim>" (<why>), reply held`; a
  second one is let through with `[claims] released: … still unbacked`.
- `Gate::Claims` joins the coding and general presets, which also turns on the finish-time Gate
  2b there. Ablation switch `claims_hold`.

`examples/claims_replay.rs` runs the Rust detector over the replay's export and agrees with the
Python replay turn for turn: 9 holds and 4 backed. Two corrections came from that comparison:
`git checkout` after the build is a write (083226 turn 1 now held), and removing `build.log` or
`run.log` is not (215800 now backed by its passing run). All 3 "still broken" complaints that
followed a claim in the data come after a held reply. Accepted misses: design-intent wording
("ensuring a consistent typography") and "Completed X" summaries, which the plan hold covers.

Nine tests: four in `tests/guards.rs` (claims found and descriptions not; failure output read
whatever the exit code; what writes and what runs; a claim needs a clean run after the last
write) and five in `tests/agent_loop.rs` (held once then backed by a clean run; a failing run
does not back and a second claim is released; a reworded reply ends the turn; a `finish` is
held the same way; no write, a question, the ablation or an unattended run never holds).

Known: `is_verification_command` reads only the first Gradle task, so the log names
`gradlew stop` over `gradlew clean test` in 215800; the verdict is unaffected. Not measured
yet: `[claims]` lines in live Zed sessions per model, and the share of holds that end in a
check rather than a reword. The bead is still open.

## 2026-11-05: `rufous acp --policy` and HTTP MCP servers (boubou-5xj)

This is Choreo's item 1: a desktop app drives rufous over ACP for everyday work, not code.
`rufous acp` ran every session under the coding preset, and it did not advertise HTTP MCP
servers. rufous already served the HTTP servers a client passes in `session/new`, but a client
that follows the spec sends them only when the agent says so.

Changes:
- `rufous acp --policy coding|general` picks the turn policy for every session. `coding` (an
  editor's agent, with the coding skills) is the default. `general` is the web chat's preset and
  skills. The batch harness (`unattended`) is not offered over ACP.
- `StandardFactory` carries the `policy` and its `skills`; `StandardFactory::coding` builds the
  editor's factory. `turn_params` takes the policy instead of always using the coding preset.
- The start is logged as `[rufous acp] policy <name>`.
- The `initialize` response sets `mcpCapabilities.http` (`McpCapabilities::new().http(true)`).
- `docs/acp.md` and the README describe the flag.

Tests: `acp_policy_is_coding_unless_asked_for_general` in the CLI's unit tests (the default,
`--policy general`, and `--policy unattended` rejected as an invalid value), and
`tests/acp_turn.rs` now asserts `mcp_capabilities.http` in the `initialize` response.

## 2026-11-05: host tools (boubou-c9a)

Choreo's item 2. A desktop host starts `rufous acp --policy general` and passes its own MCP
server in `session/new`, one tool per automation. Session `acp_1105_105519` failed: the model
reached for `skills load "Say Hello"` and replied that no such automation exists. Session
`acp_1105_105645`, with the prompt worded around MCP, worked first time. The code showed why:
- A question turn got `face.chat_tool_defs()` and no MCP tools at all.
- On a task turn the server had no categories, so its tools landed in `mcp`, which routes only
  on words such as "mcp", "plugin" or "api" (`CAT_KEYWORDS`).
- `McpRegistry::tool_defs` cut every description to 60 characters.
- The prompt said "call enable_tools first" and did not name the tools.

The design is in docs/host-tools.md. A server the client passes is the host's own tool set, not
a library to discover. Its tools are on the face on every turn and every route, and the system
prompt names them. The set is fixed for the session, so the tool list stays byte-identical from
turn to turn and the prefix cache holds (boubou-g11). `mcp.json` servers keep their routing.

Changes:
- `ServerConfig.host` (`#[serde(skip)]`, never read from `mcp.json`). The runner sets it on the
  `session/new` servers when host tools are on.
- `rufous acp --host-tools on|off`. Without the flag it is on under `--policy general` and off
  under `coding` (`acp_host_tools`). The start line says `policy …, host tools on|off`.
- `McpRegistry::host_face` returns a `HostFace`: every tool of every host server, with no
  category, `expose` or built-in-duplicate filter, sorted by qualified name. Offline drops a
  `url` host server, as before. `tool_defs` skips host servers, so nothing is listed twice.
- Host tools keep up to 200 characters of description (`HOST_DESC_CAP`); routed tools keep 60
  (`DESC_CAP`). The probe now stores descriptions up to `HOST_DESC_CAP`, not 150.
- At most `HOST_TOOL_CAP` = 15 host tools are inlined; `RUFOUS_HOST_TOOL_CAP` overrides it for
  the bench. Past the cap the names stay in the prompt, the schemas go, and `mcp_call` is put on
  every route (`face.host_fallback`).
- `face::host_note` adds a `HOST TOOLS:` line to the task and chat prompts that names the tools.
  The log line is `[face] host tools: N inlined|over the cap, by mcp_call: …`.
- A host-tool call counts as productive for the anti-loop counters, whatever its name.
- `bench/host_mcp.py` is a stand-in for Choreo's server (loopback HTTP, one tool per automation,
  every call recorded). `bench/host_tools.py` sweeps the tool count over 4 host prompts and 3
  plain ones, one fresh session per prompt.

The cap is set by accuracy; prefill is recorded, not gated (80bf278). A percentage rule is
unstable when the base is under a second (gemma4:e2b) or noisy (qwen3.5:4b measured faster at
N=5 than at N=0). The +25 % prefill rule would have given 10 for qwen.

Reading, 2026-11-05 (`bench/results/host_tools/2026-11-05`, N=0, 5, 10, 15, 25, 3 runs):
- Plain prompts made no host call, 9/9 at every N on both models.
- Choreo's prompt passed 3/3 on qwen3.5:4b up to N=15 and 2/3 at N=25, where qwen dropped the run
  id it was asked to quote. gemma4:e2b passed 3/3 at every N.
- About 75 prompt tokens per host tool. qwen went from 1738 tokens and 3.3 s at N=0 to 3083 and
  4.4 s at N=15; gemma from 1406 to 3114 tokens and 0.44 to 0.92 s over N=0–25.
- "What did Say Hello return for Ada?" in a fresh session failed on qwen at every N (0–1/3), so
  it is not a cap effect: boubou-8c4.

Tests: `host_servers_are_assembled_whole_sorted_and_long_described_not_routed`
(`tests/mcp.rs`); a question turn is offered the host tools and the prompt names them, and past
the cap the tools ride on `mcp_call` (`tests/agent_loop.rs`); the flag default follows the
policy (`rufous-cli`). The safety gate still does not vet MCP calls; Choreo approves each run on
its side.

## 2026-11-05: host tool mentions (boubou-8c4)

The 2026-11-05 sweep (boubou-c9a) found that in a fresh session qwen3.5:4b answers "What did
Say Hello return for Ada?" without calling `choreo.say_hello`: 0–1 of 3 at every N. gemma4:e2b
passed 11 of 12. The tools were sent (`acp_1105_123214.request.json`), and the route is the same
as Choreo's original prompt, which passes. The replies either guessed from the description or
said there was no record. docs/host-tool-mentions.md gives three causes in the text the model
reads: `TASK_BASE` opens by telling it not to call a tool for greetings ("hello"); the user says
"Say Hello" where the list says `choreo.say_hello`; and "what did … return" reads as a question
about history.

Changes (a91d374):
- `HostFace.titles`: a display name per tool, the MCP `title` (or `annotations.title`), else the
  tool name with `_` and `-` read as spaces. The `HOST TOOLS` line shows `choreo.say_hello (Say
  Hello)`.
- `face::host_mentions`: a message mentions a host tool when its display name, or its qualified
  name, occurs on word boundaries, ignoring case and punctuation. A one-word display name is never
  a mention on its own. The instruction is `qa::task_routing_text`, and the log line is `[host]
  mention: …`.
- Arm A, `host_mention_note`: a `Hint: "Say Hello" is the app's automation choreo.say_hello. If
  the user wants it run or wants its result, call it; it runs now.` goes in after the user
  message, once per turn, only on a mention.
- Arm B, `host_mention_hold`: a text reply that ends a turn whose instruction named a host tool
  that never ran is held once.
- `bench/host_tools.py --arm none|note|hold` and four new prompts: "What did Say Hello return for
  Ada?", "Has Weekly Report run today?", "What does Tidy Downloads do?" and "Don't run Weekly
  Report, just tell me what it would make."

Reading, 2026-11-05 (`bench/results/host_tools/2026-11-05_arm_{none,note,hold}`, N=5 and 15, 3
runs; runs where the named automation was called, out of 6):
- "What did Say Hello return": qwen 1 with no arm, 6 with the note, 6 with the hold; gemma 6 in
  every arm.
- "Don't run Weekly Report": 0/0 with no arm and with the note, 5/1 (qwen/gemma) with the hold.
- "What does Tidy Downloads do?": 0/0, 0/3 and 6/6. "Has Weekly Report run today?": 1/4, 4/6
  and 6/6.
- "Hello!" and the plain prompts made no host call in any arm.

The hold is removed (dcc1a65). Held once, the model answered with the call whatever the user
said. It ran Weekly Report after "Don't run Weekly Report", and Tidy Downloads, which moves
files, on a question about what it does. Every Choreo call is a real run with real effects. The
note is kept and is on by default; the bench's default arm is `note`. With it qwen calls
`say_hello` 6/6, against 1/6 before, and the note caused no false calls.

The note misses the design's 8-of-9 bar only on quoting. Three of qwen's six replies gave "Hello,
Ada!" without the `(run …)` id the result carried; gemma quoted it whole 6/6. That is bead
boubou-caf.

Tests in `tests/agent_loop.rs`: a named host tool gets the note before the first call; a reply
that skips it ends the turn ("Don't run Say Hello, just tell me what it does.", one reply, no
hint); a greeting is not a mention of a two-word tool. In `face.rs`, mentions need the whole
display name or the qualified name. In `tests/mcp.rs`, the display name is the title, else the
name read as words. Open: whether Choreo's tools will carry an MCP `title`, and whether "What
does X do?" should ever run X.

## 2026-11-05: rufous as a sandboxed helper (boubou-f8d)

Choreo's item 3. Choreo ships rufous in `Choreo.app/Contents/Helpers`, signed with
`app-sandbox` + `inherit`, and starts `rufous acp --policy general` itself. Inside the sandbox
`HOME` is Choreo's container, `ollama` cannot be started, and a path the user did not share fails
with `EPERM`. Checked on 2026-11-05 (docs/sandboxed-helper.md):
- The engine URL could only come from `config.json`.
- `rufous acp` started Ollama, restarted it after four 500s, and retried a refused connection
  for eight attempts, about 4.5 minutes, before the turn failed.
- A refused path read as `[not found: …]`, `[not a dir: …]` or `(no matches)`.
- The `Home directory:` line named the container.
- On stdin close a `run_bash` command was left behind, re-parented to launchd (`sleep 301`,
  ppid 1).

Changes:
- `--base-url <url>`, global: overrides `local_backend.base_url` for `--backend lmstudio|openai`
  and is never written to `config.json`. Without one of those backends it is an error, since
  Ollama's setting is `OLLAMA_HOST`.
- `--no-start-engine`, global (`Config::no_start_engine`): rufous never starts or restarts
  Ollama. At start it probes for 2 s and logs `[rufous acp] engine <host> not answering; not
  starting it (--no-start-engine)`, then serves anyway. A refused or unresolvable connection is
  `ChatError::Unreachable`. With `RunParams::engine_unmanaged`, `RetryLadder` has
  `fail_fast_unreachable` and gives up at once. The turn ends with `model server: <engine>: not
  reachable (<reason>); rufous does not start the engine under --no-start-engine`. Without the
  flag, `Unreachable` keeps the back-off.
- `--user-home <path>` sets `RUFOUS_USER_HOME`. `config::user_home` takes it, else nothing inside
  an App Sandbox (`APP_SANDBOX_CONTAINER_ID`), else the home directory. `fill_base` drops the line
  when there is none. Sandbox detection, not `RUFOUS_HOME`, because every bench sets `RUFOUS_HOME`.
- `[no access: <path> (not shared with this assistant)]` for `PermissionDenied` (`EPERM` and
  `EACCES`) in `read_file`, `delete_file`, `list_dir`, `search_files` and the writes. It is in
  `ERROR_OPENINGS` and `FAIL_MARKERS`. Other errors are unchanged.
- `exec.rs` keeps a registry of live process groups (`LiveGroup`). `run_bash` commands and stdio
  MCP servers (now `process_group(0)`) register. `rufous acp` calls `kill_live_groups()` when
  stdin closes and on `SIGTERM`, `SIGHUP` or `SIGINT`, then exits. An `ollama serve` that rufous
  started is not touched (boubou-t70).
- `scripts/macos-universal.sh` builds arm64 and x86_64 with `MACOSX_DEPLOYMENT_TARGET=11.0` and
  default features and joins them with `lipo`. It fails if `otool -L` lists anything outside
  `/System/Library/` and `/usr/lib/`, and prints `minos` and the SHA-256.
- docs/acp.md gains "Inside a host app", the table of the host's settings.

Tests: an unreachable engine gives up at once only when it is not ours (`ladders.rs`); it ends
the turn after one call, and a managed one is retried (`tests/agent_loop.rs`); a closed port is
`Unreachable` on both engines, and `--base-url` overrides the config and needs a backend that
takes one (`tests/openai_backend.rs`); a refused path is `[no access: …]` and live groups are
killed with everything they started (`tests/exec_tools.rs`); no `Home directory:` line without a
user home, and `--no-start-engine` fails fast (`tests/run_params.rs`). By hand with a fake
engine, a refused connection failed in 0.1 s with no Ollama spawned, and a `sleep` under
`run_bash` was gone after both stdin close and a signal. The workspace tests and clippy pass.
Not yet confirmed: that the hardened runtime with no entitlements works once Choreo signs the
universal binary.

## 2026-11-05: where data goes (boubou-45n)

Choreo's item 4. For every run, Choreo shows the user which hosts were contacted and from which
step; that record is its main privacy claim. rufous's own traffic bypasses Choreo, so rufous has
to report it. Checked on 2026-11-05, Choreo could not see redirects from `web_fetch`, the search
engines behind `web_search`, any `url` MCP server or its tools/list probe, or the model engine.
Offline mode worked by hiding tools, so a web tool reached another way (`batch_tools`, a name
remembered from an earlier turn) still ran. A LAN engine or an `OLLAMA_HOST` elsewhere was used
as normal.

Changes (design in docs/egress.md, contract in docs/acp.md "Where data goes"):
- `egress.rs`: `Egress { host, purpose, local }` and `EgressLog`. The host carries `:port` when
  the port is not the scheme's default; `local` is true for loopback. A cache hit records
  nothing.
- `Web::get` records `web_search` or `web_fetch`, and a redirect policy records every hop as
  `redirect`. `McpRegistry` records `mcp:<server>` when it calls or probes a `url` server.
- After each tool runs, the loop drains the log (`ToolRunner::take_egress`), logs `[egress] …`
  and sends `AgentEvent::Egress` before that call's result. The ACP bridge puts it on the call's
  `tool_call_update` as `_meta.rufous.egress = [{host, purpose, local}]`. Traffic outside a call
  (the MCP probes) becomes an Info `notice` titled `Contacted <hosts>`.
- The `session/new` and `session/load` responses carry `_meta.rufous.engine = {label, url,
  host, local}` and `_meta.rufous.offline` (`AgentFactory::session_meta`).
- A host's `_meta.rufous.offline: true` on `session/new` or `session/load` keeps that session
  offline (`Config::offline_forced`, which wins over `AGENT_OFFLINE=0`). It can only turn
  offline on.
- Offline is enforced in the executor: `web_search`, `web_search_multi`, `web_fetch` and calls
  to a non-loopback `url` MCP server are refused with `[offline: <tool> would reach the
  network; this session stays on this machine]`. Loopback `url` servers, such as a host's own,
  are kept.
- Offline refuses a model engine off this machine: `offline: the model engine at <url> is not on
  this machine`. This changes behaviour for anyone running offline with a LAN engine.

Tests: unit tests in `egress.rs`, `bridge.rs` and `runner.rs`;
`a_call_s_egress_is_reported_before_its_result_and_logged` (`tests/agent_loop.rs`); redirects,
cache hits and offline refusals of the web tools and a remote MCP server
(`tests/web_memory.rs`); `offline_means_this_machine_for_the_model_engine_too`
(`tests/openai_backend.rs`); the loopback host server kept offline (`tests/mcp.rs`); and
`a_host_can_keep_one_session_offline_and_is_told_the_engine` (`tests/acp_turn.rs`). Checked end
to end with the debug binary against a fake engine.

Not covered: `run_bash` and stdio MCP servers. rufous cannot see their traffic, and Choreo labels
shell use as unrecorded.

## 2026-11-05: permission waits that survive a restart (boubou-lbo)

Choreo's item 5. Choreo runs on an always-on Mac and may answer `session/request_permission`
hours later, from a notification, without reading the conversation. Checked on 2026-11-05, the
request carried only `{tool, action, target, workdir}`: not the new file text, not the command.
The diff arrived only after the call had run. And if rufous died mid-wait, `session/load`
replayed the previous turn's end. The user's prompt and the call asked about were both gone,
and nothing said so.

Changes (design in docs/permission-waits.md):
- The request carries the whole proposed action (`permission_fields`, `current_args` in the
  bridge). `rawInput.args` holds the call's complete arguments beside the existing keys. The
  content is the write's diff, the `run_bash` command, the `web_fetch` URL, or an MCP call's
  server, tool and arguments. The locations are added as for the `tool_call`.
- While a question is open, `drive_turn` keeps `<sessions>/<name>.asking.json` = `{tool, args,
  asked_at}`, and removes it on an answer, a cancellation or an error.
- `session/load` that finds the marker runs `session::recover_asking`. When the checkpoint ends
  in a tool-call message without results, it becomes the history. The asked call gets `[not
  run: rufous stopped while this call was waiting for the user's permission, …]`. An earlier
  call in the message gets `[interrupted: …]`, and a later one `[not run: rufous stopped before
  this call started, …]`. The meta status becomes `interrupted`. A marker without a matching
  checkpoint tail is stale and is removed.
- `[not run:` and `[interrupted:` join `FAIL_MARKERS` and `ERROR_OPENINGS`, so the replay shows
  the call failed.
- "Go ahead" asks again: the model makes the same call, and the gate asks, because an allow from
  an earlier turn never carries over.

Tests: `a_permission_request_carries_the_whole_proposed_action` (`bridge.rs`);
`a_turn_stopped_at_a_question_comes_back_with_its_calls_answered` and
`a_stale_marker_is_removed_and_changes_nothing` (`session.rs`); and
`a_turn_killed_at_a_permission_question_is_recovered_on_load` (`tests/acp_turn.rs`).

Checked by hand with the debug binary against a fake engine that proposes a `create_file`
outside the workspace. The request carried the args, the diff and the location, and the marker
was on disk. After a `SIGKILL` mid-question, the file was not written. On restart the replay
showed the prompt and the call failed with the not-run text. "go ahead" asked again; allowed,
the file was written and the turn ended `end_turn`.

Not changed: the wait has no timeout, and nothing on the rufous side wakes a sleeping host.

## 2026-11-05: per-call token usage on usage_update (boubou-70l)

Choreo's item 6. Choreo meters cost per run and per model, so it needs the input tokens, output
tokens, model id and engine of every model call. Checked on 2026-11-05, `usage_update` carried
only the context occupancy, after the main call. The compaction summary, the corrector's
consultation and the wrap-up were reported nowhere except, for some, the session log. The model
id the loop passed was not always the one billed: the cloud backend sends its `model_override`.

Changes (design in docs/call-usage.md):
- `chat.rs`: `CallUsage { model, purpose, input_tokens, output_tokens }` and `Metered`, a
  `ChatBackend` that wraps the loop's backend for the whole run. Every successful call records
  one entry; a failed or cancelled call records nothing. `purpose` is `turn`, `compaction`,
  `corrector` or `wrap_up`; `compact` sets and restores it itself.
- `ChatBackend::model_id` returns the id billed. `OpenAiBackend` returns its `model_override`.
- `ContextUsage` gains `call`. Side calls go out as `AgentEvent::ModelCall { call, used, limit }`
  with the last context numbers. Calls after the last main call are drained before `Finished`.
- ACP: each model call is exactly one `usage_update` with `_meta.rufous.call = {model, engine,
  purpose, input_tokens, output_tokens}`. `engine` is the same object as the `session/new`
  response's `_meta.rufous.engine`, so a host can tell free local calls from paid ones. A count
  the engine did not report is `null`, not 0.
- The CLI and the web UI ignore `ModelCall` beyond the existing `[timing:]` line.

Tests: `each_successful_call_is_recorded_under_the_billed_model` and
`a_failed_call_records_nothing` (`chat.rs`); `every_model_call_is_reported_once_with_its_purpose`
(`tests/agent_loop.rs`); `each_model_call_is_one_usage_update_carrying_the_call_and_the_engine`
(`bridge.rs`); `every_model_call_reports_its_tokens_model_and_engine` (`tests/acp_turn.rs`).

Also in this commit: the live-group kill test moved out of `tests/exec_tools.rs` into its own
binary, `tests/live_groups.rs`. `kill_live_groups` kills every group registered in the process,
so run beside the other exec tests it killed their running commands too.

Not covered: the children of a parallel dispatch, the local decision model, and a local engine
that streams without reporting usage, whose counts stay `null`.

## 2026-11-05: MCP tool schemas keep enums, required and descriptions (boubou-erw)

A request from Choreo's bizframe eval (qwen3.5:2b, qwen3.5:4b, gemma4:e2b over ACP, `--policy
general --host-tools on`). `compact_schema` (`mcp/manifest.rs`), ported from the Python harness,
kept only `{type}` per parameter:
- `enum` was dropped. bizframe's `set_entity.kind` (`master`, `document`, `log`) reached the
  model as a bare string, and `set_field.type` lost its 12 values. The models wrote free-text
  labels for `kind`, and gemma4:e2b put the field type in `options`.
- Parameter descriptions were dropped.
- A schema with no `required` list was sent as all-required, so `show.name`, which is optional,
  had to be passed.

Changes (design in docs/mcp-schemas.md):
- Every MCP tool, routed or host, keeps an `enum` that is a list of strings, numbers or booleans.
- `required` is kept as the server sends it; a missing or empty list means none. The rule that a
  tool with more than eight parameters keeps only the required ones applies only when some are
  required.
- The probe keeps each parameter description on one line, cut to `PARAM_DESC_CAP` (120
  characters). `flat_def` strips them again for routed tools; `host_face` keeps them, so only
  host tools send them. docs/host-tools.md says so.
- Not changed: a parameter without a `type` is still dropped, and array `items` are still not
  described. A cache entry written before this change expires within the five-minute TTL.

Tests in `tests/mcp.rs`:
`compact_schema_keeps_names_types_enums_descriptions_and_required_as_sent` and
`a_host_tool_keeps_parameter_descriptions_a_routed_tool_keeps_only_enums`.

Also in this commit: a race in `a_raised_stop_flag_terminates_a_running_command`
(`tests/exec_tools.rs`). The stop went up after a fixed 300 ms, which under a loaded suite was
before the shell had printed. The command now touches a `ready` file, and the flag goes up once
it exists.

Not measured yet: Choreo's bizframe before/after bench on the three models, run against the
commit that closes this bead.

## 2026-11-05: the MCP tool cache follows each server's config and binary (boubou-gj0)

A request from Choreo: when a host app rebuilds its MCP server (bizframe), rufous should not
serve the old tool descriptions for up to five minutes, if the fix is cheap. The cache
(`<home>/mcp_manifest_cache.json`, `{ts, data}`) kept one timestamp for the whole file and was
served as it was within the TTL:
- a rebuilt server, or one with new `command` or `args`, was served its old tools;
- a server that arrived in `session/new` under a fresh cache got no tools until the TTL ran out.
  `rufous mcp add` and `remove` delete the cache, but `session/new` bypasses them;
- a removed server's tools were still listed.

Changes (design in docs/mcp-cache.md):
- `fingerprint` hashes the server's serialised configuration (`command`, `args`, `env`, `url`,
  `headers`, `categories`, `tools`, `expose`) and, for a stdio server, the size and modification
  time of its command binary (`command_path`: the path, or the first match on `PATH`). Files
  named in `args` are left out: bizframe's `--app` file changes with every edit. Headers and env
  are hashed, never stored.
- The cache file keeps Mingbird's `{ts, data}` and gains `servers: {name: {fp, ts}}` (`Stamp`). A
  file without it, or a server missing from it, counts as stale.
- Only servers whose stamp is stale or does not match are probed. A failed probe keeps that
  server's previous tools and retries after 60 s. `force` probes them all.
- The manifest returns configured servers only. Entries for servers this registry does not
  configure stay in the file for a day (`PRUNE_AFTER`).

Four tests in `tests/mcp.rs`: `a_changed_server_is_probed_again_and_only_it`,
`a_server_added_under_a_fresh_cache_is_probed_and_a_removed_one_is_not_served`,
`a_changed_server_whose_probe_fails_keeps_its_old_tools_and_retries_soon` and
`a_cache_file_in_the_old_shape_is_read_as_stale`.

Not covered: a server launched through an interpreter or launcher (`node server.js`, `uvx
bizframe`), whose code changes while the binary stays the same, and a `url` server. Both refresh
at the TTL, or sooner when their configuration changes.

## v0.4.0 (2026-11-06)

CI run 42151908646 on 7df9edf is green on all three systems.

v0.4.0 adds the following since v0.3.2. It is a minor release because `rufous acp` gains new
flags and new protocol fields for desktop hosts.

ACP and desktop hosts:
- `rufous acp --policy coding|general` picks the turn policy for a desktop host, and the agent
  advertises `mcpCapabilities.http` so a client that follows the spec sends its HTTP MCP servers
  (boubou-5xj).
- Host tools: the MCP servers a client passes in `session/new` reach the model on every turn and
  are routed, with 200-character descriptions and display names on the HOST TOOLS line. Above 15
  tools (`RUFOUS_HOST_TOOL_CAP`) the rest go through `mcp_call`. Turn on with `--host-tools`; it is
  on by default under `--policy general` (boubou-c9a). When the instruction names a host tool, a
  note is added before the call. The end-of-reply hold was benched and removed, because it ran
  tools the user had told it not to run (boubou-8c4; docs/host-tool-mentions.md).
- Sandboxed helper: `--base-url`, `--no-start-engine`, `--user-home`, `[no access: …]` for
  permission errors in the file tools, and child process groups killed when stdin closes or on
  a signal; `scripts/macos-universal.sh` (boubou-f8d; docs/acp.md "Inside a host app").
- Egress: each tool call reports the hosts it reached in `_meta.rufous.egress`. The
  `session/new` and `session/load` responses name the model engine and whether offline is on.
  A host can keep one session on this machine with `_meta.rufous.offline`. Offline mode now
  refuses the web tools, keeps loopback MCP servers and refuses a model engine on another
  machine (boubou-45n; docs/egress.md).
- A permission question survives a restart: the request carries the whole proposed action, and
  `session/load` recovers a turn that was waiting for an answer (boubou-lbo;
  docs/permission-waits.md).
- Every model call in a turn is reported as a `usage_update` with
  `_meta.rufous.call` (model, engine, purpose, input and output tokens), including compaction and
  the wrap-up call (boubou-70l; docs/call-usage.md).

Model engines:
- `--backend` flag and config `backend` key (`ollama`, `lmstudio`, `openai`). The log names the
  engine and where the choice came from, and request errors name the engine (boubou-56g).
- Model profiles also match LM Studio and Hugging Face model ids (boubou-eie).
- Gemma 4 tool calls that the server passes through as text are recovered and run (boubou-5hg).
- A reply timeout ends the turn with a message instead of sending the request again
  (boubou-yqu).

MCP:
- MCP tool schemas keep their enums and the required list as the server sent it. Host tools also
  keep their parameter descriptions (boubou-erw; docs/mcp-schemas.md).
- The MCP tool cache is checked per server against its configuration and binary, so a rebuilt,
  added or removed server is seen at once (boubou-gj0; docs/mcp-cache.md).

The agent loop:
- Claims hold: a reply that says a change works is held once unless a clean run after the last
  write shows it (boubou-bcs).
- Plan hold: the plan check also runs when a turn ends with a text reply, and a todo update that
  changes nothing does not count as progress (boubou-ear, boubou-uzl).
- Circling ladder: after many calls or repeated failing runs of one command, the model is asked
  for a hypothesis, then a checkpoint ends the turn with a report. This applies to conversations
  only, not unattended runs (boubou-m5d).
- A follow-up turn keeps the session's tools, so the server's prefix cache still matches
  (boubou-g11). The log compares each request's prefix with the previous turn's (boubou-xwi).
- Compaction does not run again straight away: it shortens old call arguments and re-arms only
  below 45 % (boubou-4e1).

## 2026-11-10: images in ACP prompts, and images on every engine (boubou-bs3, fky, 080, 2cr)

Asked for by bizframe (`../bizframe/docs/requests/rufous-acp-image-prompts.md`), which drives a
small vision model through `rufous acp` to read a scanned form or receipt into a record.

- ACP: `promptCapabilities.image` is advertised. `bridge::prompt_input` turns image blocks and
  `image/*` blobs into `[image N: <mime>, <size>]` markers and hands the base64 to
  `history::queue_image`, the queue `read_file` already fills; `lib.rs` empties the queue at
  the start of each prompt and queues into the session's harness directory, so the loop's
  `attach_pending_images` hangs the images on the prompt's message before the first request.
  The loop itself is unchanged. `queue_image` is now the one place the 20 MB and 4-image limits
  live (`read_image` calls it). On Ollama, `chat::model_can_see` (`/api/show` `vision`, cached
  with `model_can_think` in one capability cache) keeps images from a model without vision:
  marker plus notice.
- Pruning (fky): `IMG_KEEP_MSGS` counted image-carrying messages, so one image rode along on
  every request for the rest of the session. `prune_images` now also drops images from before
  the previous turn (`IMG_KEEP_TURNS = 2`; turns split as `reattribute_declined` splits them,
  now `history::turn_starts`).
- Storage (080): `history::for_storage` writes the session file and the checkpoint with only
  the last turn's images; the transcript keeps none. A 5 MB picture was rewritten into
  `<name>.json` every save and replayed after `session/load`.
- OpenAI-compatible engines (2cr): `to_openai_messages` sent no images, while `read_file` told
  the model one was attached. A user message's images go as `image_url` parts; tool results'
  images follow the run of results as one user message, since nothing may stand between calls
  and their results. The mime type comes from the base64's first bytes. The 400 that drops an
  image now also matches the OpenAI wording ("only supported by certain models", "does not
  support images"), and `IMAGE_DROPPED_NOTE` says the model may not take images.

Checked live on `gemma4:e4b`: a generated receipt read correctly (merchant, date, total) over
`rufous acp` on Ollama's native API and on its OpenAI-compatible `/v1`; after the third turn
the saved session and checkpoint held no image and the transcript no base64. The two-turn
drop itself is checked in `acp_turn.rs` with a scripted model.

