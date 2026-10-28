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
| `agent_loop` (empty-turn ladder, hard reset, six finish gates, dup/streak/research/todo anti-loop, failure ladder, budget pacing, stale-plan nudge, Q&A wrap-up), `_FAIL_MARKERS`, `_ws_note` | `boubou-core/src/agent/run.rs` | ported, tested against the scripted-loop suite and a live gemma4:12b run. `_maybe_parallel_dispatch` and child mode (`_child_sandbox`, `_CHILD_SYSTEM`, gate exemptions, exit 53) are in. `restart_ollama` on 4× 500 is in through `ChatBackend::restart_engine` (max 3 per run; an OpenAI-compatible engine returns `None` and the loop keeps the compaction retry) |
| `call_chat` (streaming, `think` 400 retry, tri-state options) | `boubou-core/src/agent/chat.rs` | ported as the `ChatBackend` trait + `OllamaBackend` (owns an `OllamaProc` for restarts); `DynBackend` boxes the engine chosen by `Agent::standard(params, &cfg)` |
| `_call_cloud`, `_call_openai_local`, `_cloud_to_openai_messages`, `_openai_normalize_message`, `_split_inline_thinking`, `_InlineThinkSplitter`, `probe_openai_backend` | `boubou-core/src/agent/openai.rs` | ported, tested (`OpenAiBackend` with `Flavor::Cloud` non-streaming / `Flavor::LocalEngine` SSE; tool-call deltas assembled by `index`; torn `<think>` tags held back; `probe`/`list_models` on `GET /models`) |
| `appconfig.cloud_provider` / `local_backend` / `backend_openai` / `engine_display_name` | `boubou-core/src/config.rs` | ported, tested (cloud off in offline mode; `openai` without `base_url` → Ollama) |
| `ensure_ollama`, `restart_ollama`, `_find_ollama_exe` | `boubou-core/src/ollama_proc.rs` | ported, tested (`OllamaProc`: kill `ollama serve` via taskkill/pkill, wait dead, respawn detached with `config.ollama_env`, wait for `/api/tags`). Not ported: Windows `_ollama_app_models_dir` sqlite lookup |
| `_model_can_think` (`/api/show` capabilities, per-model cache) | `boubou-core/src/agent/chat.rs` | ported, tested; `None` on an OpenAI engine or probe failure; `boubou doctor` prints it per model |
| `_active_tools`, `_disabled_tools`, `route_categories`, `tools_for_categories`, `_chat_tool_defs`, `_filter_offline_tools`, `_is_mcp_producing` | `boubou-core/src/agent/face.rs` | ported (`ToolFace`); MCP flattened tool defs join when the client lands |
| `compact_history` L1/L2/L3, `_estimate_messages_tokens`, `_workdir_files_text`, `_dedupe_trailing_assistant`, `sanitize_ckpt`, `_similar`, `_attach_pending_images` | `boubou-core/src/agent/history.rs` | ported, tested; `_similar` is a difflib-style matching-blocks ratio without the autojunk heuristic |
| `.agent_state.json`, `<s>.json`, `<s>.transcript.jsonl`, `<s>.meta.json`, `<s>.todo.json`, `_transcript_flush` cursor rules | `boubou-core/src/agent/session.rs` | ported, tested; verified cross-readable by Mingbird's `sanitize_ckpt`/`load_session` |
| `SYSTEM`, `CHAT_SYSTEM`, `system_prompt()`, `platform_profile.prompt_block` | `boubou-core/src/agent/prompt.rs` | ported; all prompt text is Boubou's own English (see "Language" below) |
| `_run_test_guard`, `_pytest_hint`, `_test_env_broken` | `boubou-core/src/agent/testguard.rs` | ported (runs `python -m pytest -q` in the workdir when `test_*.py` exist) |
| `main()`: `--session/--new/--append/--time-budget`, checkpoint/session resume, `--chat` | `boubou-cli/src/main.rs` | ported; `boubou-cli/tests/cli.rs` runs the binary headlessly (help, tools, skills, plan, tool, run with a missing task file, voice, doctor). `ensure_engine` runs before a task: probe an OpenAI engine (warn) or `OllamaProc::ensure` |
| `platform_profile.py` | `boubou-core/src/agent/prompt.rs` (`Platform`, `platform_block`) | ported |
| `parallel_config.py` (`DEFAULT_PARALLEL`, env overrides, `hard_disabled`, `current_depth`) | `boubou-core/src/parallel/config.rs` | ported, tested (`ParallelConfig`, typed; invalid values keep the current value; rule lists are data) |
| `parallel_safety.py` (`ChildSandbox`, path boundary, dangerous-command rules, audit JSONL, fuse) | `boubou-core/src/parallel/sandbox.rs` | ported, tested. Windows-style paths (`..\x`, `C:\…`, `\\wsl$`) are judged on every platform so the Python cases hold on macOS/Linux; POSIX adds `/etc/`, `chmod -R`, `launchctl`/`systemctl` rules |
| `parallel_probe.py` (`ResourceProbe`, `estimate_child_cost`, `plan_capacity`, `format_capacity_table`) | `boubou-core/src/parallel/probe.rs` | ported, tested with injected readers; `sysinfo` for RAM, `/api/ps` for loaded models; `boubou plan` prints the table |
| `parallel_todo.py` (`JsonTodoProvider`, `classify_task`, `safety_precheck`, `should_dispatch`) | `boubou-core/src/parallel/trigger.rs` | ported, tested; reason strings verbatim |
| `parallel_dispatch.py` (`ParallelDispatcher`, `ResultIntegrator`, `summary_for_parent`, `@@DISPATCH@@`) | `boubou-core/src/parallel/dispatch.rs` | ported, tested with a scripted `Spawner`; children are `boubou run <model> task.md <dir> --session disp_<run>_NN --new` in their own process group; a worker pool of `max_parallel` scoped threads |
| `_maybe_parallel_dispatch`, `install_child_mode`, `child_gate`, `_CHILD_SYSTEM` | `boubou-core/src/parallel/hook.rs`, `exec.rs` (`Executor::install_child_mode`, ring-0 check first in `prepare`), `agent/run.rs` (`RunParams::child_mode`, `AgentEvent::Dispatch`, `Agent::parallel`) | ported, tested. The library never calls `exit(53)`: the loop returns `Error::ChildTerminated` and the CLI maps it to exit 53 |
| `run_python` (bundled interpreter) | — | **dropped** (decision 2026-10-27): no Python runtime in Boubou. The `office_word`, `office_excel` and `image_batch` skills went with it; `doc_digest` and `file_organizer` still mention `run_python` and need rewording. Revisit later. |
| `voice_input.py` (sherpa-onnx, VAD recorder, `transcribe_ollama`, `pick_stt_model`) | `boubou-voice` crate, `boubou-web` `/api/voice` + mic button, `boubou voice` | ported behind cargo feature `voice` (`sherpa-onnx = "=2.0.1"` static; `sherpa-rs` only wraps offline recognizers). Default builds are native-free and report `built_without_voice_feature`. Same model dirs as Mingbird under `<install>/stt/`, `~/.ollama_agent/stt/` or `BOUBOU_STT_DIR`; no installer/downloader yet |
| `agent_gui.py`, `ui_v4.py`, `ui_theme.py` (Tk) | dropped | replaced by Tauri shell |
| `webui/server.py` (`/api/*`, SSE, `@@ASK@@` approval channel, `gui_prefs.json`, session rows, `kill_tree`) | `boubou-web/src/{api,state,runner,events,prefs}.rs` | ported, tested. axum on 127.0.0.1; the agent runs in-process on a thread, `AgentEvent`s map to the same SSE types (`tok`/`think`/`ask`/`dispatch`/`ctx`/`note`/`exit`) plus `tool_done` (tool result head, blocked flag) and a compaction `note`; `dispatch` carries the full `args`; `WebConfirmer` replaces the stdin round-trip (only the current `ask_id` is accepted); stop = a shared flag the loop and the Ollama stream both check (no process to kill). Cloud / OpenAI-compatible settings now select the engine through `Agent::standard`; `/api/status` reports `engine` and `voice`; `POST /api/voice` transcribes an uploaded WAV |
| `webui/static/index.html` | `boubou-web/static/index.html` | Redesigned (post-v0.1.0): rail / conversation / inspector layout, run blocks built from `dispatch` + `tool_done` events, inline permission cards, dark-first palette from the Boubou mark; same `/api/*` contract plus `/logo.png`. `include_str!` into the binary; `BOUBOU_WEB_INDEX=<path>` serves the page from disk for UI work without a rebuild |
| Tk GUI (`agent_gui.py`) | `boubou-app/src/main.rs` | Tauri 2 shell: starts the web server on a free loopback port and opens a window on it. Voice comes from the page's mic button; `Info.plist` carries `NSMicrophoneUsageDescription` but is not wired into the bundle config yet |
| `mcp_utils_server.py` | — | keep as a Python example, or rewrite as a tiny Rust MCP server |
| `tests/` (33 files, 556 test functions) | `boubou-core/tests/` | in progress, see below |

## Test port status

| Python file | Rust file | Ported | Still deferred |
|---|---|---|---|
| `test_guards.py` | `tests/guards.rs`, `tests/time_budget.rs`, `tests/safety.rs`, `tests/agent_loop.rs` | 88 of 96 | docstring checks (the `main()` CLI, `_model_can_think` and `call_chat` payload cases live in `boubou-cli/tests/cli.rs` and `tests/openai_backend.rs`) |
| `test_todo.py` | `tests/todo.rs`, `tests/guards.rs`, `tests/history.rs` | 26 of 27 | `test_mcp_tool_counts` folded into has_task_progress |
| `test_safety_net.py` | `tests/safety.rs`, `tests/exec_tools.rs` | 17 of 17 | — (five-ring docstring check became the module doc) |
| `test_crawl_guard.py` | `tests/exec_tools.rs` | 8 of 8 | — |
| `test_output_cap.py` | `tests/guards.rs`, `tests/agent_loop.rs` | 4 of 5 | `NUM_PREDICT` env override at import time (Boubou reads it in `RunParams::from_env`; the default is asserted) |
| `test_v211_backends.py`, cloud cases of `test_offline_mode.py` / `test_v203_fixes.py` | `tests/openai_backend.rs`, `tests/ollama_proc.rs` | all | — (plus loopback round trips against a one-shot TCP server and scripted 4×500 restart runs) |
| `test_v191_universal.py::TestBilingualSTT` | `boubou-voice/tests/voice.rs` | all | — (the real zh transcription test runs only when the model is installed and the feature is on) |

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
`boubou-web/tests/api.rs` drives a real loopback server with a scripted runner: status shape,
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
web, cli; the real zh transcription test runs when the model is present).

Counts are test functions, not parametrised cases. Several Python tests were merged where they
shared a fixture; the assertions are all present.

## Web UI contract (`webui/server.py`)

GET: `/api/status`, `/api/sessions`, `/api/session?name=`, `/api/plan`, `/api/events` (SSE),
`/api/prefs`. POST: `/api/chat`, `/api/stop`, `/api/ask`, `/api/newchat`, `/api/offline`,
`/api/cloud`, `/api/session`, `/api/prefs`. Implemented in `boubou-web`, byte-for-byte the
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

## Parallel dispatch as shipped (step 6, 2026-10-28)

Off by default, like Mingbird: `config.json` → `"parallel": {"enabled": true, "child_model":
"<tag>"}` turns it on; `AGENT_PARALLEL=0` is the kill switch; `AGENT_PARALLEL_MODEL/CTX/
MIN_ITEMS/MIN_DISPATCH/MAX/TIMEOUT/DEPTH` override single fields. The hook runs once before the
loop (never for Q&A, children, or a resumed history that already has a tool message or the
`[parallel-dispatch]` receipt). Children are the same binary with `AGENT_CHILD_SANDBOX=1`,
`AGENT_PARALLEL=0`, `MINGBIRD_DEPTH+1`; the parent's time budget is not inherited. In a child
the Executor installs `ChildSandbox` as ring 0 (allow list `read_file list_dir create_file
todo finish`, writes only inside the child dir, sensitive paths and severe commands refused,
`audit.log` JSONL), the confirmation channel is a headless deny, the prompt is `CHILD_SYSTEM`
with no platform block, gates 4–6 and the wrap-up call are skipped, and a severe refusal ends the
run with exit 53. The dispatcher judges each child on exit code + `TASK COMPLETE` in `agent.log`
+ artefacts matching its claims + a clean audit, copies artefacts back without overwriting,
marks todo items done and appends the receipt message. Known gap: a parent run started from the
web UI dispatches too, but the page only shows the `dispatch` chips; there is no cancel for an
in-flight batch except the run's stop flag (checked between children).

## Engines and voice as shipped (step 7, 2026-10-28)

Engines: `Agent::standard(params, &cfg)` returns `Agent<DynBackend, Executor>` and picks, in
Mingbird's order: a cloud provider (`cloud.enabled` + `base_url` + `model`, off in offline mode)
→ `OpenAiBackend::cloud` (non-streaming `/chat/completions`, Bearer always,
`enable_thinking: false`, 900 s timeout); `local_backend = "openai"` with a `base_url` →
`OpenAiBackend::local` (SSE streaming, Bearer only with a key, inline `<think>` splitter that
holds back torn tags, tool-call deltas assembled by `index`); otherwise `OllamaBackend` with an
`OllamaProc`. Messages map through `to_openai_messages` (tool results paired FIFO to the
preceding `tool_call_id`s; unparsable arguments become `{"_raw": …}`). The 4×500 ladder calls
`ChatBackend::restart_engine`: Ollama kills `ollama serve`, waits for the API to die, respawns
it detached with `config.ollama_env` and waits up to 40 s; OpenAI engines return `None` and the
loop logs that the engine cannot be restarted by this program and keeps the compaction retry.
At most three restarts per run. The CLI runs `ensure_engine` before a task (probe `GET /models`
for an OpenAI engine and warn; `OllamaProc::ensure(25 s)` otherwise) and prints `[engine] …`.
`model_can_think` probes `/api/show` capabilities with a per-process cache and returns `None`
on an OpenAI engine. `AGENT_SLEEP_SCALE` scales the loop's recovery sleeps (tests use 0).
`Agent.engine` is a display string (`Ollama · host`, `OpenAI-compatible · base`, `cloud · model (base)`).
Only loopback capture tests exercised the OpenAI path; no LM Studio / llama-server was available.

Voice: crate `boubou-voice`, native engine behind feature `sherpa` (`sherpa-onnx = "=2.0.1"`,
prebuilt static libs fetched at build time, ~12 s extra); `boubou-cli` and `boubou-web` expose it
as feature `voice`. Models are Mingbird's streaming zipformers (zh 14M, en 20M; int8 weights +
`tokens.txt`) looked up as `stt/<model dir>` under `BOUBOU_STT_DIR`, the install directory, then
`~/.ollama_agent` (a base may also be the model directory itself). Pipeline: WAV of any PCM/float
layout → mono → 16 kHz → silence gate (rms < 0.002 → `no_voice`) → sherpa for the requested
language, then zh → Ollama audio model (`pick_stt_model`, whisper first; `images` base64, `think:
false`, marker stripping, 500-char cap) → `Unavailable(no_stt_model_installed |
built_without_voice_feature)`. Surface: `boubou voice <wav> [--lang zh|en] [--model]`,
`POST /api/voice?lang=&model=` (400 bad audio, 422 no voice, 503 unavailable, 500 failed, 32 MB
body limit), `/api/status.voice {local, fallback, langs, lang, feature, hint}`, pref `stt_lang`,
and the page's mic button (hidden unless the status says voice is available; getUserMedia → 16 kHz
PCM, RMS VAD 0.008, 1.2 s silence auto-stop, 60 s cap, same as Mingbird's recorder). Live: the zh
model's `test_wavs/0.wav` (5.6 s) transcribed in 1.4 s through the CLI and `1.wav` through
`/api/voice` on a running `boubou serve --features voice`.

## Loop behaviour beyond Mingbird (2026-10-28, from the LRAB WF-08 post-mortem)

Two additions in `agent/run.rs` that Mingbird does not have; both came out of Boubou's WF-08
run (37 consecutive `create_file` calls without `path`), and both are covered by scripted-loop
tests in `tests/agent_loop.rs`:

- **Missing-argument ladder.** gemma-class models drop a required argument about one call in
  eight and then copy their own failed call from the history. From the second consecutive
  missing-argument error for the same tool, the loop removes the earlier failed pairs (the
  one-call assistant turn, its `is missing argument` tool error and the loop's own note) so one failed
  example is all the model sees, and prescribes a different call shape (short `create_file`
  with the key first, then `append_file`), with the plan's still-missing files as a hint.
  Four notes per run at most; a complete call resets the streak. The generic failure ladder
  (the generic "check the real state with list_dir / read_file first" hint) no longer fires for this class.
- **Truncated tool call.** When `done_reason=length` and a built-in call lacks required
  arguments, Ollama has returned the leading arguments of a call cut off by `num_predict`
  (`content` before `path`). The call is not executed; the tool error names the cap and the
  missing keys and the chunking advice is injected (three times at most) instead of the
  generic "retry with the same content". A truncated response whose call is complete runs
  normally.

### Text-only turns (2026-10-28, from a `boubou chat` transcript with gemma4)

A plain question ("Are you capable of software development ?", 41 characters, no question
word) was classified as a task, so every text reply drew the generic continue note; the model
answered with three alternating sentences and the repeat detector, which compared only with
the previous turn, never fired. Four changes, all tested in `tests/agent_loop.rs` and
`tests/guards.rs`:

- `qa::ends_with_question`: a trailing `?` (after trailing quotes/brackets) is a Q&A cue when
  no task verb matched; "are you / can you / do you / will you / would you / is there" joined
  the question openers. A `?` in the middle still does not count.
- The repeat detector compares a new text with the last four text outputs (`TEXT_WINDOW`),
  so A/B/C cycles close the turn as a dead loop.
- Task-mode text-only turns are capped: the second draws a sharper "tool call or finish, not
  more text" note (`TALK_SHARP_NOTE`), the fourth ends the turn with the earlier text as
  the answer (`TALK_FORCE_END`). A tool call resets the streak. Mingbird has no such cap.
- `RunParams::chat_mode` (set by `boubou chat`): a text reply ends the turn even when the
  message reads like a task. The web page keeps the detector-only behaviour because it has
  no separate chat/task entry point. The CLI also prints thinking dimmed (ANSI) so it reads
  apart from the answer.

## Language (2026-10-28)

Boubou is UK English from top to bottom; nothing of Mingbird's Chinese text survives in the
source, prompts, tool and harness messages, web page, skills, docs or tests. Decisions:

- Prompts, harness notes, tool errors and log lines are Boubou's own English. The functional
  markers other code matches on moved with them: `[safety gate`, `[safety net`,
  `[overwrite guard`, `[child safety gate` (`FAIL_MARKERS`), `is missing argument '<key>'`
  (`missing_arg_key`), `⚠️ [missing argument]`, `[Earlier context summary]`
  (`qa::CONTEXT_SUMMARY_MARK`), `[blocked:`, `Hint:`, `Your answer is already sufficient`,
  `Continue the previous conversation` (`HARNESS_NOTE_PREFIXES`), `outside the working
  directory` / `out of bounds` (the path-refusal hint), `timed out` (MCP).
- Tool categories are `files`, `code`, `web`, `memory`, `mcp`. `mcp::normalise_category`
  accepts the English names case-insensitively and Mingbird's four Chinese tags (kept as
  `\u{..}` escapes in the source, never as characters), so an existing `mcp.json` and a
  manifest cache written by Mingbird still route.
- Deliberate domain bias (post-v0.2.0): skills accept `triggers:` / `autoload:` frontmatter and
  matching skills are prefilled into the task prompt (`SkillSet::for_task`, cap 3, logged as
  `[skills in effect: …]`); `<workdir>/SYSTEM.md` is appended to the task and chat prompts, or
  replaces them when it starts with `<!-- boubou: replace -->` (`prompt::project_system`). The
  explicit override file still wins the base. Resumed sessions keep their original prompt.
- Routing (`face::route_categories`) is domain-neutral since v0.2.0+: the code keyword list is
  programming vocabulary only (no `fix`/`test`/`error`/`calculate`/`run`), and the built-in
  `files` and `code` tools are offered on every task; routing only adds `web`/`memory`/`mcp`
  built-ins and decides which MCP servers' tools are exposed. The hollow-finish gate asks for
  "a deliverable" (file or answer text), not for a file.
- Routing keywords (`face::CAT_KEYWORDS`), Q&A hints (`guards/qa.rs`), parallel veto/simple
  patterns (`parallel/config.rs`) and complexity words are English-only lists matched on the
  lower-cased text. A bare `?` is not a Q&A cue (Mingbird's `?` entry never matched either:
  it went through the word-boundary regex).
- The time-budget parser understands digits and English number words with `minute(s)/min(s)`,
  `hour(s)/hr(s)/h`, `half an hour`, `an hour`, `an hour and a half`, `N and a half hours`;
  Chinese numerals, measure-word forms and full-width digits are gone.
- The web page is English-only (`<html lang="en">`, no zh table, no language toggle).
  `AGENT_LANG` / `lang_en` are gone. The CLI, Tauri shell and `Info.plist` are English only.
- Speech-to-text defaults to `en` (`stt_lang`, `--lang`, `/api/voice?lang=`); the Chinese
  zipformer remains a supported model and is tried as a fallback when the English one is
  not installed. The Ollama transcription prompt is English.
- Compaction summaries are requested in English (`history.rs`).
- Spelling: UK in prose, strings, comments and docs (behaviour, artefact, organise,
  normalise, cancelled); identifiers and external API names are unchanged (`artifacts`
  module, serde `Serialize`, `Flavor`). Classifier lists carry both spellings where users
  may type either (`summarise`/`summarize`).

## Step 8 brief (handoff notes, 2026-10-28)

- Voice model installer: `boubou voice --install <lang>` downloading the k2-fsa `tar.bz2`,
  keeping only `*int8.onnx` + `tokens.txt` under `~/.ollama_agent/stt/`; today the hint text
  only tells the user where to put the files.
- Tauri bundling: wire `crates/boubou-app/Info.plist` into `tauri.conf.json`, bundle `stt/` as a
  resource, build `boubou-app` with the `voice` feature, test the mic permission prompt.
- Live-check an OpenAI-compatible engine (LM Studio / llama-server) and a real cloud provider;
  only loopback capture tests cover `OpenAiBackend` so far.
- Windows: `_ollama_app_models_dir` (sqlite lookup of the Ollama app's model dir) is not ported.
- `doc_digest` / `file_organizer` skills still mention `run_python`.
- LRAB head-to-head: `bench/lrab_h2h.py` (Boubou side; Mingbird is third-party and is never
  modified). It runs Mingbird's own `run_bench.py --agent mingbird` for one arm, drives `boubou run`
  itself for the other with the same isolated env (`~/.mingbird_bench` home, `AGENT_CTX=32768`,
  `AGENT_SLIM_OUTPUT=0`, `AGENT_UNATTENDED=1`, no `--new` so kill-resume resumes `.agent_state.json`)
  and scores both with Mingbird's `score_task` imported read-only. Needs `requests` +
  `beautifulsoup4` for the mingbird arm: `uv run --with requests --with beautifulsoup4 python
  bench/lrab_h2h.py --model gemma4:e2b`. WF-01 smoke on gemma4:e2b: Boubou 1.0 in 131 s.

Search backends: Boubou defaults to Brave → DuckDuckGo HTML → Bing → Baidu. Changed on
2026-10-28 from Bing-first after Bing answered the query `jevai.net` with ten well-formed but
unrelated results (Microsoft Forms, Gmail) and the model reported that as fact; DuckDuckGo, the
fallback, was silently answering with a bot-challenge page (HTTP 202) that parsed as "no
results". Four guards now sit on the scraping, all in `web.rs` and covered by loopback tests in
`tests/web_memory.rs`:

- Relevance floor: a result whose title, URL and snippet share no word with the query (stop
  words and `www/com/net` removed; five-letter prefixes count; Chinese tokens match as substrings)
  is dropped. A backend whose results all fail the floor is skipped and not cached, and the
  loop moves to the next backend.
- Bot challenge: DuckDuckGo's `anomaly-modal` page is reported in the tool output ("served a
  bot challenge instead of results") and in the log instead of counting as empty.
- Site-shaped query (`jevai.net`, `typesafe.ai/blog`): the site itself is fetched first and
  leads the output as "(fetched directly)", whatever the engines return.
- When every backend fails the floor the tool returns `[web_search: no relevant results for
  "…" (per-backend reasons). Rephrase the query …]`; transport failures keep the
  `[web_search error: …]` form.

Mingbird's `cn.bing.com` host is therefore not shared in the search cache; `web_fetch` entries
still are. `AGENT_SEARCH_BACKENDS` overrides the list. Backlog (beads): a SearXNG backend
(`format=json`, no HTML scraping, self-hosted) and a Tavily backend (hosted API, key in config).

