# Boubou · 布布

A local-model autonomous agent, written in Rust. Boubou is the successor to
[Mingbird (鸣鸟)](../Mingbird-agent), the Python/Tk original, and keeps its two ideas:

- **Tiny prefill.** System prompt plus nine core tool schemas, about 1250 tokens, so a 1B–4B
  model running under Ollama can drive it.
- **Progressive disclosure.** Advanced tools are enabled on demand with `enable_tools`;
  skills are listed by name and loaded only when used.

The name: the Southern Boubou, *Laniarius ferrugineus*, is a rust-coloured African bush-shrike
that sings in duet. *Ferrugineus* means "rusty".

## Status

Core in progress. Typed protocol, config, skills, Ollama client, the five-ring safety gate,
the tool executor (files, commands, todo, crawl guard), all pure guards and the agent loop
(finish gates, anti-loop ladders, compaction, checkpoint/session/transcript files) are ported
and tested. `boubou run <model> <taskfile> <workdir>` works end to end against a local Ollama;
sessions it writes load in Mingbird and vice versa. Web search/fetch (shared result cache),
long-term memory, parallel `batch_tools` and MCP (stdio and streamable HTTP via `rmcp`, tools
flattened into the categories as `server.tool`) are in. `boubou doctor` probes every MCP server;
`boubou tool <name> '<json>'` runs any single tool, built-in or `server.tool`, for debugging.
Web search defaults to Bing, then DuckDuckGo, then Baidu (`AGENT_SEARCH_BACKENDS` overrides).
`boubou serve` runs the local web UI (Mingbird's page and `/api/*` contract, agent in-process,
approval dialogs, stop); `cargo run -p boubou-app` opens the same UI in a native window.
Parallel dispatch is in and off by default: with `"parallel": {"enabled": true, "child_model":
"<tag>"}` in `config.json`, a plan with enough simple single-file items is farmed out to
sandboxed child processes (`AGENT_CHILD_SANDBOX=1`, default-deny tool face, writes only inside
the child directory, audit log, exit 53 on a severe violation). `boubou plan` shows what this
machine could run. The engine follows `config.json` like Mingbird: Ollama (restarted
automatically after four consecutive 500s), an OpenAI-compatible local server (LM Studio,
llama-server) or a cloud provider. Voice input is behind the `voice` cargo feature
(`cargo build --features voice`, sherpa-onnx linked statically): `boubou voice <wav>` and the
web UI's mic button transcribe with Mingbird's bundled zh/en models from `~/.ollama_agent/stt/`
or `BOUBOU_STT_DIR`, falling back to an Ollama audio model. Next: voice model installer, Tauri
bundling, the LRAB bench adapter.
See [docs/PORTING.md](docs/PORTING.md).

`run_python` and the three skills that depended on it (office_word, office_excel, image_batch)
are not carried over; Boubou ships no Python runtime.

## Compatibility with Mingbird

Boubou reads Mingbird's data directory unchanged (`~/.ollama_agent`, or `BOUBOU_HOME` /
`MINGBIRD_HOME`): the same `config.json`, `mcp.json`, `skills/` and sessions.
Tool schemas are the same JSON Mingbird sends, so prompts and the bench numbers carry over.

## Build

```sh
cargo build --release            # core + CLI (boubou)
cargo build --release -p boubou-app   # Tauri desktop shell (heavier; needs the Tauri prerequisites)
cargo test
```

Windows binaries link the C runtime statically (see `.cargo/config.toml`). To cross-compile
from macOS or Linux: `cargo install cargo-xwin && cargo xwin build --release --target x86_64-pc-windows-msvc`.

## CLI

```sh
boubou doctor                    # config, Ollama reachability, models, skills, MCP servers
boubou models
boubou skills [workdir]
boubou tools --enable web_fetch  # print the schemas the model receives
boubou run <model> <taskfile> <workdir> [--session NAME] [--new] [--append]
boubou chat <model> <workdir>
```

Environment variables are inherited from Mingbird: `OLLAMA_HOST`, `OLLAMA_BIN`, `AGENT_CTX`,
`AGENT_TEMP`, `AGENT_NUMPREDICT`, `AGENT_THINK`, `AGENT_SYSTEM_FILE`, `AGENT_SLIM_OUTPUT`,
`AGENT_OFFLINE`.

## Layout

```
crates/boubou-core   protocol, config, skills, MCP config, Ollama client, agent surface
crates/boubou-cli    `boubou` binary
crates/boubou-app    Tauri shell embedding webui/index.html (not built by default)
skills/              built-in skills, carried over from Mingbird unchanged
docs/PORTING.md      Python → Rust module map and remaining work
```

## License

Apache-2.0, as Mingbird.
