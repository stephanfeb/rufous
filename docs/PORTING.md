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
| `mcp.json` parsing + category tagging | `boubou-core/src/mcp.rs` | ported, tested |
| MCP client (stdio + streamable HTTP) | `boubou-core/src/mcp.rs` | todo: `rmcp` |
| `_gate_check`, `_risk_classify`, `_safe_path`, `_DANGER_CMD`, `_BASH_ESCAPE_PATTERNS`, `_SENSITIVE_PATTERNS` (rings 1–3) | `boubou-core/src/safety.rs` | ported, tested; `Confirmer` trait replaces the stdin `@@ASK@@` channel; child sandbox (ring 0) todo |
| `run_tool`: create/read/edit/append/delete/list/search/run_bash/todo/skills/enable_tools/finish/batch_tools, `.bak`, `.mingbird_trash/`, overwrite guard, crawl guard v2, `_bash_snapshot`, `_auto_repair` (text files), `_format_bash_out`, slim output, timeout tree-kill | `boubou-core/src/exec.rs` | ported, tested. Not yet: web_search/web_fetch/web_search_multi/memory_*/mcp_call (stubs that say so), `.py` auto-repair (needs an interpreter), Windows-only path hints are `cfg!(windows)` |
| `todo_file`/`load_todo`/`save_todo`/`format_todo_lines`/`todo_progress`, `_atomic_write_json` | `boubou-core/src/todo.rs` | ported, tested |
| `_model_params_b`, `_semantically_empty`, `_estimate_tokens` | `boubou-core/src/guards/mod.rs` | ported, tested |
| `_is_qa`, `_has_task_progress`, `_task_original_text`, `_HARNESS_NOTE_PREFIXES` | `boubou-core/src/guards/qa.rs` | ported, tested |
| `try_parse_tool_calls`, `_parse_func_call_expr` (format-leak salvage) | `boubou-core/src/guards/leak.rs` | ported, tested |
| `_claimed_missing_files`, `_plan_named_missing` | `boubou-core/src/guards/artifacts.rs` | ported, tested |
| `_EditConvergenceGuard`, `_norm_p` | `boubou-core/src/guards/edit.rs` | ported, tested (case-insensitive on all platforms, by design) |
| `parse_time_budget`, `parse_duration_str`, `resolve_time_budget`, `_budget_nudge`, elapsed sidecar | `boubou-core/src/guards/budget.rs` | ported, tested; module globals (`BUDGET_SEC`, `TIME_BUDGET_PARSE`) became explicit parameters |
| `agent_loop`: empty-turn ladder, hard reset, finish gates wiring, research-streak ban, compaction, checkpoints, transcript `.jsonl` | `boubou-core/src/agent.rs` | surface only; loop body is the next step |
| `platform_profile.py` | `boubou-core/src/platform.rs` | todo |
| `parallel_*.py` (dispatch, probe, safety, todo) | `boubou-core/src/parallel/` | todo; config section kept opaque in `Config::parallel` |
| `run_python` (bundled interpreter) | — | **dropped** (decision 2026-10-27): no Python runtime in Boubou. The `office_word`, `office_excel` and `image_batch` skills went with it; `doc_digest` and `file_organizer` still mention `run_python` and need rewording. Revisit later. |
| `voice_input.py` (sherpa-onnx) | `boubou-app` | todo: `sherpa-rs`; the one native lib that may ship as a DLL |
| `agent_gui.py`, `ui_v4.py`, `ui_theme.py` (Tk) | dropped | replaced by Tauri shell |
| `webui/server.py` | `boubou-app/src/` | todo: expose the `/api/*` surface below as Tauri commands or a loopback server |
| `mcp_utils_server.py` | — | keep as a Python example, or rewrite as a tiny Rust MCP server |
| `tests/` (33 files, 556 test functions) | `boubou-core/tests/` | in progress, see below |

## Test port status

| Python file | Rust file | Ported | Deferred to the loop port |
|---|---|---|---|
| `test_guards.py` | `tests/guards.rs`, `tests/time_budget.rs`, `tests/safety.rs` | 61 of 96 | `_scripted_loop` wiring (shotgun/budget/finish/research-streak), `main()` CLI, `_model_can_think`, `call_chat` payload, docstring check |
| `test_todo.py` | `tests/todo.rs`, `tests/guards.rs` | 23 of 27 | `compact_history` plan re-injection (3), `test_mcp_tool_counts` folded into has_task_progress |
| `test_safety_net.py` | `tests/safety.rs`, `tests/exec_tools.rs` | 17 of 17 | — (five-ring docstring check became the module doc) |
| `test_crawl_guard.py` | `tests/exec_tools.rs` | 8 of 8 | — |
| `test_output_cap.py` | `tests/guards.rs` | 1 of 5 | `NUM_PREDICT` default/env (RunParams), truncation feedback and hard reset (loop) |

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
2. Agent loop against the deferred wiring tests above; checkpoint/transcript formats byte-compatible.
3. web_search / web_fetch / memory tools, then `batch_tools` parallelism for network calls.
4. MCP client via `rmcp`.
5. Tauri shell: `/api/*` surface, then voice input.
6. Parallel dispatch.
7. Child sandbox (ring 0) when sub-agents arrive.

## Step 2 brief: the agent loop (handoff notes, 2026-10-27)

Where the Python lives (`Mingbird-agent/ollama_agent.py`, v3.1.4 line numbers, approximate):

| Function | Lines | Notes |
|---|---|---|
| `call_chat` | ~2328 | streaming + 400 retry; `THINK` tri-state → top-level `think`, never in `options` |
| `_estimate_messages_tokens`, `_workdir_files_text` | ~2430 | compaction trigger inputs |
| `compact_history(model, messages, level, todo_text, files_text)` | ~2449 | L2/L3 re-inject `[当前计划(todo)` block; summary via `call_chat` |
| transcript `.transcript.jsonl`, checkpoint `.agent_state.json`, `[RESUMED checkpoint: N msgs]` | ~2572–2780 | byte-compatible formats required for resume |
| `_TASK_HINTS` / `_QA_HINTS` / `_is_qa` | ~2803–2886 | ported (`guards/qa.rs`) |
| `_dedupe_trailing_assistant` | ~2905 | ollama 400 "2 or more assistant messages" fix |
| `_similar` (repeat-output detector) | ~2960 | difflib ratio > 0.75 on first 200 chars, whitespace stripped |
| `agent_loop(model, messages, workdir, session, budget_sec=None)` | after ~2990 | the body to port |

Behaviours the deferred tests pin (all in `tests/test_guards.py` / `test_output_cap.py` / `test_todo.py`):

- Empty-turn ladder: `_semantically_empty` turns count; ≥3 → correction, 6 → hard reset that injects
  `上下文已重置` and re-seeds the original task. Truncated turn (`done_reason=length` + empty) gets a
  one-time chunked-delivery hint mentioning `append_file 逐段追加` and the actual `num_predict` value.
- Finish gates, in order: claimed-artifact check (`声称的以下产物`), plan-named check, then a one-time
  task re-read injection (`⚠️ ... 交付前自查 ... 重新 finish`) before the first allowed finish; skipped
  for Q&A. Bare finish with empty content *and* empty summary and no real work → `finish 被拒绝` with
  `下一步三选一`, `create_file`/`answer.txt` template, `继续用工具推进`, `分析/讲解类任务`, and the
  absolute workdir. Non-empty content or substantive summary = valid text delivery.
- Edit convergence: only receipts starting `[edited` feed `EditConvergenceGuard`; hint injected as a
  user message, tool never disabled.
- Research streak: identical `web_search` / `web_search_multi` fingerprints repeated ≥6 turns →
  `重复完全相同的网络查询` ban; distinct queries never banned. `enable_tools` unlocks a disabled tool.
- Budget: `budget_sec` param > `BUDGET_SEC` env; resume adds `load_elapsed`; nudges once per mark;
  sidecar written only when a budget is active.
- `_HARNESS_NOTE_PREFIXES` invariant: every injected user message starts with one of them.
- Compaction re-injects the todo text at L2 and L3; no marker when there is no plan.

Rust pieces already waiting for the loop: `agent::RunParams`, `agent::AgentEvent`, `exec::Executor`
(use `run_tool`), `guards::*`, `ollama::Client::chat_stream`, `todo::*`.
