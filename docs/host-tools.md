# Host tools: MCP servers from `session/new` reach the model (boubou-c9a)

Choreo's item 2 (requirements doc: https://claude.ai/artifact/3LnyuZaigSjibqGMGrfTDM). Draft, 2026-11-05.

## The problem

A desktop host (Choreo) starts `rufous acp --policy general` and passes its own MCP server in
`session/new`; each of the user's automations is one tool there. Today the model sees those tools
only when the user's words trip a keyword:

- A question turn gets `face.chat_tool_defs()` and no MCP tools at all (`run.rs`, the `ct` match).
  "…then tell me exactly what it returned" is a question to the heuristic decider.
- On a task turn, `mcp_config_from_acp` gives the server no categories, so its tools land in `mcp`.
  That category routes only on "mcp", "external tool", "tool server", "plugin", "server", "api",
  "database" (`CAT_KEYWORDS`, `face.rs`).
- `McpRegistry::tool_defs` cuts every description to 60 characters, which loses what an automation
  does and needs.
- The system prompt says "Need web, memory or MCP tools? Call enable_tools first" (`prompt.rs:25`).
  A small model follows the text it is given, not the state behind it, so the text has to name the
  tools.

Session `acp_1105_105519` failed: the model reached for `skills load "Say Hello"` and replied that no
such automation exists. Session `acp_1105_105645`, with the prompt worded around MCP, worked first time.

## Design

A server the client passes in `session/new` is the host's own tool set, not a library for the model to
discover. When host tools are on, its tools are part of the face on every turn and every route.
They are flattened `server.tool` schemas with longer descriptions, and the system prompt names them.
`mcp.json` / `.mcp.json` servers keep today's routing. The host set is fixed for the session, so the
tool list stays byte-identical from turn to turn and the prefix cache holds (boubou-g11).

1. **Marking.** `ServerConfig` gets `host: bool` (`#[serde(skip)]`, never read from `mcp.json`).
   `mcp_config_from_acp` sets it when the session runs with host tools on. A host server's name wins
   over a same-named `mcp.json` server: `runner.rs` already extends the configured servers with the
   `session/new` ones.
2. **Opt-in.** `rufous acp --host-tools on|off`. The default follows the policy: `on` for `general`,
   `off` for `coding`. Editors (Zed passing its own context servers) stay as they are, and
   Choreo needs no extra flag. A per-server `_meta` hint can come later if the ACP schema carries one;
   the flag is enough for one host process per app.
3. **Assembly.** `McpRegistry::host_defs(offline) -> Vec<Value>`: every tool of every host server,
   sorted by server and then tool name, with no category, `expose` or built-in-duplicate filter.
   `tool_defs` skips host servers so nothing is listed twice. In `run.rs`, all three `ct` arms append
   `host_defs`: the question route, the routed task route and the flat-prefill fallback. The question
   route keeps its other limits: `QA_BLOCKED` covers only the file mutators, so a host call is allowed there.
4. **Descriptions.** Host tools keep up to 200 characters (`HOST_DESC_CAP`); others keep 60. Their parameters also keep
   their descriptions, and every MCP tool keeps its `enum`s and its `required` list as sent
   (docs/mcp-schemas.md, boubou-erw).
5. **Prompt.** When the session has host tools, one line replaces the MCP half of `prompt.rs:25`:
   *"Your host app's tools are in your tool list as `<server>.<tool>`; use them when the user asks
   for one of its automations or what one returned."* The server name comes from the config, so the
   line says `choreo.say_hello`, not a placeholder.
6. **Cap.** At most `HOST_TOOL_CAP` host tools are inlined (a constant to start, set from the
   bench below). Past the cap, host servers fall back to category `mcp` with `mcp` always in `cats`,
   so `mcp_call` and `enable_tools` are on the face. The prompt line then lists the tool names (no
   schemas) and says to call `enable_tools` with one. That is still better than today: the names are
   in the text. A `[face] host tools: N inlined | over cap N>M, on demand` log line records which.
7. **Offline.** A host server is a `url` server, so `AGENT_OFFLINE` drops it as today, even though
   Choreo's is loopback. A per-session offline switch (item 4) will need to exempt `127.0.0.1` host
   servers; noted here, decided there.

Not changed: the safety gate does not vet MCP calls today and still won't. Choreo approves each
run on its side, which is what it asked for. `is_mcp_producing` counts `say_hello` as unproductive;
see the risks below.

## Measure, don't guess: the cap

Prefill is what a host tool costs. Per tool: name, a description of up to 200 characters, and the
compacted schema, about 60–120 tokens. On the bench, not in a live session:

- Bench `qwen3.5:4b` and `gemma4:e2b` with 0, 5, 10, 15 and 25 synthetic host tools, using descriptions
  from the shape of Choreo's. Record the prompt tokens of the first call, the time to first token and
  the tool-choice accuracy on a fixed set: right tool, wrong tool, no tool, and a turn that needs no host tool.
- Set `HOST_TOOL_CAP` at the largest count where host prompts keep their accuracy and plain
  prompts stay at the 0-tool baseline (no host call). Prefill cost is recorded, not gated: a
  percentage rule is unstable when the base is under a second (gemma4:e2b) or noisy (qwen3.5:4b
  measured faster at N=5 than at N=0). Choreo expects 3–15.

**Reading, 2026-11-05** (`bench/host_tools.py`, 3 runs, `bench/results/host_tools/2026-11-05`):

| Model | N | Choreo's prompt | Weekly / tidy | Plain (no host call) | Prompt tok | Prefill |
|---|---|---|---|---|---|---|
| qwen3.5:4b | 0 | – | – | 9/9 | 1738 | 3.3 s |
| | 5 | 3/3 | 6/6 | 9/9 | 2294 | 2.9 s |
| | 10 | 3/3 | 6/6 | 9/9 | 2696 | 3.4 s |
| | 15 | 3/3 | 6/6 | 9/9 | 3083 | 4.4 s |
| | 25 | 2/3 | 6/6 | 9/9 | 3785 | 5.2 s |
| gemma4:e2b | 0–25 | 3/3 each | 6/6 each | 9/9 each | 1406 → 3114 | 0.44 → 0.92 s |

About 75 prompt tokens per host tool. qwen first slips at 25 (it dropped the run id it was asked to
quote). `HOST_TOOL_CAP` = 15. "What did Say Hello return for Ada?" in a fresh session fails on qwen at
every N, so it is not a cap effect: bead boubou-8c4.

## Risks

- **Distraction.** Host tools on every turn may pull the small model toward them on unrelated
  turns. The no-host-tool set in the bench is there to catch that.
- **Anti-loop counters.** A host tool not named like `create_`/`write_` does not reset the
  research/todo counters (`PRODUCTIVE`, `is_mcp_producing`). Two runs of one automation in a turn
  could trip a streak guard. Proposal: count any host-tool call that returns without error as
  productive.
- **Question-route finish.** A question turn ends on text. It needs a test that the model can make
  the host call and then answer in the same turn.

## Done when

- The original prompt (*Use the Choreo "Say Hello" automation to greet Ada, then tell me exactly
  what it returned.*) runs the automation over ACP with `qwen3.5:4b` and `gemma4:e2b`. So does
  "What did Say Hello return for Ada?". Choreo's `test/mcp_rufous_e2e_test.dart` passes with its
  default prompt.
- An editor session (`--policy coding`, no flag) assembles the same tool list as before, byte for
  byte, and the lrab numbers are unchanged.
- Tests: host tools on the question route; `tool_defs` skips host servers; the 200-character
  description cap; the fallback past the cap and its prompt line; the flag default following the policy.

## Answers for Choreo's open questions

- Always-on or opt-in: opt-in by flag, on by default under `--policy general`, so Choreo passes
  nothing extra.
- Prefill budget: set from the bench above; until then, keep the list at 15 tools or fewer.
