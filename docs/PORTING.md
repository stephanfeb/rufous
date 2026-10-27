# Porting Mingbird → Boubou

Mingbird is ~11K lines of Python. The port keeps every on-disk and wire format and replaces the
Tk GUI with a Tauri shell around the existing web UI.

## Module map

| Python (Mingbird) | Rust (Boubou) | Status |
|---|---|---|
| `appconfig.py` | `boubou-core/src/config.rs` | ported, tested |
| `ollama_agent.py` tool schemas (`CORE_TOOLS`, `ADVANCED_TOOLS`, `_REQ_ARGS`, arg coercion) | `boubou-core/src/tools.rs` | ported, tested |
| `ollama_agent.py` HTTP + NDJSON streaming | `boubou-core/src/ollama.rs` | ported, decoder tested; needs a live-Ollama integration test |
| skills discovery / `skills` tool | `boubou-core/src/skills.rs` | ported, tested |
| `mcp.json` parsing + category tagging | `boubou-core/src/mcp.rs` | ported, tested |
| MCP client (stdio + streamable HTTP) | `boubou-core/src/mcp.rs` | todo: `rmcp` |
| `agent_loop`, finish gate, anti-loop, crawl guard, Q&A guard, compaction, checkpoints, transcript `.jsonl` | `boubou-core/src/agent.rs` | surface only; loop body todo |
| tool execution (`run_tool`, `.bak` backups, `.mingbird_trash/`, slim output) | `boubou-core/src/exec/` | todo |
| `platform_profile.py` | `boubou-core/src/platform.rs` | todo |
| `parallel_*.py` (dispatch, probe, safety, todo) | `boubou-core/src/parallel/` | todo; config section kept opaque in `Config::parallel` |
| `run_python` (bundled interpreter) | — | design decision needed: ship a Python runtime or drop `run_python` and the three office/image skills that depend on it |
| `voice_input.py` (sherpa-onnx) | `boubou-app` | todo: `sherpa-rs`; the one native lib that may ship as a DLL |
| `agent_gui.py`, `ui_v4.py`, `ui_theme.py` (Tk) | dropped | replaced by Tauri shell |
| `webui/server.py` | `boubou-app/src/` | todo: expose the `/api/*` surface below as Tauri commands or a loopback server |
| `mcp_utils_server.py` | — | keep as a Python example, or rewrite as a tiny Rust MCP server |
| `tests/` (631 pytest cases) | `boubou-core/tests/` | port as the behavioural spec *before* porting the loop |

## Web UI contract (`webui/server.py`)

GET: `/api/status`, `/api/sessions`, `/api/session`, `/api/plan`, `/api/events` (SSE).
POST: `/api/chat`, `/api/stop`, `/api/ask`, `/api/newchat`, `/api/offline`, `/api/cloud`,
`/api/session`, `/api/prefs`.

`crates/boubou-app/webui/index.html` is Mingbird's page, copied verbatim. It will render but
every call fails until the shell implements this surface.

## Static linking

- Rust code and `reqwest` with `rustls` link statically; no OpenSSL.
- Windows: `+crt-static` in `.cargo/config.toml`.
- Linux: build for `x86_64-unknown-linux-musl`.
- onnxruntime (behind sherpa-onnx) is the exception; static builds exist but expect to fight it.

## Order of work

1. Port the pytest suite for the loop and gates into `boubou-core/tests/` as failing tests.
2. Tool executor with `.bak`, trash, slim output, crawl guard.
3. Agent loop against the tests; checkpoint/transcript formats byte-compatible.
4. MCP client via `rmcp`.
5. Tauri shell: `/api/*` surface, then voice input.
6. Parallel dispatch.
