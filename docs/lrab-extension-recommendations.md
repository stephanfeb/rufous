# Improving and Extending LRAB for a Rust Mingbird Port

*As of 2026-10-28*

Parity with Mingbird's LRAB score is a useful floor, but LRAB as published is a weak instrument for the next step. At 12B Mingbird already scores 0.906, the paper's own ablation could not separate any single mechanism from noise, and the authors list most of the gaps themselves. The recommendations below fall into five groups: sharpen the measurement, add mechanism-level micro-benchmarks, raise the task ceiling, score more than artefacts, and add dimensions LRAB does not have.

Source throughout: [Mingbird: A Local-First Agent Harness Enabling Small Open Models to Complete Real Tasks](https://arxiv.org/pdf/2610.02001) (arXiv 2610.02001, Oct 2026).

---

## 1. Fix the measurement before adding tasks

**Replicate every cell 3–5× and report task-level bootstrap CIs.**
The paper found same-night reruns of an identical arm moved the 18-task mean by up to 0.069 and single long-horizon cells by ±0.5. Any improvement smaller than that is invisible under single-trial scoring.

**Interleave execution round-robin across arms and tasks.**
Mingbird's campaigns were blocked by arm over different weeks; the authors flag this as temporal confounding. Randomize or round-robin (task, arm) order so machine state and background load cannot drift with the arm.

**Split the suite into a dev set and a frozen held-out set.**
Three of the four ablated mechanisms were tuned on LRAB tasks (the anti-loop guard literally against WF-08). Since tasks come from seeded generator scripts, generate fresh parametric variants for the held-out side rather than reusing the 18 fixed instances. Freeze the held-out set before touching the harness.

**Hide the acceptance tests.**
On WF-04 and LH-03 the finish gate runs the same test files the scorer runs, which inflates the gate's measured value. Keep visible smoke tests the agent may run, and hidden tests only the scorer sees.

---

## 2. Add micro-benchmarks for each failure form

The 18 end-to-end tasks are low-power for mechanism work. Build cheap, high-n probes (50–200 trials each, one mechanism per probe) and treat them as unit tests for harness behaviour.

| Probe | Setup | Measure |
| --- | --- | --- |
| Loop induction | A command that always fails | Turns-to-intervention, recovery rate |
| Output budget | A tool returning 200 KB–2 MB | Run survival, continuation in chunks |
| False finish | "Done" is only true after tests pass | Claim-vs-ground-truth mismatch rate, not just final score |
| Format rescue | Prompts that elicit plain-text tool calls | Salvage rate, wrong-salvage rate |
| Self-correction | One deliberately failing tool call | Whether the next call is different and valid |
| Schema-valid emission | Per model × backend × quant | Valid tool-call rate (standing gate) |

The schema-valid emission probe is the model-side floor Mingbird used to reject candidate models. Make it a standing gate so a parser problem is never attributed to the harness.

Mingbird left **M2 (chat/task layering)** and **M5 (format rescue)** entirely unquantified. Those are open ground for mechanism-level evidence.

---

## 3. Raise the ceiling so 12B can still be separated

- **Context-pressure tier.** The same tasks with a 2,000-line log to grep, verbose test output, and an edit target deep in a 1,500-line file, run at 16k / 32k / 64k context. Mingbird's 12B cells peaked at a median of ~14.6k prompt tokens inside a 32k budget, so context management was barely exercised.
- **Convention-following tasks.** An existing codebase with a style the agent must match (test naming, module layout). Score collateral diff, not just artefact presence.
- **Environment-recovery tasks.** Missing dependency, wrong working directory, stale lockfile. These hit the "self-correction diverges" failure form in realistic shape.
- **More long-horizon cells with kill-and-resume at random points**, not only at half budget (LH-03's current design).
- **Multi-file refactors** where partial completion is detectable, so partial credit is meaningful.

---

## 4. Score more than artefacts

Report per cell, alongside the artefact score:

- Tool-call count
- Peak prompt tokens
- Wall-clock
- Loop interceptions
- Finish refusals
- Malformed calls
- **Integrity**: completion claims that contradict ground truth

The paper found 2B models restating refused actions as done, and no gate closes that; track it as a number. Then plot score vs. tokens as a Pareto frontier, because for local users the efficient frontier matters more than the mean.

---

## 5. Dimensions LRAB has none of

- **Platform.** LRAB is single-machine Windows/Ollama. A Rust port should run the matrix on Linux and macOS and on llama.cpp as a second backend. agent-mini's collapse at 35B was a shell-platform effect; your users will not all be on Windows.
- **Serving sweeps as benchmark axes.** Quant (Q4_K_M / Q6 / Q8), KV-cache quant, context size, thinking on/off, temperature. Thinking alone swung opencode's τ²-bench telecom score from 0.30 to 0.99.
- **A second small-tier family per size point.** Tiers and families are confounded in Mingbird's matrix (one model each).
- **External anchors at local scale.** Keep τ²-bench, and add a coding-native check (aider's polyglot set or a mini-swe-agent subset) so there is a non-self-authored coding benchmark.
- **A small human baseline**, which the authors name as the single most valuable addition.

---

## Where to look first

A parity score says nothing about *which* residual failures you share. Run Mingbird's failure-form coder (or your port of it) over your own transcripts and mine the cells scoring ≤ 0.5. At 12B, Mingbird's misses were concentrated in WF-09 and the long-horizon tasks; your residuals may not be the same ones.

---

## Checklist

- [ ] 3–5 replications per cell with bootstrap CIs
- [ ] Round-robin execution order across arms and tasks
- [ ] Dev / held-out split from seeded generators
- [ ] Hidden scorer tests separate from visible smoke tests
- [ ] Six micro-benchmark probes at n ≥ 50
- [ ] Schema-valid emission gate per model × backend × quant
- [ ] Context-pressure tier at 16k / 32k / 64k
- [ ] Convention, environment-recovery, random-kill, and multi-file refactor tasks
- [ ] Per-cell process metrics and integrity metric
- [ ] Score-vs-tokens Pareto plot
- [ ] Linux / macOS / llama.cpp matrix
- [ ] Serving sweeps (quant, KV quant, ctx, thinking, temperature)
- [ ] Second model family per tier
- [ ] External coding anchor (aider polyglot or mini-swe-agent subset)
- [ ] Failure-form coding of your own ≤ 0.5 cells
