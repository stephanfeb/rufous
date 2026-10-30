# Refactor: kernel and policies

This document states where Boubou's agent loop is going and tracks how far it has got. It is
a plan and a status board, not a log: when a step lands, its row changes, and nothing is
appended. Dates, measurements and the story of how a step went belong in `docs/changelog.md`;
what the code does today belongs in `README.md`, `docs/acp.md` and the crate docs.

## Intent

Boubou started as a port of Mingbird, a batch harness for 2–12B local models. What it has
become is a layer that compensates for small-model quirks, and that layer is the basis for two
products over small models:

- a **general-purpose harness**: a conversation with an assistant that has tools;
- an **agentic coding harness**: an editor's agent working in the user's project.

Neither of them is Mingbird's unattended task run, and the loop must stop assuming it is.

The loop (`crates/boubou-kernel/src/agent/run.rs`) is two things entangled:

| | What it is | Examples |
|---|---|---|
| **Kernel** | What every product needs from a small model: the same for a chat, an editor turn and a batch task | format-leak salvage, argument coercion, the duplicate-call, streak and idle ladders, the empty-reply ladder, the integrity and honest-failure checks, the corrector, compaction, the five safety rings, per-model profiles |
| **Policy** | What the harness does *around* the model for this kind of turn | whether a text reply ends the turn, whether `finish` must pass the delivery gates, whether a question is routed away from the producing tools, whether a plan is dispatched to children, which system prompt frames the turn, the continue notes |

The goal is a kernel that does not know which product it is running in, driven by a policy
that is set in one place and read by name.

## Repositories

The end state is three repositories: the kernel, the general-purpose agent and the coding
agent. Until the kernel's API stops moving they are crates in one workspace with the same
boundaries, so every step below lands as one commit with the whole suite green, and the split
is a `git subtree split` per crate directory when the time comes.

| Crate | Holds | Rule |
|---|---|---|
| `boubou-kernel` | the model boundary, the ladders, salvage, coercion, compaction, the safety rings, profiles, the executor, config, sessions, tools, MCP, web search, memory, `TurnPolicy` | depends on no product crate; its public API is what the kernel repository will publish |
| `boubou-general` | the general-purpose product: the web UI today; the chat CLI, the `general` preset's prompts and skills to follow | depends on the kernel (and the voice adapter) only |
| `boubou-coding` | the coding product: the ACP server; the `coding` preset's prompts and skills to follow | depends on the kernel only |
| `boubou-voice` | the speech-to-text adapter | product-neutral; goes with the kernel or the general product at the split |
| `boubou-cli` | the `boubou` binary wiring the crates together, `run` for the batch harness and the bench | the only crate that sees everything |
| `boubou-app` | the Tauri shell around `boubou-general` | goes with the general product |

Still to place: the `unattended` preset and the parallel dispatch machinery are Mingbird's
batch harness, used by `boubou run` and the bench. They stay in the kernel until a product
wants them or neither does, and then move to the bench or a crate of their own. The skills
split by product (`bug_fix`, `code_review`, `refactor` to coding; `file_organizer`,
`web_research`, `doc_digest` to general); the bench stays beside the kernel with a product arm
each.

The repositories split when two of these three hold:

- the kernel's API changes less often than the products do, over a few weeks;
- a product wants to ship against a pinned kernel version rather than the tip;
- someone works on one product and should not see the other, or the licences differ.

## The seam: `TurnPolicy`

`crates/boubou-kernel/src/agent/policy.rs`. `RunParams::policy` replaces the old
`RunParams::interactive` boolean. A child run (`AGENT_CHILD_SANDBOX=1`) always runs
`unattended`. At the start of a turn the loop calls `policy.turn(intent, child, ablated)`
once and gets a `Turn`: the `Route` (task, question, follow-up), the `Prompt` that frames
the turn (task, chat, child), the `Gates` this turn runs (the policy's, none on the
question route or under the ablations, minus the plan bookkeeping for a child) and the
switches derived from the route (`working`, `dispatch`). From there the loop reads those
names and never recombines the inputs; the system prompt is built in one place from
`Turn::prompt`, and each gate block asks `turn.gates.has(Gate::…)`.

| Field | On | Off |
|---|---|---|
| `text_ends_turn` | a text reply with no tool call ends the turn; two empty replies end it | continue note; context reset after repeated empty replies |
| `gates` | the set of finish gates `finish` must pass, one `Gate` each: delivery, tests, claims, claimed files, named files, plan files, plan sync, re-read, wrap-up | empty: `finish` accepted as is |
| `intent_routing` | a question takes the Q&A prompt and loses the producing tools; a follow-up ends once acted on and reported | every turn takes the task prompt and the full tool set |
| `parallel_dispatch` | a plan with enough simple items is farmed out to sandboxed children when `parallel` is configured | nothing is dispatched |

Presets and who sets them:

| Preset | `text_ends_turn` | `gates` | `intent_routing` | `parallel_dispatch` | Set by |
|---|---|---|---|---|---|
| `unattended` | off | all nine | on | on | `boubou run`, children, the web UI with `batch: true`; the default |
| `general` | on | none | off | off | the web UI's chat (`boubou serve`, `boubou-app`) and `boubou chat` |
| `coding` | on | none | off | off | the ACP server (`boubou acp`, Zed) |

`general` and `coding` are equal field for field today: they are the two behaviours the loop
already had, given names. A unit test in `policy.rs` asserts the equality so that the first
divergence is a deliberate, documented change (step 9 below) and not drift.

## What is still entangled

Policy decisions the loop still takes from somewhere other than `TurnPolicy`:

- **`RunParams::child_mode`**: three things in one flag. The ring-0 sandbox and the
  narrowed tool face are safety, and stay where they are; "never Q&A, never dispatch, batch
  gates" is a policy and is already expressed as `unattended`; the child system prompt is
  prompt selection (next item).
- **Ablations** (`AGENT_ABLATION=…`): the bench's way of switching pieces off for a
  measurement. The two finish-gate ablations are one input to `TurnPolicy::turn`; the
  others (`flat_prefill`, `corrector`) are still read where they act.
- **`RunParams` itself** carries kernel knobs (`num_ctx`, `think`, `prune_keep`, the profile)
  and policy side by side. Fine while the kernel is the only crate that builds one; the
  product crates will construct their own.
- **The run log and the decision sidecar** record the policy by name but the session files
  do not: a session resumed on another surface silently changes policy.

## Progress

| # | Step | Done when | Status |
|---|---|---|---|
| 1 | `TurnPolicy` struct with `general`, `coding` and `unattended`; `RunParams::interactive` removed; every surface sets a preset | workspace tests pass with no behaviour change; the equality test is in | done |
| 2 | Crates mirror the repositories: `boubou-core` → `boubou-kernel`, `boubou-web` → `boubou-general`, `boubou-acp` → `boubou-coding` | the workspace builds and tests under the new names; the kernel depends on no product crate | done |
| 3 | `boubou chat` runs `general`; `chat_mode` removed | `RunParams::chat_mode` gone; `chat` and the web UI behave the same on a text reply | done |
| 4 | The intent route is a value the policy derives from the decider's intent (`Route`, and `Turn` for the derived switches) | the route read in fewer than ten places in `run.rs`; no `!child && policy.x && intent == …` expressions | done |
| 5 | System prompt chosen from the policy and the route (`Turn::prompt`, `Prompt::rebuild`) | `run.rs` no longer branches on `child` or `qa` to pick a prompt | done |
| 6 | The finish gates are named units the policy lists (`Gate`, `Gates`) | `coding` can keep the named-deliverable gate and drop the test guard by changing the preset only | done |
| 7 | `child_mode` split: sandbox and tool face stay as safety; the rest is `unattended` plus the child prompt | `run.rs` reads `child` only where the sandbox or the dispatch receipt needs it | open |
| 8 | Kernel boundary inside `boubou-kernel`: the ladders, salvage, compaction and corrector are a module the policy-driven loop calls | `run.rs` is the policy loop; the kernel module has no policy field reads; line counts recorded in the changelog | open |
| 9 | Sessions record the policy they ran under | a resumed session keeps its policy or says it changed | open |
| 10 | `general` and `coding` diverge where measurement says so | the equality test is replaced by one that states the difference and why | open |
| 11 | The chat CLI, the prompts and the skills move into the product crates | `boubou-cli` holds wiring and `run` only; each product crate owns its preset's prompts and skills | open |
| 12 | Repositories split: kernel, general-purpose agent, coding agent | each product repository depends on a published kernel version; two of the three signals above hold | open |

Steps 3 to 7 can land in any order and each one is a single commit with the workspace tests
green. Step 8 waits for 4 to 7; 11 waits for 8; 12 waits for 11 and the signals. Step 10 is
driven by the bench, not by this document: a divergence without a measurement behind it is
drift.
