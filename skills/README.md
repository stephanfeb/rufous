# Skills directory

A Boubou "skill" is an operating manual for the model (a .md file). Normally the agent only knows each skill's name and one-line description (saving tokens); when it needs one it loads the full text with `skills(load)`. This is "progressive loading".

## Directories and precedence
Lookup order (on a name clash, the earlier one wins):
1. `<workdir>/skills/` -- skills specific to the current task
2. `~/.ollama_agent/skills/` -- global skills you installed yourself (recommended location)
3. `<install dir>/skills/` -- built-in skills shipped with the install (coding-oriented plus general-purpose)

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

## MCP servers
Skills cover "how to do it"; MCP covers "which extra tools exist". MCP servers are configured in `~/.ollama_agent/mcp.json`;
once configured, their tools are routed by category (`files`, `code`, `web`, `memory`, `mcp`) and the model calls them as
`server.tool`; see `mcp.json.example` in the repository root for the format. The "Skills" window in the UI also lists the configured MCP servers.
