# Skills directory

A Boubou "skill" is an operating manual for the model (a .md file). Normally the agent only knows each skill's name and one-line description (saving tokens); when it needs one it loads the full text with `skills(load)`. This is "progressive loading".

## Directories and precedence
Lookup order (on a name clash, the earlier one wins):
1. `<workdir>/skills/` -- skills specific to the current task
2. `~/.ollama_agent/skills/` -- global skills you installed yourself (recommended location)
3. `<install dir>/skills/` -- a `skills/` directory beside the binary, if you keep one there
4. the skills built into the product -- compiled into the binary from `crates/boubou-general/skills/` (documents, folders, research) and `crates/boubou-coding/skills/` (code); the `boubou` binary carries both sets, an editor session sees the coding ones, the web UI and `boubou chat` the general-purpose ones

## Installing your own skill
Put a .md file in `~/.ollama_agent/skills/` with two lines of metadata at the top:

```
---
name: my_skill
description: One sentence saying when to use it
---
Body: the operating steps (instructions for the model)...
```

After restarting Boubou it appears under the "Skills" button in the UI. Follow the built-in skills in this directory as a model: step by step, tool names written in full (list_dir / run_bash / web_search ...), and the red lines (what must not be done) at the end.

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

## Per-folder instructions: SYSTEM.md
A `SYSTEM.md` in the working directory is appended to the system prompt for task and chat turns
alike, under "PROJECT INSTRUCTIONS". If its first line is `<!-- boubou: replace -->` it replaces the
built-in prompt instead (the Settings → System prompt override, when enabled, still wins the base and
the file is appended under it). Use append for a persona, house rules and output formats; use replace
only when you also take over the intent judgement and workspace rules the built-in prompt carries.

## MCP servers
Skills cover "how to do it"; MCP covers "which extra tools exist". MCP servers are configured in `~/.ollama_agent/mcp.json`;
once configured, their tools are routed by category (`files`, `code`, `web`, `memory`, `mcp`) and the model calls them as
`server.tool`; see `mcp.json.example` in the repository root for the format. The "Skills" window in the UI also lists the configured MCP servers.
