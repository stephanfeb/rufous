# Effective Agentic Coding with Smaller Models

A working guide, started 2026-10-31. It collects what has held up when a 2B–12B model runs
under Ollama as a coding agent: what to prepare before the first prompt, what the harness
has to compensate for, and how to tell a real gain from noise. Boubou is the harness the
measurements come from, but most of the advice is about the model and the project, not the
tool.

Every claim carries one of three tags. **Measured** means a bench cell with n ≥ 3 and the
changelog entry that holds it. **Observed** means a live session or a single run: an
illustration, not evidence. **Hypothesis** means we think so and have not run the cell. A
claim moves tags only when its cell is run; see the measurement section before trusting a
mean.

## 1. Principles

The thread through everything below: a small model is good at doing one named thing with the
facts in front of it, and bad at deciding what the thing is or guessing the facts. Preparation
moves deciding and guessing out of inference and into the context.

- **Decomposition is moved out of inference.** The model's weakest skill is choosing the next
  step from an open brief. A task pre-split into scoped items turns each turn into "do the
  named thing". *Observed* (qwen3.5:9b, 2026-10-31): with skills, a spec framework and beads
  in place, one prompt produced a usable initial spec. Cell: boubou-rld, `split` level.
- **Project knowledge is moved into context.** Grounding failures were the largest loss cause
  in the gemma4:e2b runs: the model invented a `workdir/` subdirectory in 6 cells across 5
  runs and wrote to it (*measured*, fixed in the harness by a profile rule, changelog
  2026-10-31). A file that states the layout and conventions is cheaper than a rule per
  mistake. Cell: boubou-rld, `agents` level.
- **Open-ended writing becomes template filling.** Small models complete a structure better
  than they compose from nothing. A spec framework gives the structure. *Hypothesis*; cell:
  boubou-rld, `spec` level.
- **State survives the model's memory.** Compaction and session breaks lose whatever lived
  only in the conversation. Specs, task files and beads do not. *Observed* across every long
  session; the kernel's compaction (`docs/refactor.md`) is the harness half of the same point.
- **Procedures the model never learned arrive as skills.** A skill with `triggers:` is
  prefilled only for matching tasks, so a 4B model gets a workflow at the cost of a few
  hundred tokens on the turns that need it (`docs/skills.md`). *Hypothesis* for the score
  effect; cell: boubou-rld, `skills` level.
- **Every word in a tool result is an instruction.** A small model acts on the harness's
  notes as if the user wrote them. *Observed* (Zed, 2026-10-31): a `[auto-backed up as
  x.bak]` note produced eight housekeeping calls to delete or inspect the backups; the note
  now says there is nothing to clean up. Offered `or pass all=true` as a way out of a refusal,
  gemma4:e2b passed it on 150 of 175 edit calls (*measured*, changelog 2026-10-31). Applies to
  anything you write into AGENTS.md or a skill as well.
- **Refusals the model cannot act on are wasted tokens.** After an edit was refused for
  matching several places, 2B models re-sent the identical edit until the tool was blocked,
  under three wordings (*measured*, ED-06 cells, 2026-10-31). Prefer a tool that makes the
  mistake impossible to a message that explains it.

## 2. The tension: context is the budget

Boubou's system prompt and nine core tool schemas are about 1,250 tokens so that a 1B–4B
model can run at all. Every piece of preparation is paid for in the same window: an
AGENTS.md, a spec, a prefilled skill, a long task list. The gain is not monotone. A 32k
window with a 2,000-token spec, a 600-token skill and a 1,000-token file read is already a
third spent before the model has done anything, and a model that was told the layout but can
no longer see the file it is editing is worse off than one that was told nothing.

So the question the experiment has to answer is not "does prep help" but "where does the
curve bend for this window size", and the guide will carry one reading per model class.
*Hypothesis* until boubou-rld runs; the `prompt_eval_count` column is the cost side.

Set the window on purpose. Under an editor Boubou takes `--ctx`; left at the model's default a
qwen3.5:9b loaded at 17 GB (bead boubou-0lg). A window the machine cannot hold is a slower
model, not a bigger one.

## 3. Prep checklist

In the order that is expected to matter most. Each item names the cell that will confirm or
demote it.

1. **Write an AGENTS.md** (or `SYSTEM.md`) in the working directory: layout, build and test
   commands, naming conventions, what not to touch. Short, declarative, no advice the model
   cannot act on. Boubou prefills it for every turn. Cell: `agents`.
2. **Install the skills for the task family** as `<name>/SKILL.md` folders under
   `.agents/skills` or `.claude/skills` with `triggers:` so they load only when relevant. Cell:
   `skills`.
3. **Write the spec before the first coding prompt**, in a fixed shape (openspec, or EGM's,
   see §6). Let the model fill the shape if it can; review it yourself before implementation.
   Cell: `spec`.
4. **Split the work into items small enough for one turn each**, with beads or a tasks file.
   Each item names its file(s) and its finish condition. Cell: `split`.
5. **Set the context window explicitly** and check the model's memory footprint once.
6. **Point the model at bounded tools.** Boubou's prompt tells the model to use `list_dir`,
   `read_file` and `search_files` over `ls`, `cat` and `grep` because their output is cut
   with a marker; an unbounded `cat` of a large file is the fastest way to lose the window.

## 4. What the harness compensates for (so you need not)

These are the kernel's jobs, measured so far on gemma4:e2b and qwen3.5 models; the reader
running another harness should check theirs does the equivalent.

- **Edit matching tiers.** Exact, then unescaped, then NFKC and whitespace-tolerant matching
  of the old text, with a nearest-region message on a miss. Edit misses fell 41 → 16 on the
  editor tier (*measured*, changelog 2026-10-31). Fuzzy tiers never matched; the unescape tier
  did the work.
- **No tool withheld by parameter count.** The inherited rule that kept `edit_file` from ≤4B
  models cost the rename task every cell; removing it moved the mean 0.748 → 0.825 and the
  rename cell 0/6 → 4/6 (*measured*).
- **Anti-loop ladders and finish gates.** With the kernel off, gemma4:e2b reported "fixed" on
  work it had not done in 6 of 10 cells, against 0 with it on (*measured*, kernel ablation).
- **Path rebasing per model profile.** The invented-subdirectory quirk above is caught at
  dispatch for the profile that has it. Model quirks live in the profile, not the prompt.
- **Commands run as written.** The harness must not rewrite the model's command (an early
  version unescaped `\n` and `\"`, and four consecutive correct commands failed; *observed*,
  Zed 2026-10-31). The replay corpus `tests/replay/zed_2026-10-31.jsonl` holds the commands.
- **Harness files out of the project.** Backups, trash, fetched pages and the todo file live
  beside the session, not in the user's tree, so the model never finds and "cleans" them.
- **A scope for the turn.** The harness classifies the instruction (tracker, docs, code,
  mixed) and asks the user before a tracker or docs turn first writes a source file or runs
  a build. *Observed* trigger (the beads turn of 2026-10-31 drifted into writing Dart after
  `bd ready`); the arrest itself is unmeasured until a live session trips it.

## 5. Per-model notes

Drawn from the profiles and the bench; one paragraph per model class once the cells exist.

- **gemma4:e2b.** Invents a `workdir/` subdirectory (profile rule); writes whole files when an
  edit would do; takes any escape hatch a refusal offers. Baseline 0.748 (n=3, 2026-10-31),
  0.825 after the ≤4B rule went.
- **qwen3.5:2b.** Quotes whole functions as the old text, so it rarely meets an ambiguity
  refusal and cannot act on one when it does (ED-06 1.00 × 6).
- **qwen3.5:4b and 9b.** The Zed sessions ran on 4b; the spec observation on 9b. No tier
  cells yet.

## 6. Open questions

- **The prep experiment** (bead boubou-rld): the levels, their cost, and whether a prepped 4B
  matches an unprepped 9B.
- **Can the small model do its own prep?** The observed spec session says sometimes. A later
  cell gives the model only the frameworks and asks for the spec and the split.
- **Elephant-Goldfish Model** (egm.dev, bead boubou-1uu). An open-source plugin for Claude
  Code, Codex and Antigravity, published on GitHub by Google and introduced by Dave Rensin:
  a full-context Elephant agent drafts the spec, zero-context Goldfish subagents that see only
  the draft and the source review it from conflicting perspectives before any code, a drift
  report later checks code against spec, and the human approves the spec. It is prep-heavy
  and spec-first, so it belongs here; the Goldfish role is also a small-model-shaped job
  (short context, one document, one lens), which raises the question whether a 4B–9B model
  can review a spec a larger model or a human wrote. Comparison with openspec and beads to
  follow once the repository has been read.
- **Where the context curve bends** per window size (§2).

## 7. Measuring your own setup

- **n = 1 is an anecdote.** Two arms with the same configuration moved a mean 0.70 → 0.47 on
  one run (finish-gate arm, 2026-10-31). Three runs per cell before any mean claim; one run is
  enough only to read a mechanism in a single cell (the rule fired, the edit missed).
- **Read the process, not just the score.** Boubou's bench table carries tool calls, edit
  misses, shell errors, backup housekeeping and harness notes per cell
  (`bench/lrab_ext/README.md`). A score that rose while misses doubled is luck.
- **Record before and after sizes** for every prompt addition; the window is the budget.
- **Replay what went wrong.** A corpus of real commands and tool inputs from a bad session,
  run against the harness as a test, catches harness-caused failures that unit inputs shaped
  like the mechanism never did.
- **Change one thing per run**, and never the binary while a run is in progress.
