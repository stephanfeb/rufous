# Porting Mingbird → Boubou

Mingbird is ~11K lines of Python. The port keeps every on-disk and wire format and replaces the
Tk GUI with a Tauri shell around the existing web UI.

## Module map

| Python (Mingbird) | Rust (Boubou) | Status |
|---|---|---|
| `appconfig.py` | `boubou-core/src/config.rs` | ported, tested |
| `ollama_agent.py` tool schemas (`CORE_TOOLS`, `ADVANCED_TOOLS`, `_REQ_ARGS`, `_coerce_args`, `_TOOL_ALIASES`) | `boubou-core/src/tools.rs` | ported, tested; `run_python` dropped (see below) |
| `ollama_agent.py` HTTP + NDJSON streaming | `boubou-core/src/ollama.rs` | ported, decoder tested; needs a live-Ollama integration test |
| skills discovery / `skills` tool | `boubou-core/src/skills.rs` | ported, tested |
| `mcp.json` parsing + category tagging (`categories`, `tools` as list or `{categories, expose}`, server `expose`, `headers`, unknown keys ignored) | `boubou-core/src/mcp/mod.rs` | ported, tested |
| `mcp_call`, `_probe_stdio`/`_probe_http`, `mcp_manifest` (memory + disk cache, stale-while-revalidate, 60 s TTL after a failed probe), `mcp_tool_defs` (flattened `server.tool` defs, expose tiers, built-in/file-verb dedupe, offline skips `url` servers), `_resolve_mcp_paths`, `_compact_schema` | `boubou-core/src/mcp/{manifest,client}.rs` | ported, tested (`rmcp` 3.8: `TokioChildProcess` + `StreamableHttpClientTransport`; probe 10 s, call 60 s inside a 120 s budget; argument errors append the tool's expected parameter names; `mcp_manifest_cache.json` is byte-compatible). Not yet: process-*group* kill of a hung launcher's children (rmcp kills the direct child), `expose` is honoured but `doctor` does not display it |
| `_gate_check`, `_risk_classify`, `_safe_path`, `_DANGER_CMD`, `_BASH_ESCAPE_PATTERNS`, `_SENSITIVE_PATTERNS` (rings 1–3) | `boubou-core/src/safety.rs` | ported, tested; `Confirmer` trait replaces the stdin `@@ASK@@` channel; child sandbox (ring 0) todo |
| `run_tool`: create/read/edit/append/delete/list/search/run_bash/todo/skills/enable_tools/finish/batch_tools, `.bak`, `.mingbird_trash/`, overwrite guard, crawl guard v2, `_bash_snapshot`, `_auto_repair` (text files), `_format_bash_out`, slim output, timeout tree-kill | `boubou-core/src/exec.rs` | ported, tested. `batch_tools` gates every call first, then runs network calls on scoped threads and the rest serially, output blocks in call order. Not yet: `mcp_call` (stub that says so), `.py` auto-repair (needs an interpreter), Windows-only path hints are `cfg!(windows)` |
| `web_search` / `web_search_multi` / `web_fetch`, `_cache_key`/`_cache_get`/`_cache_set` | `boubou-core/src/web.rs` | ported, tested (loopback server, no live network in tests). `reqwest::blocking` + `scraper` with Mingbird's Bing/Baidu selectors; cache keys are byte-identical so both stacks share `<home>/cache/`; `AGENT_SEARCH_BACKENDS` honoured. Live-checked against Bing and Wikipedia 2026-10-27 |
| `load_memory`/`save_memory`/`memory_store`/`memory_recall` | `boubou-core/src/memory.rs` | ported, tested; same `memory.json` shape and (score, text, ts) ordering |
| `todo_file`/`load_todo`/`save_todo`/`format_todo_lines`/`todo_progress`, `_atomic_write_json` | `boubou-core/src/todo.rs` | ported, tested |
| `_model_params_b`, `_semantically_empty`, `_estimate_tokens` | `boubou-core/src/guards/mod.rs` | ported, tested |
| `_is_qa`, `_has_task_progress`, `_task_original_text`, `_HARNESS_NOTE_PREFIXES` | `boubou-core/src/guards/qa.rs` | ported, tested |
| `try_parse_tool_calls`, `_parse_func_call_expr` (format-leak salvage) | `boubou-core/src/guards/leak.rs` | ported, tested |
| `_claimed_missing_files`, `_plan_named_missing` | `boubou-core/src/guards/artifacts.rs` | ported, tested |
| `_EditConvergenceGuard`, `_norm_p` | `boubou-core/src/guards/edit.rs` | ported, tested (case-insensitive on all platforms, by design) |
| `parse_time_budget`, `parse_duration_str`, `resolve_time_budget`, `_budget_nudge`, elapsed sidecar | `boubou-core/src/guards/budget.rs` | ported, tested; module globals (`BUDGET_SEC`, `TIME_BUDGET_PARSE`) became explicit parameters |
| `agent_loop` (empty-turn ladder, hard reset, six finish gates, dup/streak/research/todo anti-loop, failure ladder, budget pacing, stale-plan nudge, Q&A wrap-up), `_FAIL_MARKERS`, `_ws_note` | `boubou-core/src/agent/run.rs` | ported, tested against the scripted-loop suite and a live gemma4:12b run. Not yet: `restart_ollama` on 4× 500 (logged, falls through to compaction retry), `_maybe_parallel_dispatch`, `_child_sandbox` |
| `call_chat` (streaming, `think` 400 retry, tri-state options) | `boubou-core/src/agent/chat.rs` | ported as the `ChatBackend` trait + `OllamaBackend`; cloud / OpenAI-compatible backends todo |
| `_active_tools`, `_disabled_tools`, `route_categories`, `tools_for_categories`, `_chat_tool_defs`, `_filter_offline_tools`, `_is_mcp_producing` | `boubou-core/src/agent/face.rs` | ported (`ToolFace`); MCP flattened tool defs join when the client lands |
| `compact_history` L1/L2/L3, `_estimate_messages_tokens`, `_workdir_files_text`, `_dedupe_trailing_assistant`, `sanitize_ckpt`, `_similar`, `_attach_pending_images` | `boubou-core/src/agent/history.rs` | ported, tested; `_similar` is a difflib-style matching-blocks ratio without the autojunk heuristic |
| `.agent_state.json`, `<s>.json`, `<s>.transcript.jsonl`, `<s>.meta.json`, `<s>.todo.json`, `_transcript_flush` cursor rules | `boubou-core/src/agent/session.rs` | ported, tested; verified cross-readable by Mingbird's `sanitize_ckpt`/`load_session` |
| `SYSTEM`, `CHAT_SYSTEM`, `system_prompt()`, `platform_profile.prompt_block` | `boubou-core/src/agent/prompt.rs` | ported; `CHAT_SYSTEM` introduces itself as 布布 |
| `_run_test_guard`, `_pytest_hint`, `_test_env_broken` | `boubou-core/src/agent/testguard.rs` | ported (runs `python -m pytest -q` in the workdir when `test_*.py` exist) |
| `main()`: `--session/--new/--append/--time-budget`, checkpoint/session resume, `--chat` | `boubou-cli/src/main.rs` | ported; not covered by tests yet |
| `platform_profile.py` | `boubou-core/src/agent/prompt.rs` (`Platform`, `platform_block`) | ported |
| `parallel_*.py` (dispatch, probe, safety, todo) | `boubou-core/src/parallel/` | todo; config section kept opaque in `Config::parallel` |
| `run_python` (bundled interpreter) | — | **dropped** (decision 2026-10-27): no Python runtime in Boubou. The `office_word`, `office_excel` and `image_batch` skills went with it; `doc_digest` and `file_organizer` still mention `run_python` and need rewording. Revisit later. |
| `voice_input.py` (sherpa-onnx) | `boubou-app` | todo: `sherpa-rs`; the one native lib that may ship as a DLL |
| `agent_gui.py`, `ui_v4.py`, `ui_theme.py` (Tk) | dropped | replaced by Tauri shell |
| `webui/server.py` | `boubou-app/src/` | todo: expose the `/api/*` surface below as Tauri commands or a loopback server |
| `mcp_utils_server.py` | — | keep as a Python example, or rewrite as a tiny Rust MCP server |
| `tests/` (33 files, 556 test functions) | `boubou-core/tests/` | in progress, see below |

## Test port status

| Python file | Rust file | Ported | Still deferred |
|---|---|---|---|
| `test_guards.py` | `tests/guards.rs`, `tests/time_budget.rs`, `tests/safety.rs`, `tests/agent_loop.rs` | 80 of 96 | `main()` headless CLI (5), `_model_can_think` probe (2), `call_chat` payload shape (1), docstring checks |
| `test_todo.py` | `tests/todo.rs`, `tests/guards.rs`, `tests/history.rs` | 26 of 27 | `test_mcp_tool_counts` folded into has_task_progress |
| `test_safety_net.py` | `tests/safety.rs`, `tests/exec_tools.rs` | 17 of 17 | — (five-ring docstring check became the module doc) |
| `test_crawl_guard.py` | `tests/exec_tools.rs` | 8 of 8 | — |
| `test_output_cap.py` | `tests/guards.rs`, `tests/agent_loop.rs` | 4 of 5 | `NUM_PREDICT` env override at import time (Boubou reads it in `RunParams::from_env`; the default is asserted) |

`tests/agent_loop.rs` also adds loop tests Mingbird never had: explicit `budget_sec` beating the
env, substantive-summary delivery, the todo sync gate's two-refusal limit, `enable_tools`
unlocking a ban, duplicate-call interception, the empty-ladder graceful exit, Q&A force-close,
repeat-output dead loop, the harness-prefix invariant over a mixed run, and the session file
shapes. `tests/web_memory.rs` covers `tests/test_core.py::TestCache` / `TestBatchTools` plus
search-result parsing against bs4's output, `web_fetch` against a loopback server (full text under
`sources/`, cache hit on the second call), memory store/recall ordering, and a timing test that
proves two `web_fetch` calls in one `batch_tools` overlap. `tests/mcp.rs` covers
`test_core.py::TestMcpManifest::test_expose_parser`, `test_v200_perf.py`'s probe / timeout-kill /
stale-merge-and-short-TTL tests and `mcp_tool_defs` filtering, plus the executor calling a real
stdio server (`tests/fixtures/fake_mcp.py`, needs `python3`, skipped when absent). 148 tests total.

Counts are test functions, not parametrised cases. Several Python tests were merged where they
shared a fixture; the assertions are all present.

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

1. ~~Port the guard/executor tests and implement against them.~~ Done 2026-10-27 (81 tests green).
2. ~~Agent loop against the deferred wiring tests above; checkpoint/transcript formats byte-compatible.~~ Done 2026-10-27 (119 tests green; live run against gemma4:12b created a file, verified it, resumed with `--append`; Mingbird's Python loaders read the files Boubou wrote).
3. ~~web_search / web_fetch / memory tools, then `batch_tools` parallelism for network calls.~~ Done 2026-10-27 (135 tests green; `boubou tool web_search '{"query":…}'` and `boubou tool web_fetch …` return real results; `mcp_call` is the last stub).
4. ~~MCP client via `rmcp`.~~ Done 2026-10-27 (148 tests green; `boubou doctor` probes every server in parallel and lists its tools; live-checked against the fake server and `npx @modelcontextprotocol/server-filesystem`: flattened `filesystem.read_text_file` with a relative path resolved against the workdir, sandbox hint on a refused path, manifest cache written).
5. Tauri shell: `/api/*` surface, then voice input.
6. Parallel dispatch.
7. Child sandbox (ring 0) when sub-agents arrive.

## Step 5 brief: Tauri shell (handoff notes, 2026-10-27)

The web UI contract is listed above (`webui/server.py` → `/api/*`). `crates/boubou-app` holds
Mingbird's `index.html` verbatim and a stub `main.rs`. Wire each endpoint as a Tauri command or
a loopback HTTP server reusing `Agent::standard` with a sink that forwards `AgentEvent`s to the
page (SSE in Mingbird; Tauri events are the natural replacement). Sessions, todo, skills, model
list and MCP manifest already have core functions. Voice input (sherpa-onnx, Chinese 14M model)
comes after the text path works. The toolchain is Rust 1.99 now, so `tauri 2` / `dlopen2 0.8`
compile; `rust-version` is 1.88 (the `rmcp` floor).

Search backends: Boubou defaults to Bing → DuckDuckGo HTML → Baidu (western-first; the user is
in Southeast Asia). Mingbird's `cn.bing.com` host is therefore not shared in the search cache;
`web_fetch` entries still are. `AGENT_SEARCH_BACKENDS` overrides the list.

Known loop gaps to pick up when convenient: `restart_ollama` after four consecutive 500s (needs
`ollama_exe`/`ollama_env` from `Config`), the cloud / OpenAI-compatible backends behind
`ChatBackend`, `_model_can_think` probing, and the `main()` CLI tests (drive `boubou run` with a
scripted backend through a test-only entry point).
