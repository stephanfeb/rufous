# Where data goes: egress reports, the model host, per-session offline (boubou-45n)

Choreo's item 4 (requirements doc: https://claude.ai/artifact/3LnyuZaigSjibqGMGrfTDM). Draft, 2026-11-05.

## The setting

For every run, Choreo shows the user which hosts were contacted and from which step. That record
is its main privacy claim. rufous's own traffic bypasses Choreo, so rufous has to report it. What
Choreo can see over ACP today, checked on 2026-11-05:

| Traffic | Today |
|---|---|
| `web_fetch` | The URL is in `rawInput`. Redirects to other hosts are not visible. |
| `web_search`, `web_search_multi` | The query only. The engines contacted are not visible: Brave, then DuckDuckGo, then Bing (`AGENT_SEARCH_BACKENDS`), plus a direct fetch when the query names a site. |
| MCP HTTP servers (`mcp.json`, `.mcp.json`, `session/new`) | Not visible. The tools/list probe also connects, outside any tool call. |
| The model engine | Not visible. The `[engine]` line goes only to the session log. |
| `run_bash` | Not visible, and rufous cannot know. Choreo labels it unrecorded. |

Offline mode (`offline_mode`, `AGENT_OFFLINE`) applies to the whole process. It works by *hiding*
tools: `ToolFace::filter_offline` drops the web tools, and `McpRegistry` skips every `url`
server, including a host's loopback server such as Choreo's own. The executor refuses nothing, so
a web tool reached another way (`batch_tools`, a name the model remembers from an earlier turn)
still runs. An enabled cloud provider is disabled offline (`Config::cloud_provider`). A named
`openai`/`lmstudio` engine on another machine, or an `OLLAMA_HOST` on another machine, is used
as normal.

## Design

1. **One recorder per executor.** `egress.rs` adds `Egress { host, purpose, local }` and
   `EgressLog`, a shared `Arc<Mutex<Vec<Egress>>>` with `record(url, purpose)` and `take()`.
   - `host` is the URL's host, plus `:port` when the port is not the scheme's default.
     `127.0.0.1:8765` and Choreo's own server should stay distinguishable from an Ollama on the
     same machine.
   - `local` is true for loopback (`localhost`, `127.0.0.0/8`, `::1`).
   - The `Executor` owns the log and hands clones to its `Web` and its `McpRegistry`.
   - A cache hit contacts nothing and records nothing.

   What records:

   - `Web::get` records each request's host. The purpose is `web_search` or `web_fetch`,
     including the direct fetch a site-naming search does.
   - A redirect policy on the `Web` client records every hop's host, with the purpose
     `redirect`, before it follows. The default limit of 10 hops is kept.
   - `McpRegistry` records the server's URL host with the purpose `mcp:<server>` when it calls
     or probes a `url` server. A stdio server records nothing; what it does on the network is
     its own, like `run_bash`.

2. **Reported per tool call.**
   - `ToolRunner::take_egress()` (default: none) drains the log. After each tool runs, at the
     main call site and the corrector's substitute call, the loop drains it. When anything was
     recorded, the loop logs `[egress] host (purpose), …` and sends `AgentEvent::Egress(Vec<Egress>)`
     *before* that call's `ToolResult`.
   - The ACP bridge holds the list and puts it on that call's `tool_call_update` as
     `_meta.rufous.egress = [{"host", "purpose", "local"}]`.
   - The loop also drains the log once after the tool face is built, which covers the MCP probes.
     A list that arrives with no call open becomes an Info `notice` titled "Contacted …" with the
     same `_meta`, so Choreo records traffic made outside any tool call.
   - The CLI and the web UI log the event; neither shows it as conversation.

3. **The model host at session start.** The `session/new` and `session/load` responses carry
   `_meta.rufous.engine = {"label", "url", "host", "local"}`, plus `_meta.rufous.offline`. The label
   is Ollama, LM Studio, OpenAI-compatible or cloud. They come from the factory
   (`AgentFactory::session_meta`), which reads the same `Config` the turns use. A host that reads
   the response needs no notice and no turn.

4. **Per-session offline from the host.**
   - `session/new` and `session/load` accept `_meta.rufous.offline: true`. The session keeps the
     setting, and every turn runs with `Config::offline_forced` set, a runtime-only field that
     `offline()` honours over `AGENT_OFFLINE=0`.
   - The flag can only turn offline on. A session cannot turn off a process that is offline.

5. **Offline means this machine, enforced in the executor, not only in the face.**
   - Under offline the `Executor` refuses `web_search`, `web_search_multi` and `web_fetch`, and
     an `mcp_call` or host call to a `url` server that is not loopback. The result reads
     `[offline: <tool> would reach the network; this session stays on this machine]`, and the
     opening joins the failure markers.
   - The face and `McpRegistry` keep loopback `url` servers when offline, so a host's own
     server, which is the host's business, stays on the face.
   - A model engine whose host is not on this machine is refused when offline:
     `Config::backend_problem` reports `offline: the model engine at <url> is not on this
     machine`. That covers a cloud provider (already off), a named LM Studio or `openai` URL,
     and `OLLAMA_HOST`.

   That last point changes behaviour for anyone running offline with a LAN engine. I'm accepting
   that, because "stays on this Mac" is what Choreo's switch promises, and a LAN box is not this
   Mac.

6. **Not solved: `run_bash`.** The table in `docs/acp.md` says so, and Choreo labels shell use as
   unrecorded.

## Keys rufous commits to

Everything sits under the `rufous` key of `_meta`, so an ACP release cannot collide with it:

- `tool_call_update._meta.rufous.egress`
- `notice._meta.rufous.egress`
- `session/new` and `session/load` responses: `_meta.rufous.engine` and `_meta.rufous.offline`
- `session/new` and `session/load` requests: `_meta.rufous.offline`

These keys are documented in `docs/acp.md`. A change to them is a breaking change for Choreo, so
renaming a key means adding the new one alongside the old for a release.

## Tests

- `egress`: host and port, `local` for loopback, a cache hit records nothing.
- `web`: against a local server that redirects to a second local port, a `web_fetch` records both
  hosts, the second as `redirect`.
- `exec`: under offline, `web_fetch` and `mcp_call` to a non-loopback `url` server are refused
  without a request. A loopback `url` server is not.
- `config`: `offline_forced` wins over `AGENT_OFFLINE=0`. Offline refuses an LM Studio URL on
  another host and accepts `127.0.0.1`.
- `agent_loop`: a runner whose tool records egress yields `Egress` before that call's
  `ToolResult`.
- bridge: the egress goes onto the matching `tool_call_update` `_meta`; with no call open it
  becomes a notice.
- ACP (`acp_turn.rs`): the `session/new` response carries `_meta.rufous.engine`, and
  `_meta.rufous.offline` in the request comes back as `true`.
