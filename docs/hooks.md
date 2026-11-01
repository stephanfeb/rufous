# Hooks

A hook is a command of yours that rufous runs at an event of the loop: when a session starts,
before and after a tool call, and when the model wants to end the turn. Hooks let a project
feed its own context in (a task tracker's state, house rules) and keep the model honest
(block a tool call, ask for more before the end) without a word in the prompt. They cost no
prompt tokens except what a `SessionStart` hook prints.

The file shape and the protocol are the ones Claude Code, Codex and Cursor share, so a
`hooks.json` written for one of them runs here unchanged. This repository's own
`.claude/settings.json`, which runs `bd prime --hook-json` at session start, is an example.

## Where hooks are read from

All of these are merged, in this order:

1. `~/.ollama_agent/hooks.json` -- your hooks, every project
2. `<workdir>/.rufous/hooks.json` -- the project's hooks for rufous
3. `<workdir>/.claude/settings.json` and `.claude/settings.local.json` -- the project's Claude Code hooks (the `hooks` key), read as they are

`AGENT_HOOKS=0` turns every hook off. The harness log says what was loaded
(`[hooks] 2 hooks: SessionStart×1, Stop×1 (from …)`), and names event types this version does
not run.

## The file

```json
{
  "hooks": {
    "SessionStart": [
      { "matcher": "", "hooks": [{ "type": "command", "command": "bd prime --hook-json" }] }
    ],
    "PreToolUse": [
      { "matcher": "delete_file|run_bash",
        "hooks": [{ "type": "command", "command": "./scripts/check_call.sh", "timeout": 10 }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "./scripts/checklist.sh" }] }
    ]
  }
}
```

`matcher` is a regular expression matched in full against the tool name (`PreToolUse`,
`PostToolUse`) or the source (`SessionStart`, always `startup`); empty, `*` or absent matches
everything. `timeout` is in seconds, 60 by default; a hook that runs over is killed and the loop
goes on. An entry with a `command` of its own, without the inner `hooks` list, is taken too.

## The protocol

The command runs through the platform shell in the working directory, with
`RUFOUS_PROJECT_DIR` set to it, and one JSON object on stdin:

| field | when |
|---|---|
| `hook_event_name`, `cwd`, `session_id` | always (`session_id` is the rufous session name when there is one) |
| `source` | `SessionStart`: `startup` |
| `tool_name`, `tool_input` | `PreToolUse`, `PostToolUse`: the tool and its arguments as the model sent them |
| `tool_response` | `PostToolUse`: the tool's result (first 20,000 characters) |
| `stop_hook_active` | `Stop`: `true` when a stop hook already held this turn open |

What the command says back:

- **exit 0**, stdout a JSON object: `{"decision": "block", "reason": "…"}` blocks;
  `{"hookSpecificOutput": {"permissionDecision": "deny", "permissionDecisionReason": "…"}}`
  blocks; `{"hookSpecificOutput": {"additionalContext": "…"}}` adds context. Plain stdout is
  context at `SessionStart` and ignored otherwise.
- **exit 2**: block, with stderr as the reason the model is told.
- any other exit, or a timeout: logged (`[hooks] PreToolUse hook `…` exited 1: …`), the loop
  goes on as if the hook had said nothing.

Hooks only narrow. A `permissionDecision` of `allow` is an ordinary proceed: the safety rings
have already judged the call and keep the last word; a hook is one more ring outside them.

## What each event does

| event | where it fires | a block means | context goes |
|---|---|---|---|
| `SessionStart` | once, when a fresh session's task prompt is built | nothing (logged) | into the system prompt under `SESSION CONTEXT (from the project's hooks; follow it)`, capped at 6,000 characters; the size is in the log |
| `PreToolUse` | in the executor, after the safety gate passed the call | the call does not run; the model sees `[tool error: blocked by a project hook: <reason>]`, which the failure ladder counts like any refusal | nothing |
| `PostToolUse` | in the executor, after the call ran | the reason is logged; the result stands | appended to the tool result as `[hook: …]` |
| `Stop` | when a text reply would end the turn, or when `finish` is accepted | the turn goes on: the reason is sent to the model as a user message (`[stop hook] <reason>`), and a `finish` is deferred; at most once per turn, a second block is logged and ignored | nothing |

Because `PreToolUse` and `PostToolUse` live in the executor, every way a tool is called sees
them: the loop, `batch_tools`, the corrector's retried call. The Q&A prompt (a question
answered in text) does not carry the session context; the task prompt does.

## Cost and care

A `SessionStart` hook's output is prompt text on every turn of the session. `bd prime` prints
about 3,500 characters, which is roughly a thousand tokens on top of a prefill of about 1,250:
for a 2B to 4B model that is not free, and the harness log line `[hooks] SessionStart: N chars
of context into the task prompt` is there so a run's cost is on record. Measure before and
after on the small profiles before keeping such a hook in a project, as every prompt addition
here is measured.

A project's hooks run as you, with your permissions, as they do in every harness that reads
them. Read a project's hook files before running rufous in it.

## Not yet

`UserPromptSubmit`, `PreCompact`, `Notification`, `SubagentStop`; `continue: false`;
asynchronous hooks; `type: prompt` hooks (a model judging the event); `~/.claude/settings.json`;
Cursor's camel-case `hooks.json`. `session_id` is empty for a run with no session name, and
`transcript_path` is not sent.
