# Design note: a System One decider for Boubou

*Draft, 28 October 2026. Status as of 29 October 2026: step 1 of section 6 is implemented
(`crates/boubou-core/src/decider.rs`, bead boubou-acy.1), see the addendum at the end. The
local backend is Ollama's native decision API rather than the Jev-compatible endpoint this note
assumed; the llama-server logits spike (step 2) is dropped as unnecessary.*

Companion to `system-one-models.md` (research notes gathered by another agent; the claims in
it are treated as unverified until we reproduce them).

## 1. Why

Boubou makes a dozen small decisions per turn by keyword list, regular expression or numeric
threshold. They are cheap, deterministic and testable, and they fail in a characteristic way:
a case the list did not anticipate flips the whole loop. The gemma4 chat transcript of
2026-10-28 was one such case. "Are you capable of software development ?" is 41 characters
with no question word, so it was routed as a task, every text reply drew a "continue with
tools" note, and the model cycled three sentences until the iteration limit. The fix was
another rule (a trailing `?` counts), which is the pattern we want to stop repeating.

System One models (Jev and its open-weights counterparts) answer exactly this class of
question: given a block of state and a typed question, return a choice, a score or a yes/no
with a calibrated probability, in one forward pass, with no free text. That is a better tool
than a keyword list for "is this a question", "is the model stuck", "is this item safe to
delegate", and the probability lets the loop behave differently at 0.95 than at 0.6 instead
of flipping on a hard boundary.

Two things this note does **not** propose: replacing the main model's tool choice (that is the
agent's job), or softening the safety floor (section 4).

## 2. Where Boubou decides today

| Decision | Code | Mechanism | System One shape |
|---|---|---|---|
| Question / task / follow-up | `guards/qa.rs::is_qa` | `TASK_HINTS`, `QA_HINTS`, trailing `?`, 40-char rule, `has_task_progress` | Choice {question, task, follow_up} |
| Tool categories to load | `agent/face.rs::route_categories` | `CAT_KEYWORDS`, default to files+code+web | multi-label Choice over `CATEGORIES` |
| Safe to delegate to a child | `parallel/trigger.rs::classify_task` | `DEFAULT_VETO_PATTERNS`, `DEFAULT_SIMPLE_PATTERNS`, complexity words, file-token count | Score (mechanical 0..3) + Noul (touches anything outside one file) |
| Model is stuck | `agent/run.rs` text-only branch | `similar()` at 0.75 over the last four texts, `talk_streak` cap | Noul over the last N assistant turns |
| Hollow or overstated `finish` | `agent/run.rs` finish gates, `guards/artifacts.rs` | producing-tool seen, claimed files exist, todo synced, re-injected brief | Score: delivery satisfies the brief (0..3) |
| Context compaction | `agent/history.rs::compact_history` | L1 truncate old tool outputs, L2/L3 LLM summary on the resident runner | Score each message's importance in one pass; drop below threshold |
| Skills and MCP tools in the prefill | `skills` tool, `mcp/manifest.rs::tool_defs` | model asks, category routing | Choice over skill / tool descriptions for the task |
| Which model for the task | not ported (Mingbird `bench/v37` routing) | fixed per run | Choice over installed models |
| Time budget stated in the task | `guards/budget.rs::parse_time_budget` | regex over units and number words | Noul (a deadline is stated) gating the regex |

Ranked by value against effort for Boubou:

1. **Intent** (question / task / follow-up). Smallest surface, the failure class we keep hitting,
   and the easiest to measure against transcripts.
2. **Stuck detection.** Replaces a string-similarity threshold with a judgement over content;
   catches paraphrased loops the window misses.
3. **Dispatch classifier.** The veto and simple lists are a Score and a Noul written out by hand.
4. **Compaction scoring.** Highest payoff on small contexts (Boubou's whole thesis) but the
   largest change: it alters what the model sees afterwards, so it needs the most measurement.
5. **Skill / MCP pre-selection.** Trims the prefill; useful once the MCP catalogue grows.
6. **Model routing.** Needs several models installed; later.

## 3. Shape of the code

A trait in `boubou-core`, deliberately tiny, mirroring the three primitives:

```rust
/// A typed decision over a block of state. Implementations must be cheap enough to call
/// several times per turn and must never block the loop for longer than `timeout`.
pub trait Decider: Send {
    /// Pick one of `options`; every option gets a probability, summing to ~1.
    fn choice(&mut self, state: &str, question: &str, options: &[&str]) -> Decision<Vec<(String, f64)>>;
    /// Rate `state` against ordered `levels` (worst first); returns the expected level and
    /// the per-level distribution.
    fn score(&mut self, state: &str, question: &str, levels: &[&str]) -> Decision<(usize, Vec<f64>)>;
    /// Probability that the answer to `question` is yes.
    fn yes_no(&mut self, state: &str, question: &str) -> Decision<f64>;
}

pub struct Decision<T> {
    pub value: T,
    /// 0..1, the implementation's own confidence; heuristics report 1.0 on a rule hit and
    /// 0.5 when they fell through to a default.
    pub confidence: f64,
    pub source: DeciderSource, // Heuristic | Local(model) | Unavailable
    pub latency_ms: u32,
}
```

Three implementations:

- **`HeuristicDecider`** wraps today's lists. It exists so the call sites can be rewritten once,
  behaviour stays byte-identical by default, and every test keeps passing. Its confidence is
  honest: 1.0 when a rule matched, 0.5 on a fall-through default. That number is what lets
  the next implementation take over only where the heuristic was guessing.
- **`LocalDecider`** talks to a Jev-compatible HTTP endpoint. Cloudflare Clef and Open-Jev both
  expose the request format Jev defined (state + typed questions, one parallel pass), and
  JevK5 ships GGUF weights that run under the llama-server backend Boubou already drives for
  the OpenAI-compatible engine. Configured in `config.json` under a new `decider` section
  (`{"type": "systemone", "base_url": ..., "model": ..., "timeout_ms": 1500}`), off by default,
  reported by `boubou doctor` with a probe decision and its latency.
- **`LayeredDecider`** (the one the loop uses): heuristic first; if its confidence is below a
  per-call-site threshold and the local decider is configured and healthy, ask it; on timeout
  or error fall back to the heuristic answer and log once. This keeps the loop's worst case
  equal to today's behaviour.

Nothing in `agent/run.rs` branches on probabilities directly. Each call site asks a named
question through a small policy function (`intent_of(messages)`, `is_stuck(recent_turns)`,
`dispatchable(item)`), so thresholds live in one place and can be tuned from measurements.

### Engines, honestly

- **llama-server** (already supported): JevK5 GGUF; also the AnyJev route of reading logits
  from the *main* model for a Choice with no second model loaded, since llama-server returns
  `n_probs`. That is the cheapest possible deployment and worth a spike before anything else.
- **Ollama**: constrained output through the JSON-schema `format` field gives a Choice but, as
  far as we know, no calibrated probability. Verify whether its OpenAI-compatible endpoint
  returns `logprobs` on current versions; if not, Ollama users get the heuristic decider only.
- **Encoder-class models** (Laya 421M, openJev-verdict 151M): CPU-cheap, weak until fine-tuned.
  Boubou's transcripts are labelled data for exactly these questions (section 5), so a
  Boubou-tuned small encoder is a realistic later step.

## 4. Where the decider is not allowed

- `safety.rs` command and path gates, the unattended refusals, the child sandbox rules in
  `parallel/sandbox.rs`. These remain regex and deterministic. A decider may add a *soft*
  layer above them ("this looks risky, confirm with the user") and may never relax a refusal.
  The medicine paper's finding that robustness to rephrasing is the real differentiator cuts
  both ways: a classifier that changes its mind when a command is reworded is worse than a
  pattern that does not.
- Anything irreversible decided without a human: deletes, uploads, pushes.
- The main model's own tool selection. The decider shapes the prefill and the harness notes;
  it does not pick tools for the model.

## 5. Measurement before wiring

Every call site is adopted only after an offline comparison, in this order:

1. **Build the labelled set from what we already have.** Session files under
   `~/.ollama_agent/sessions`, the LRAB transcripts in `bench/results/` and the scripted-loop
   fixtures give, per turn: the user text, whether the loop treated it as Q&A, whether a
   finish was rejected and why, whether a repeat or talk cap fired. Hand-label 200–300
   intent cases and 100 stuck/not-stuck windows; that is an afternoon.
2. **Score heuristic vs decider** on accuracy, decision-reversal rate under light rephrasing
   (the robustness metric from the medicine paper, cheap to generate), latency on the two
   reference machines (16 GB laptop with an integrated GPU; 32 GB+ with a discrete GPU), and
   memory while the main model is resident.
3. **Replay, not just classify.** The scripted-loop harness in `tests/agent_loop.rs` can run a
   recorded transcript with the decider substituted for the heuristic and show the turn count
   and the notes injected. The 2026-10-28 chat transcript is the first replay case.
4. **LRAB cell check.** Tiers 1–3 with the decider on and off, same model, through
   `bench/lrab_h2h.py`. We know from the WF-08 work that one run per cell is noisy; use the
   mechanism micro-probes from `lrab-extension-recommendations.md` for the per-decision view
   and treat LRAB as the regression floor.

Acceptance for the first call site (intent): no regression on the existing tests, at least a
ten-point accuracy gain over the heuristic on the labelled set, reversal rate no worse, p95
latency under 300 ms on the discrete-GPU machine and under 1.5 s on the laptop, or the
decider stays off by default.

## 6. Rollout

1. `Decider` trait, `HeuristicDecider`, `LayeredDecider` with a `Null` local backend; move the
   intent call site onto it. Zero behaviour change, full test pass. One commit.
2. Spike: AnyJev-style Choice from the main model's logits through llama-server. If it works,
   it is the default local decider for llama-server users and needs no extra memory.
3. `LocalDecider` for a Jev-compatible endpoint; `doctor` probe; config section. Run JevK5
   GGUF locally, build the labelled set, measure (section 5).
4. Adopt for intent if it passes acceptance. Then stuck detection, then the dispatch
   classifier, each with its own measurement and its own `docs/changelog.md` entry.
5. Compaction scoring as a separate design once the first three have shipped and we have the
   latency numbers on real hardware.

## 7. Risks and open questions

- **Memory.** A 4B decider is another 2.5–3 GB resident. On the 16 GB tier running gemma4:e2b
  that may evict the main model. The logits spike (step 2) and the encoder route exist for
  this reason; the layered design keeps the heuristic as a zero-cost fallback.
- **Latency claims.** The 13 ms figure is an H100 number. On an integrated GPU a decision over
  2K tokens of state is a prefill of one to two seconds. Decisions must run over short
  states: the last user message plus a few turns, never a transcript.
- **A six-week-old field.** Positions on JevBench change weekly and most numbers are
  self-reported. Pick by our own measurement, not the leaderboard. Candidates with
  independent numbers and permissive licences today: JevK5 (Apache-2.0, GGUF) and Clef
  (Apache-2.0). Kev is CC BY-NC and is out for a shipped product.
- **Calibration drift.** Probabilities calibrated on the vendors' tasks are not calibrated on
  Boubou's. Thresholds are set from our labelled set, and `doctor` should show the decider's
  confidence histogram over recent sessions so drift is visible.
- **Does Ollama expose logprobs?** Unknown at the time of writing; it decides whether
  Ollama-only installs get a local decider without a second engine.
- **Training on our own transcripts.** Attractive, but transcripts contain user data. Any
  fine-tuning set is built locally, from the user's own sessions, by explicit command; nothing
  is collected.

## Addendum, 29 October 2026: what was built

Ollama 0.40 serves decision models natively (`ollama pull d1`, capability `decision`,
`POST /v1/systemone`), with the three primitives this note wanted: `noul` (probability of yes),
`choice` (option, per-option probabilities, confidence) and `score` (weighted level over an
ordered scale). One request carries a `state` block and up to 64 named questions; no tokens are
generated. The installed `d1:latest` (LFM, 2.7B, Q8_0) answers the intent question over a
200-token state in about 0.3 s warm on the laptop, 10 s on cold load, deterministically. Open
question 7 ("does Ollama expose logprobs?") is therefore moot.

Step 1 as shipped:

- `guards/qa.rs`: `intent_heuristic()` returns `{intent, confidence, rule, text, has_progress}`;
  confidence is 1.0 on a rule hit (task hint, question hint, trailing `?`) and 0.5 on each
  default (short message with or without progress, long message with no cue, no message).
  After progress the "short" default reaches `FOLLOWUP_MAX_CHARS` (80) rather than 40 (bead
  boubou-he2; +2 follow-ups right, no task lost on the labelled set). `FollowUp` now has loop
  behaviour of its own: the task loop runs, but once the model has acted and then reports in
  text the turn ends instead of drawing the task-mode continue notes.
  `is_qa()` is a wrapper over it, so every existing test holds unchanged.
- `decider.rs`: `DecisionBackend` trait (state + named questions → answers), `NullBackend`,
  `OllamaSystemOne`, and `LayeredDecider::intent_of(messages)` with three modes from
  `config.json` `decider.mode`: `off` (default; heuristic only, backend never built), `shadow`
  (ask every run, log the answer, act on the heuristic), `on` (act on the model only where the
  heuristic confidence is below `threshold`, default 0.9, that is only on the defaults). Any
  backend error leaves the heuristic standing; after three consecutive failures the model is
  not asked again that run.
- `agent/run.rs`: the one `is_qa` call site goes through the decider and logs one line,
  `[decider] intent=… ← heuristic|model (rule, confidence); d1: … p=… conf=… (ms) [DISAGREES]`.
  Children never ask. `boubou doctor` probes the configured model with the 2026-10-28 chat
  sentence and prints the latency.

Shadow mode is the labelled-set collector section 5 asks for. In `shadow` and `on` the loop
appends one JSON line per decision to `<sessions>/<session>.decisions.jsonl` (bead
boubou-acy.3): timestamp, the text and `has_progress` the heuristic saw, the heuristic's
intent/rule/confidence, the model's name/intent/probability/confidence and latency (or
`model_error`), and `acted_on`. The log line alone reached no file on any surface (the CLI's
stderr, the web log pane, ACP's tracing under a `warn` filter), so a morning of shadow mode
left nothing to label. `bench/decider/build_intent_set.py --shadow DIR` turns the sidecars
into `shadow_candidates.json` keyed by a short text hash; label them in `shadow_labels` and
they join the set as `shadow:<session>`. Swap `decider.model` to compare decision models on
the same sessions; the sidecar records which model answered. Adoption (`on`) waits on
that comparison; the acceptance numbers in section 5 stand.

### Measurement, 29 October 2026

`bench/decider_eval.py` over a 306-case labelled set (`bench/decider/intent_set.jsonl`; details
in `docs/changelog.md`, bead boubou-acy.2): heuristic 76.5% vs d1:latest 81.4% on question-vs-not,
shipped `on` policy 78.8%, reversal under rephrasing 35.9% vs 19.0%, p95 608 ms. Acceptance not
met: the shipped policy loses 2.6 points of 3-class accuracy because the layering premise is
inverted (the heuristic errs on rule hits, the model errs on the heuristic's defaults). On the
99 cases from real use the heuristic scores 87.9% and no policy beats it meaningfully; the
model's gains are on authored blind-spot cases. `off` stays the default; shadow collection
continues; re-evaluate on shadow-collected cases.
