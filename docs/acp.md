# Boubou in your editor (Agent Client Protocol)

`boubou acp` serves the [Agent Client Protocol](https://agentclientprotocol.com) over stdio.
Any ACP client (Zed, JetBrains IDEs, Neovim plugins) can spawn it and get Boubou's local
model as an agent inside the editor: streamed text and thoughts, every tool call with its
status and a diff for file edits, the plan, and the safety gate's questions as the editor's
own permission dialog.

## Run

```sh
cargo build --release
./target/release/boubou acp --model gemma4:e2b      # or any tag from `boubou models`
```

The model can also come from `BOUBOU_MODEL`. Display names from `config.json` `models` are
accepted. Ollama is started if it is down; an OpenAI-compatible or cloud engine in
`config.json` is used as for the CLI. stdout carries the protocol, so all diagnostics go to
stderr: `RUST_LOG=boubou_acp=info` shows the harness lines (`[decider]`, `[profile]`, gates).

## Zed

Agent Settings → External Agents → Add Agent → Add Custom Agent, or in `settings.json`:

```json
{
  "agent_servers": {
    "Boubou": {
      "type": "custom",
      "command": "/absolute/path/to/boubou",
      "args": ["acp", "--model", "gemma4:e2b"],
      "env": {}
    }
  }
}
```

Then start a new external agent thread from the Agent Panel and pick Boubou. The session's
working directory is the project root Zed passes as `cwd`; Boubou's session files are written
under `~/.ollama_agent/sessions/acp_*` as for a web chat. The live checkpoint goes there too
(`acp_*.state.json`, with `acp_*.state.meta.json` for the time budget) rather than to the
CLI's `<cwd>/.agent_state.json`: the project directory gets nothing of the harness, so the
model cannot find its own history in a listing and your git status stays clean. The one
file the loop still writes into the project is `todo.json` when the model uses the plan
tool.

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

## Turn semantics

An editor turn is a standard agent turn, not a batch task run. The turn ends when the model
replies in text without a tool call, or when it calls `finish`. `finish` is accepted as it
is: the delivery, test, artefact, plan and self-check gates exist for unattended batch tasks,
and in a project directory they would run your own test suite and bounce the turn into work
you did not ask for. The loop's "continue with tools" notes are not sent, every turn takes the
task prompt and the full tool set (a question can still read files, and an instruction
phrased as a question can still write them), and nothing is dispatched to child processes.
The safety gate, the duplicate-call and failure ladders, the idle-streak nudges and context
compaction are unchanged. Two empty replies in a row end the turn.

## Resuming after a restart

Boubou advertises `loadSession`. When the editor restarts the agent and reopens a thread, it
sends `session/load` with the thread's session id, which is the Boubou session name
(`acp_MMDD_HHMMSS`). Boubou reads `~/.ollama_agent/sessions/<id>.json`, replays the history
as `session/update` notifications (your turns, the agent's prose, each tool call with its
result, the plan if there was one; the loop's own nudges and compaction summaries are not
shown) and then continues the conversation from where it stopped. An unknown id is
`session not found`; a thread whose Boubou session files were deleted cannot be reopened.

## Not yet

Session modes, the client's `fs` and `terminal` capabilities (edits go straight to disk, not
through editor buffers), and MCP servers passed in `session/new` or `session/load` (Boubou
keeps its own `mcp.json`). The SDK ships weekly with frequent major versions; the dependency
is pinned exactly and bumped on purpose.
