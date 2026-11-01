# Boubou in your editor (Agent Client Protocol)

`boubou acp` serves the [Agent Client Protocol](https://agentclientprotocol.com) over stdio.
Any ACP client (Zed, JetBrains IDEs, Neovim plugins) can spawn it and get Boubou's local
model as an agent inside the editor: streamed text and thoughts, every tool call with its
status and a diff for file edits, the plan, and the safety gate's questions as the editor's
own permission dialog.

## Run

```sh
cargo build --release
./target/release/boubou acp --model gemma4:e2b --ctx 32768   # any tag from `boubou models`
```

`--ctx` is the context window, in tokens, for every editor session. The CLI reads `AGENT_CTX`,
but an editor starts the agent with no environment, so without the flag the window is the
131072-token default and the model loads with a KV cache sized for it (qwen3.5:9b: 17 GB
resident under Zed). The editor turns measured so far peak at 6 to 13k tokens, so 32768 is
plenty; the window is printed in the first log line of every turn. The model can also come
from `BOUBOU_MODEL`. Display names from `config.json` `models` are
accepted. Ollama is started if it is down; an OpenAI-compatible or cloud engine in
`config.json` is used as for the CLI. stdout carries the protocol, so all diagnostics go to
stderr: `RUST_LOG=boubou_coding=info` shows the harness lines (`[decider]`, `[profile]`, gates).

## Zed

Agent Settings → External Agents → Add Agent → Add Custom Agent, or in `settings.json`:

```json
{
  "agent_servers": {
    "Boubou": {
      "type": "custom",
      "command": "/absolute/path/to/boubou",
      "args": ["acp", "--model", "gemma4:e2b", "--ctx", "32768"],
      "env": {}
    }
  }
}
```

Then start a new external agent thread from the Agent Panel and pick Boubou. The session's
working directory is the project root Zed passes as `cwd`; Boubou's session files are written
under `~/.ollama_agent/sessions/acp_*` as for a web chat. The live checkpoint goes there too
(`acp_*.state.json`, with `acp_*.state.meta.json` for the time budget) rather than to the
CLI's `<cwd>/.agent_state.json`, and so do the loop's working files: `acp_*.files/` holds
`todo.json`, `backups/` (the previous version of every file an edit or a redirecting command
touched, where the CLI leaves `<file>.bak` beside it), `trash/` (deleted files, the CLI's
`.mingbird_trash/`) and the image queue. The project directory gets nothing of the harness,
so the model cannot find its own history or safety net in a listing and your git status
stays clean; tool results say a file was backed up but not where, and `list_dir` never shows
harness files in any mode.

## What the editor sees

| Boubou | ACP `session/update` |
|---|---|
| streamed answer / thinking | `agent_message_chunk` / `agent_thought_chunk` |
| a tool call | `tool_call` (kind read / search / edit / delete / execute / fetch / think, in_progress, raw input, file locations) |
| its result | `tool_call_update` (completed, or failed when blocked or a tool error; output as content; a diff for create / edit / append) |
| `todo.json` | `plan` after every `todo` call and at the finish |
| context usage | `usage_update` |
| compaction, errors, turn limit | `notice` |
| `finish` summary | the closing message, when the model wrote no prose after its last tool call |
| a safety-gate question | `session/request_permission` with Allow / Allow for the rest of this turn / Deny. A shell redirect, `tee` or `cp`/`mv` destination outside the project asks the same question as a file tool (bead boubou-hzl). After Deny the model reads that the user declined, must not do it another way or elsewhere, and may make the same call again if you ask again (beads boubou-ik4, boubou-77n); a write of the declined file name at another path in the same turn is refused without a dialog (bead boubou-sxx); in a later turn it is a dialog titled `write the declined file name elsewhere`, since "put it in the project instead" is something you may have said; the name is forgotten once you allow the write at the declined path, or after five turns (bead boubou-dml); the server carries the list across your turns, since it builds a fresh gate for each prompt (bead boubou-vj3). A refused call is reported to the editor as failed (bead boubou-bj4) |
| the scope gate (bead boubou-6x3) | the same dialog, titled `outside the instruction: <path or command>`, when a tracker or docs instruction (beads, issues, specs, docs) is followed by a call that writes a source file or runs a build/test tool; Allow clears the turn, Deny hands the model the instruction back and asks for a report. A code instruction never asks. `"scope_gate": "shadow"` in `config.json` only logs |
| `session/cancel` | the loop's stop flag: the turn ends at the next check and reports `cancelled` |
| `read_file`, `create_file`, `edit_file`, `append_file` | `fs/read_text_file` and `fs/write_text_file` when the client offered them at `initialize` (see below) |
| `mcpServers` in `session/new` / `session/load` | added to Boubou's `mcp.json` and the project's `.mcp.json` for that session (stdio, HTTP; SSE tried as HTTP), routed by category like any other server |
| the loop's protocol lines (`⚙`, `[ctx: …]`, `🐚`, `⚠️`, `[prompt]`, `[hooks]`) | not sent to the editor; written to `<sessions>/<name>.log` behind a timestamp, with a `[turn] <prompt head>` line per prompt and tracing warnings mirrored in, so `bench/session_metrics.py <log>` reads a live session with the bench's counters (`AGENT_SESSION_LOG=0` switches it off) |

## Turn semantics

An editor turn runs under the `coding` turn policy (`docs/refactor.md`), not the batch task
run. The turn ends when the model replies in text without a tool call, or when it calls
`finish`. `finish` is accepted as it is: the delivery, test, artefact, plan and self-check
gates exist for unattended batch tasks, and in a project directory they would run your own
test suite and bounce the turn into work you did not ask for. The loop's "continue with tools" notes are not sent, every turn takes the
task prompt and the full tool set (a question can still read files, and an instruction
phrased as a question can still write them), and nothing is dispatched to child processes.
The safety gate, the duplicate-call and failure ladders, the idle-streak nudges and context
compaction are unchanged. Two empty replies in a row end the turn. One check does run at either ending (bead boubou-78s): when your instruction names a file to write or change ("add … to tasks.md", "tick the first task in tasks.md") and no write reached it this turn, the reply or finish is held once with a note naming the file and saying the todo plan is not it; a reply that says why the file could not be written ends the turn. A bare name in your instruction is the file of that name in the project when one exists: a write of the same name in another directory (`openspec/tasks.md` for "tasks.md" with `./tasks.md` present) is held with both paths in the note.

## Files through the editor

A client that advertises `fs.readTextFile` and `fs.writeTextFile` in its `initialize`
capabilities (Zed does) owns the project's text files for the session: `read_file` asks the
editor for the file's text, which is the open buffer when there is one and the disk otherwise,
so unsaved changes are what the model reads; `create_file`, `edit_file` and `append_file`
hand the editor the new text, which lands in the buffer and is saved as the editor decides,
rather than being written to disk beneath it. Each method is used only when offered, so a
client with reads but no writes gets the mix. The edit's backup copy is still made, of the
text the model saw, and the overwrite guard still measures the file on disk. Everything the
protocol has no method for stays on disk: `delete_file`, `list_dir`, `search_files`, images,
commands and their backups, and whether a file exists. The log line `fs/read_text_file
<path>` (at `boubou_coding=info`) shows each call that went through the editor. When the
editor cannot serve a read (Zed answers a path that is a directory with an internal error),
the disk is read instead and both errors are reported only if that fails too; `read_file` on
a directory says so before anyone is asked. A write the editor refuses falls back to the
disk the same way. The editor is asked only about paths inside the session's working
directory: a file elsewhere (a skill under the home directory, say) goes to the disk, since
the editor has no worktree for it (Zed answered such writes with "invalid path", or opened a
buffer it never saved and left a 0-byte file). Fetched page texts go to the session's
`.files/sources/` directory, not the project, and the `web_fetch` pointer is that absolute
path. Without the capability every tool uses the disk, as for the CLI.

## Resuming after a restart

Boubou advertises `loadSession`. When the editor restarts the agent and reopens a thread, it
sends `session/load` with the thread's session id, which is the Boubou session name
(`acp_MMDD_HHMMSS`). Boubou reads `~/.ollama_agent/sessions/<id>.json`, replays the history
as `session/update` notifications (your turns, the agent's prose, each tool call with its
result, the plan if there was one; the loop's own nudges and compaction summaries are not
shown) and then continues the conversation from where it stopped. An unknown id is
`session not found`; a thread whose Boubou session files were deleted cannot be reopened.

## Not yet

Session modes, and the client's `terminal` capability (`run_bash` runs in Boubou's own
process, the editor shows its output as a tool result). The SDK ships weekly with frequent
major versions; the dependency is pinned exactly and bumped on purpose.

Not Boubou's to add: editing an earlier message and resuming from it (Zed's rewind). Zed
gates it on a client-generated message id and a `truncate` hook that its external-agent
connection does not implement for any agent, and the protocol has no method for it (checked
2026-10-31 against Zed `main`, `crates/agent_servers/src/acp/{v1,v2}.rs`, and schema 1.13);
the "not supported" notice appears for every ACP agent. When ACP specifies it, Boubou's side
is a truncation of the session's message list and files to the named user message.
