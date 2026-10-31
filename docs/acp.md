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
| a safety-gate question | `session/request_permission` with Allow / Allow for the rest of this turn / Deny |
| `session/cancel` | the loop's stop flag: the turn ends at the next check and reports `cancelled` |
| `read_file`, `create_file`, `edit_file`, `append_file` | `fs/read_text_file` and `fs/write_text_file` when the client offered them at `initialize` (see below) |
| `mcpServers` in `session/new` / `session/load` | added to Boubou's `mcp.json` and the project's `.mcp.json` for that session (stdio, HTTP; SSE tried as HTTP), routed by category like any other server |

## Turn semantics

An editor turn runs under the `coding` turn policy (`docs/refactor.md`), not the batch task
run. The turn ends when the model replies in text without a tool call, or when it calls
`finish`. `finish` is accepted as it is: the delivery, test, artefact, plan and self-check
gates exist for unattended batch tasks, and in a project directory they would run your own
test suite and bounce the turn into work you did not ask for. The loop's "continue with tools" notes are not sent, every turn takes the
task prompt and the full tool set (a question can still read files, and an instruction
phrased as a question can still write them), and nothing is dispatched to child processes.
The safety gate, the duplicate-call and failure ladders, the idle-streak nudges and context
compaction are unchanged. Two empty replies in a row end the turn.

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
<path>` (at `boubou_coding=info`) shows each call that went through the editor. Without the
capability every tool uses the disk, as for the CLI.

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
