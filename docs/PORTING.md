# Porting Mingbird → Boubou

Mingbird is ~11K lines of Python. The port keeps every on-disk and wire format and replaces the
Tk GUI with a Tauri shell around the existing web UI.

This file is the module map and port status only. Dated notes on what shipped and what was
measured live in [docs/changelog.md](changelog.md); current behaviour is documented in the
README and [docs/acp.md](acp.md).

## Module map

| Python (Mingbird) | Rust (Boubou) | Status |
|---|---|---|
| `appconfig.py` | `boubou-kernel/src/config.rs` | ported, tested |
| `ollama_agent.py` tool schemas (`CORE_TOOLS`, `ADVANCED_TOOLS`, `_REQ_ARGS`, `_coerce_args`, `_TOOL_ALIASES`) | `boubou-kernel/src/tools.rs` | ported, tested; `run_python` dropped (see below) |
| `ollama_agent.py` HTTP + NDJSON streaming | `boubou-kernel/src/ollama.rs` | ported, decoder tested; needs a live-Ollama integration test |
| skills discovery / `skills` tool | `boubou-kernel/src/skills.rs`; the built-in skills are compiled into the product crates (`boubou-general/skills/`, `boubou-coding/skills/`) | ported, tested |
| `mcp.json` parsing + category tagging (`categories`, `tools` as list or `{categories, expose}`, server `expose`, `headers`, unknown keys ignored) | `boubou-kernel/src/mcp/mod.rs` | ported, tested |
| `mcp_call`, `_probe_stdio`/`_probe_http`, `mcp_manifest` (memory + disk cache, stale-while-revalidate, 60 s TTL after a failed probe), `mcp_tool_defs` (flattened `server.tool` defs, expose tiers, built-in/file-verb dedupe, offline skips `url` servers), `_resolve_mcp_paths`, `_compact_schema` | `boubou-kernel/src/mcp/{manifest,client}.rs` | ported, tested (`rmcp` 3.8: `TokioChildProcess` + `StreamableHttpClientTransport`; probe 10 s, call 60 s inside a 120 s budget; argument errors append the tool's expected parameter names; `mcp_manifest_cache.json` is byte-compatible). Not yet: process-*group* kill of a hung launcher's children (rmcp kills the direct child), `expose` is honoured but `doctor` does not display it |
| `_gate_check`, `_risk_classify`, `_safe_path`, `_DANGER_CMD`, `_BASH_ESCAPE_PATTERNS`, `_SENSITIVE_PATTERNS` (rings 1–3) | `boubou-kernel/src/safety.rs` | ported, tested; `Confirmer` trait replaces the stdin `@@ASK@@` channel; child sandbox (ring 0) todo |
| `run_tool`: create/read/edit/append/delete/list/search/run_bash/todo/skills/enable_tools/finish/batch_tools, `.bak`, `.mingbird_trash/`, overwrite guard, crawl guard v2, `_bash_snapshot`, `_auto_repair` (text files), `_format_bash_out`, slim output, timeout tree-kill | `boubou-kernel/src/exec.rs` | ported, tested. `batch_tools` gates every call first, then runs network calls on scoped threads and the rest serially, output blocks in call order. Not yet: `mcp_call` (stub that says so), `.py` auto-repair (needs an interpreter), Windows-only path hints are `cfg!(windows)` |
| `web_search` / `web_search_multi` / `web_fetch`, `_cache_key`/`_cache_get`/`_cache_set` | `boubou-kernel/src/web.rs` | ported, tested (loopback server, no live network in tests). `reqwest::blocking` + `scraper` with Mingbird's Bing selectors (Baidu dropped: Boubou is English-only); cache keys are byte-identical so both stacks share `<home>/cache/`; `AGENT_SEARCH_BACKENDS` honoured. Live-checked against Bing and Wikipedia 2026-10-27 |
| `load_memory`/`save_memory`/`memory_store`/`memory_recall` | `boubou-kernel/src/memory.rs` | ported, tested; same `memory.json` shape and (score, text, ts) ordering |
| `todo_file`/`load_todo`/`save_todo`/`format_todo_lines`/`todo_progress`, `_atomic_write_json` | `boubou-kernel/src/todo.rs` | ported, tested |
| `_model_params_b`, `_semantically_empty`, `_estimate_tokens` | `boubou-kernel/src/guards/mod.rs` | ported, tested |
| `_is_qa`, `_has_task_progress`, `_task_original_text`, `_HARNESS_NOTE_PREFIXES` | `boubou-kernel/src/guards/qa.rs` | ported, tested |
| `try_parse_tool_calls`, `_parse_func_call_expr` (format-leak salvage) | `boubou-kernel/src/guards/leak.rs` | ported, tested |
| `_claimed_missing_files`, `_plan_named_missing` | `boubou-kernel/src/guards/artifacts.rs` | ported, tested |
| `_EditConvergenceGuard`, `_norm_p` | `boubou-kernel/src/guards/edit.rs` | ported, tested (case-insensitive on all platforms, by design) |
| `parse_time_budget`, `parse_duration_str`, `resolve_time_budget`, `_budget_nudge`, elapsed sidecar | `boubou-kernel/src/guards/budget.rs` | ported, tested; module globals (`BUDGET_SEC`, `TIME_BUDGET_PARSE`) became explicit parameters |
| `agent_loop` (empty-turn ladder, hard reset, six finish gates, dup/streak/research/todo anti-loop, failure ladder, budget pacing, stale-plan nudge, Q&A wrap-up), `_FAIL_MARKERS`, `_ws_note` | `boubou-kernel/src/agent/run.rs` | ported, tested against the scripted-loop suite and a live gemma4:12b run. `_maybe_parallel_dispatch` and child mode (`_child_sandbox`, `_CHILD_SYSTEM`, gate exemptions, exit 53) are in. `restart_ollama` on 4× 500 is in through `ChatBackend::restart_engine` (max 3 per run; an OpenAI-compatible engine returns `None` and the loop keeps the compaction retry) |
| `call_chat` (streaming, `think` 400 retry, tri-state options) | `boubou-kernel/src/agent/chat.rs` | ported as the `ChatBackend` trait + `OllamaBackend` (owns an `OllamaProc` for restarts); `DynBackend` boxes the engine chosen by `Agent::standard(params, &cfg)` |
| `_call_cloud`, `_call_openai_local`, `_cloud_to_openai_messages`, `_openai_normalize_message`, `_split_inline_thinking`, `_InlineThinkSplitter`, `probe_openai_backend` | `boubou-kernel/src/agent/openai.rs` | ported, tested (`OpenAiBackend` with `Flavor::Cloud` non-streaming / `Flavor::LocalEngine` SSE; tool-call deltas assembled by `index`; torn `<think>` tags held back; `probe`/`list_models` on `GET /models`) |
| `appconfig.cloud_provider` / `local_backend` / `backend_openai` / `engine_display_name` | `boubou-kernel/src/config.rs` | ported, tested (cloud off in offline mode; `openai` without `base_url` → Ollama) |
| `ensure_ollama`, `restart_ollama`, `_find_ollama_exe` | `boubou-kernel/src/ollama_proc.rs` | ported, tested (`OllamaProc`: kill `ollama serve` via taskkill/pkill, wait dead, respawn detached with `config.ollama_env`, wait for `/api/tags`). Not ported: Windows `_ollama_app_models_dir` sqlite lookup |
| `_model_can_think` (`/api/show` capabilities, per-model cache) | `boubou-kernel/src/agent/chat.rs` | ported, tested; `None` on an OpenAI engine or probe failure; `boubou doctor` prints it per model |
| `_active_tools`, `_disabled_tools`, `route_categories`, `tools_for_categories`, `_chat_tool_defs`, `_filter_offline_tools`, `_is_mcp_producing` | `boubou-kernel/src/agent/face.rs` | ported (`ToolFace`); MCP flattened tool defs join when the client lands |
| `compact_history` L1/L2/L3, `_estimate_messages_tokens`, `_workdir_files_text`, `_dedupe_trailing_assistant`, `sanitize_ckpt`, `_similar`, `_attach_pending_images` | `boubou-kernel/src/agent/history.rs` | ported, tested; `_similar` is a difflib-style matching-blocks ratio without the autojunk heuristic |
| `.agent_state.json`, `<s>.json`, `<s>.transcript.jsonl`, `<s>.meta.json`, `<s>.todo.json`, `_transcript_flush` cursor rules | `boubou-kernel/src/agent/session.rs` | ported, tested; verified cross-readable by Mingbird's `sanitize_ckpt`/`load_session`. Boubou addition (boubou-ll7): `Agent::checkpoint` relocates the live checkpoint and its elapsed sidecar; the ACP server writes `<sessions>/<s>.state.json` so the editor's project holds no harness state. The CLI keeps `<workdir>/.agent_state.json` for kill-resume |
| `SYSTEM`, `CHAT_SYSTEM`, `system_prompt()`, `platform_profile.prompt_block` | `boubou-kernel/src/agent/prompt.rs` | ported; all prompt text is Boubou's own English (see "Language" in `docs/changelog.md`) |
| `_run_test_guard`, `_pytest_hint`, `_test_env_broken` | `boubou-kernel/src/agent/testguard.rs` | replaced (2026-10-30): the gate re-executes the turn's own last check when files changed after it passed, and asks for a check when none ran in a tested workdir; it never picks a runner. `pytest_hint` kept, now also reads a plain runner's `FAIL` line |
| `main()`: `--session/--new/--append/--time-budget`, checkpoint/session resume, `--chat` | `boubou-cli/src/main.rs` | ported; `boubou-cli/tests/cli.rs` runs the binary headlessly (help, tools, skills, plan, tool, run with a missing task file, voice, doctor). `ensure_engine` runs before a task: probe an OpenAI engine (warn) or `OllamaProc::ensure` |
| `platform_profile.py` | `boubou-kernel/src/agent/prompt.rs` (`Platform`, `platform_block`) | ported |
| `parallel_config.py` (`DEFAULT_PARALLEL`, env overrides, `hard_disabled`, `current_depth`) | `boubou-kernel/src/parallel/config.rs` | ported, tested (`ParallelConfig`, typed; invalid values keep the current value; rule lists are data) |
| `parallel_safety.py` (`ChildSandbox`, path boundary, dangerous-command rules, audit JSONL, fuse) | `boubou-kernel/src/parallel/sandbox.rs` | ported, tested. Windows-style paths (`..\x`, `C:\…`, `\\wsl$`) are judged on every platform so the Python cases hold on macOS/Linux; POSIX adds `/etc/`, `chmod -R`, `launchctl`/`systemctl` rules |
| `parallel_probe.py` (`ResourceProbe`, `estimate_child_cost`, `plan_capacity`, `format_capacity_table`) | `boubou-kernel/src/parallel/probe.rs` | ported, tested with injected readers; `sysinfo` for RAM (macOS: `vm_stat` free + inactive first, psutil's figure; `sysinfo` subtracts the compressor and reads 0 with a model resident, bead boubou-wh5), `/api/ps` for loaded models; `boubou plan` prints the table |
| `parallel_todo.py` (`JsonTodoProvider`, `classify_task`, `safety_precheck`, `should_dispatch`) | `boubou-kernel/src/parallel/trigger.rs` | ported, tested; reason strings verbatim |
| `parallel_dispatch.py` (`ParallelDispatcher`, `ResultIntegrator`, `summary_for_parent`, `@@DISPATCH@@`) | `boubou-kernel/src/parallel/dispatch.rs` | ported, tested with a scripted `Spawner`; children are `boubou run <model> task.md <dir> --session disp_<run>_NN --new` in their own process group; a worker pool of `max_parallel` scoped threads |
| `_maybe_parallel_dispatch`, `install_child_mode`, `child_gate`, `_CHILD_SYSTEM` | `boubou-kernel/src/parallel/hook.rs`, `exec.rs` (`Executor::install_child_mode`, ring-0 check first in `prepare`), `agent/run.rs` (`RunParams::child_mode`, `AgentEvent::Dispatch`, `Agent::parallel`) | ported, tested. The library never calls `exit(53)`: the loop returns `Error::ChildTerminated` and the CLI maps it to exit 53 |
| `run_python` (bundled interpreter) | — | **dropped** (decision 2026-10-27): no Python runtime in Boubou. The `office_word`, `office_excel` and `image_batch` skills went with it; `doc_digest` and `file_organizer` still mention `run_python` and need rewording. Revisit later. |
| `voice_input.py` (sherpa-onnx, VAD recorder, `transcribe_ollama`, `pick_stt_model`) | `boubou-voice` crate, `boubou-general` `/api/voice` + mic button, `boubou voice` | ported behind cargo feature `voice` (`sherpa-onnx = "=2.0.1"` static; `sherpa-rs` only wraps offline recognizers). Default builds are native-free and report `built_without_voice_feature`. Same model dirs as Mingbird under `<install>/stt/`, `~/.ollama_agent/stt/` or `BOUBOU_STT_DIR`; no installer/downloader yet |
| `agent_gui.py`, `ui_v4.py`, `ui_theme.py` (Tk) | dropped | replaced by Tauri shell |
| `webui/server.py` (`/api/*`, SSE, `@@ASK@@` approval channel, `gui_prefs.json`, session rows, `kill_tree`) | `boubou-general/src/{api,state,runner,events,prefs}.rs` | ported, tested. axum on 127.0.0.1; the agent runs in-process on a thread, `AgentEvent`s map to the same SSE types (`tok`/`think`/`ask`/`dispatch`/`ctx`/`note`/`exit`) plus `tool_done` (tool result head, blocked flag) and a compaction `note`; `dispatch` carries the full `args`; `WebConfirmer` replaces the stdin round-trip (only the current `ask_id` is accepted); stop = a shared flag the loop and the Ollama stream both check (no process to kill). Cloud / OpenAI-compatible settings now select the engine through `Agent::standard`; `/api/status` reports `engine` and `voice`; `POST /api/voice` transcribes an uploaded WAV |
| `webui/static/index.html` | `boubou-general/static/index.html` | Redesigned (post-v0.1.0): rail / conversation / inspector layout, run blocks built from `dispatch` + `tool_done` events, inline permission cards, dark-first palette from the Boubou mark; same `/api/*` contract plus `/logo.png`. `include_str!` into the binary; `BOUBOU_WEB_INDEX=<path>` serves the page from disk for UI work without a rebuild |
| (none in Mingbird) | `boubou-coding` crate, `boubou acp` | new: Boubou as an ACP agent over stdio (see `docs/acp.md`) |
| Tk GUI (`agent_gui.py`) | `boubou-app/src/main.rs` | Tauri 2 shell: starts the web server on a free loopback port and opens a window on it. Voice comes from the page's mic button; `Info.plist` carries `NSMicrophoneUsageDescription` but is not wired into the bundle config yet |
| `mcp_utils_server.py` | — | keep as a Python example, or rewrite as a tiny Rust MCP server |
| `tests/` (33 files, 556 test functions) | `boubou-kernel/tests/` | in progress, see below |

## Test port status

| Python file | Rust file | Ported | Still deferred |
|---|---|---|---|
| `test_guards.py` | `tests/guards.rs`, `tests/time_budget.rs`, `tests/safety.rs`, `tests/agent_loop.rs` | 88 of 96 | docstring checks (the `main()` CLI, `_model_can_think` and `call_chat` payload cases live in `boubou-cli/tests/cli.rs` and `tests/openai_backend.rs`) |
| `test_todo.py` | `tests/todo.rs`, `tests/guards.rs`, `tests/history.rs` | 26 of 27 | `test_mcp_tool_counts` folded into has_task_progress |
| `test_safety_net.py` | `tests/safety.rs`, `tests/exec_tools.rs` | 17 of 17 | — (five-ring docstring check became the module doc) |
| `test_crawl_guard.py` | `tests/exec_tools.rs` | 8 of 8 | — |
| `test_output_cap.py` | `tests/guards.rs`, `tests/agent_loop.rs` | 4 of 5 | `NUM_PREDICT` env override at import time (Boubou reads it in `RunParams::from_env`; the default is asserted) |
| `test_v211_backends.py`, cloud cases of `test_offline_mode.py` / `test_v203_fixes.py` | `tests/openai_backend.rs`, `tests/ollama_proc.rs` | all | — (plus loopback round trips against a one-shot TCP server and scripted 4×500 restart runs) |
| `test_v191_universal.py::TestBilingualSTT` | `boubou-voice/tests/voice.rs` | all | — (English model only; the real transcription test runs only when the model is installed and the feature is on) |

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
stdio server (`tests/fixtures/fake_mcp.py`, needs `python3`, skipped when absent).
`boubou-general/tests/api.rs` drives a real loopback server with a scripted runner: status shape,
prefs round-trip and `prefver` migration, session rows / detail / select / newchat, plan,
chat → SSE (`note`, `tok`, `ask` with `ask_id`, `exit`), 409 on busy and on a stale ask id,
stop, offline and cloud config writes. `tests/parallel_sandbox.rs`, `parallel_config.rs`,
`parallel_probe.rs`, `parallel_trigger.rs` and `parallel_dispatch.rs` port the five
`test_parallel_*.py` files (the Windows-path cases included); `tests/child_mode.rs` ports the
child-mode cases of `test_integration_0901.py` (finish-gate exemption, Q&A stays task, receipt
isolation, hook off-by-default / kill switch / idempotence, a dispatch through the hook with a
fake spawner that marks the todo done, and a scripted loop ending in `Error::ChildTerminated`
with the checkpoint saved). `tests/openai_backend.rs` and `tests/ollama_proc.rs` cover the
engine layer (config normalisation, message mapping, payload shapes, SSE assembly in 7-byte
chunks, loopback captures, engine selection, think-probe gating, restart ladder);
`boubou-cli/tests/cli.rs` is the headless `main()` suite; `boubou-voice/tests/voice.rs` covers
WAV decode/encode/resample, model-dir resolution, the silence gate and the Ollama transcript
post-processing. 296 tests in the default build, plus 28 in the `voice` feature build (voice,
web, cli; the real transcription test runs when the model is present).

Counts are test functions, not parametrised cases. Several Python tests were merged where they
shared a fixture; the assertions are all present.

## Web UI contract (`webui/server.py`)

GET: `/api/status`, `/api/sessions`, `/api/session?name=`, `/api/plan`, `/api/events` (SSE),
`/api/prefs`. POST: `/api/chat`, `/api/stop`, `/api/ask`, `/api/newchat`, `/api/offline`,
`/api/cloud`, `/api/session`, `/api/prefs`. Implemented in `boubou-general`, byte-for-byte the
shapes Mingbird's page expects. `boubou serve --port 8765` for a browser; `boubou-app` for a
window. Differences from Mingbird: `dispatch` events carry every tool call (`kind` = tool name,
`name` = first 80 chars of the arguments) rather than sub-agent dispatches; `log` events are
emitted but the page ignores them, as it did; a non-empty `finish` summary is pushed as answer
text (`tok`) so a tool-only run still shows a reply (Mingbird printed it as an ignored log line).

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
5. ~~Tauri shell: `/api/*` surface~~ Done 2026-10-27 (`boubou serve` + `boubou-app` window; 159 tests green). Voice input still open.
6. ~~Parallel dispatch.~~ Done 2026-10-28 (247 tests green; `boubou plan` live; one real child run under `AGENT_CHILD_SANDBOX=1` against gemma4:12b).
7. ~~Child sandbox (ring 0) when sub-agents arrive.~~ Done with 6 (same commit).
8. ~~Voice input, `restart_ollama`, cloud / OpenAI-compatible engines, `_model_can_think`, CLI tests.~~ Done 2026-10-28 (296 + 28 tests green; `boubou voice` and `/api/voice` transcribed the zh model's sample WAVs through sherpa-onnx; `boubou doctor` shows the engine and a per-model thinking column).
