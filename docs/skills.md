# Skills directory

A rufous "skill" is an operating manual for the model (a .md file). Normally the agent only knows each skill's name and one-line description (saving tokens); when it needs one it loads the full text with `skills(load)`. This is "progressive loading".

## Two shapes
A skill is either a flat file, `<dir>/<name>.md`, or a folder, `<dir>/<name>/SKILL.md`, the
[Agent Skills](https://agentskills.io) convention that Claude Code, Codex, Cursor and others read.
Both carry the same `name` / `description` frontmatter and are listed and loaded the same way. A
folder may hold `scripts/`, `references/` and `assets/` beside `SKILL.md`; when the model loads the
skill, the body ends with those files as absolute paths so it can `read_file` or `run_bash` them
(at most 20 are named). A folder without `name` in its frontmatter is named after the folder. The
convention's folded multi-line `description` and nested `metadata` block are accepted;
`allowed-tools` is read but not acted on, since rufous's tools are gated by the safety rings. A
skill written for another harness therefore works as it is: drop the folder in, no copying.

## Directories and precedence
Lookup order (on a name clash, the earlier one wins):
1. `<workdir>/skills/` -- skills specific to the current task
2. `<workdir>/.agents/skills/` and `<workdir>/.claude/skills/` -- the project's skills in the shared convention, the directories other harnesses read too
3. `~/.ollama_agent/skills/` -- global skills you installed yourself (recommended location)
4. `~/.agents/skills/` and `~/.claude/skills/` -- your global skills in the shared convention
5. `<install dir>/skills/` -- a `skills/` directory beside the binary, if you keep one there
6. the skills built into the product -- compiled into the binary from `crates/rufous-general/skills/` (documents, folders, research) and `crates/rufous-coding/skills/` (code); the `rufous` binary carries both sets, an editor session sees the coding ones, the web UI and `rufous chat` the general-purpose ones

Discovery costs no prompt text: the listing is the `skills` tool's result, and a skill is prefilled
only when it carries rufous's `triggers:` or `autoload:` keys, which skills from other harnesses do
not.

## Installing your own skill
Put a .md file in `~/.ollama_agent/skills/` with two lines of metadata at the top:

```
---
name: my_skill
description: One sentence saying when to use it
---
Body: the operating steps (instructions for the model)...
```

After restarting rufous it appears under the "Skills" button in the UI. Follow the built-in skills in this directory as a model: step by step, tool names written in full (list_dir / run_bash / web_search ...), and the red lines (what must not be done) at the end.

## Putting a skill in effect automatically
Two optional metadata lines make a skill apply without the model asking for it:

```
---
name: finance_analysis
description: Reconcile and analyse ledgers and CSV exports
triggers: ledger, invoice, reconcile, cash flow
autoload: false
---
```

`triggers` is a comma-separated list; when any entry occurs in the task text (case-insensitive) the
skill's full body is placed in the system prompt before the run starts, under "SKILL IN EFFECT FOR
THIS TASK". `autoload: true` does that for every task that can see the skill. At most three skills
are prefilled per task, in name order, and the log shows `[skills in effect: …]`. Put domain skills
in `<workdir>/skills/` so they apply to that folder only. A resumed session keeps the prompt it
started with.

## Per-folder instructions: SYSTEM.md and AGENTS.md
A `SYSTEM.md` in the working directory is appended to the system prompt for task and chat turns
alike, under "PROJECT INSTRUCTIONS". If its first line is `<!-- rufous: replace -->` it replaces the
built-in prompt instead (the Settings → System prompt override, when enabled, still wins the base and
the file is appended under it). Use append for a persona, house rules and output formats; use replace
only when you also take over the intent judgement and workspace rules the built-in prompt carries.

An `AGENTS.md` in the working directory, the file the other harnesses read, is appended after
`SYSTEM.md` under the same heading, in both prompts, capped at 6,000 characters (a longer one is
cut with a marker). A project written for another agent therefore brings its instructions along;
`SYSTEM.md` remains the place for what is rufous's alone (its persona, a replaced base).

## The environment line
The task prompt ends with one line the model would otherwise have to guess at: the date, whether the
working directory is a git repository (and its branch, read from `.git/HEAD`), and which of the common
local tools are on PATH (`git`, `rg`, `fd`, `jq`, `python3`, `uv`, `node`, `npm`, `cargo`, `go`, `dart`,
`flutter`, `docker`, `make`, `gh`, `bd`), probed once per run. It is a measured addition like every
other: `AGENT_ABLATION=env_block` turns it off for a comparison run. The Q&A prompt does not carry it.

## MCP servers
Skills cover "how to do it"; MCP covers "which extra tools exist". MCP servers are configured in `~/.ollama_agent/mcp.json`;
once configured, their tools are routed by category (`files`, `code`, `web`, `memory`, `mcp`) and the model calls them as
`server.tool`; see `mcp.json.example` in the repository root for the format. The "Skills" window in the UI also lists the configured MCP servers.

A project brings its own servers in `<workdir>/.mcp.json`, the file Claude Code, Cursor and Codex read: either their
`{"mcpServers": {...}}` wrapper (a `type` key is ignored; `command`/`args`/`env` and `url`/`headers` are the same keys) or
rufous's bare map, and `categories`/`tools`/`expose` work in both. The project's servers are added to the home file's, and
a project server replaces a home server of the same name. Under an editor, the servers the client passes in
`session/new` are added on top for that session. A server is still launched only when a tool of its category is routed
or enabled, so a project's `.mcp.json` runs processes the project chose, as its hooks do: read it before running rufous
there; `AGENT_PROJECT_MCP=0` ignores it. `rufous doctor` counts the project's servers separately. Not yet: `${VAR}`
expansion in `env` values, the SSE transport (an `sse` entry is tried as streamable HTTP).
