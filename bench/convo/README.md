# Conversation scripts (bead boubou-6gq)

Scripted multi-turn sessions that `bench/acp_convo.py` drives through `boubou acp` the way an
editor does, for the features the single-prompt tiers cannot see: the scope gate, standing
instructions held across tool-heavy turns, follow-ups that must be answered rather than acted
on, compaction over a long conversation, the permission dialog. One cell is model x script x
run; the rig answers every permission request from the script (default deny) and records the
ask, reads the session's `.log` (bead boubou-bmq) for the tool calls per turn, and checks each
turn's expectations deterministically. The rig never starts Ollama; you do.

```sh
python3 bench/acp_convo.py --model qwen3.5:4b,gemma4:e4b --ctx 32000 --runs 3 \
    --results bench/results/convo/4b_$(date +%F)
python3 bench/acp_convo.py --summary --results bench/results/convo/4b_2026-10-31
```

A cell with a `score.json` is skipped; use a new `--results` dir to rerun. `--scripts a,b`
restricts to script ids; `--turn-timeout-min` (default 10) cancels a turn that runs on, which
the row then shows as a failed turn with `stop cancelled_by_rig`.

## Scripts

| id | fixture | what it probes |
|---|---|---|
| `tracker_drift` | `convo/fixtures/tracker_spec` | a spec to be split into `tasks.md`, never implemented: every turn is tracker/docs work; a source write is drift (the gate asks, the rig denies, `asks` counts it) |
| `follow_ups` | `lrab_ext/fixtures/ed01_slug` | a code task, two one-line follow-ups, a what-next question that must be answered in text, a touch-nothing-else edit |
| `constraints` | `lrab_ext/fixtures/ch01_notes` | "start every reply with OK" set in turn 1 and checked after listing, counting and writing; a no-shell turn; a one-sentence reply |
| `longform` | `lrab_ext/fixtures/ch01_notes`, ctx 12000 | ten appending turns on one file at a small window so compaction fires; the turn-1 codename recalled without reading; a section count |
| `permission` | `lrab_ext/fixtures/ch05_hello` | an out-of-workdir write denied, then allowed (`/tmp/boubou_convo_perm.txt`, removed before the cell) |

## Expectation keys

Per turn, under `expect`: `reply_regex` (string or list; case-insensitive, dot matches
newline), `reply_not_regex`, `reply_max_lines`, `reply_nonempty`, `files_exist`,
`files_absent` (paths relative to the workdir, or absolute), `file_regex` / `file_not_regex`, `keep_file_regex` (added by the rig itself: a turn whose `say` has "Change nothing else" also keeps every earlier turn's `file_regex` pair it does not re-check, keyed `<file>#t<n>` by the turn that set it, and only when that check passed at its own turn (bead boubou-f1x), bead boubou-dby; `--rescore` reports lost sections in finished cells from their final files)
(`{path: regex}`; `path#tag` lets one file take several), `no_source_writes` (no
`create_file`/`edit_file`/`append_file` on a source extension this turn, read from the
session log), `no_tool` (list of tool names), `max_calls`, `permission_asked` (bool), `asked_target_regex` (some ask's `<tool> <target>` matches: the write itself was asked about, not just any call),
`stop_reason`. `permission` on the turn is the rig's answer: `deny` (default), `allow`,
`allow_all`. A script's `ctx` overrides `--ctx`; `absent_before` lists files removed before
the cell starts.

## Reading the table

`turns` passed/total per cell, `asks` permission requests, `calls` tool calls, `gsw`
source-file writes in turns the gate classed as tracker or docs (the drift that got through,
or was allowed), `compact` compactions, `peak` prompt tokens, `wall` seconds; then the mean
turn pass rate per model x script with n. A failed turn's reasons are in the last column and
in the cell's `turns.jsonl` with the reply text. Three runs before any mean claim, as for the
tiers; one run reads a mechanism (which turn failed, how) and nothing more.

## Scope labels

`scope_labels.jsonl` is one line per rig instruction (and the two live ones) with the class the
scope gate should give it and why (bead boubou-gqw). A kernel test asserts every line; a
classifier change that moves one must change its label and say why.
