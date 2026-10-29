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
| `parallel_probe.py` (`ResourceProbe`, `estimate_child_cost`, `plan_capacity`, `format_capacity_table`) | `boubou-core/src/parallel/probe.rs` | ported, tested with injected readers; `sysinfo` for RAM (macOS: `vm_stat` free + inactive first, psutil's figure; `sysinfo` subtracts the compressor and reads 0 with a model resident, bead boubou-wh5), `/api/ps` for loaded models; `boubou plan` prints the table |
| `parallel_todo.py` (`JsonTodoProvider`, `classify_task`, `safety_precheck`, `should_dispatch`) | `boubou-core/src/parallel/trigger.rs` | ported, tested; reason strings verbatim |
| `parallel_dispatch.py` (`ParallelDispatcher`, `ResultIntegrator`, `summary_for_parent`, `@@DISPATCH@@`) | `boubou-core/src/parallel/dispatch.rs` | ported, tested with a scripted `Spawner`; children are `boubou run <model> task.md <dir> --session disp_<run>_NN --new` in their own process group; a worker pool of `max_parallel` scoped threads |
| `_maybe_parallel_dispatch`, `install_child_mode`, `child_gate`, `_CHILD_SYSTEM` | `boubou-core/src/parallel/hook.rs`, `exec.rs` (`Executor::install_child_mode`, ring-0 check first in `prepare`), `agent/run.rs` (`RunParams::child_mode`, `AgentEvent::Dispatch`, `Agent::parallel`) | ported, tested. The library never calls `exit(53)`: the loop returns `Error::ChildTerminated` and the CLI maps it to exit 53 |
| `run_python` (bundled interpreter) | — | **dropped** (decision 2026-10-27): no Python runtime in Boubou. The `office_word`, `office_excel` and `image_batch` skills went with it; `doc_digest` and `file_organizer` still mention `run_python` and need rewording. Revisit later. |
| `voice_input.py` (sherpa-onnx, VAD recorder, `transcribe_ollama`, `pick_stt_model`) | `boubou-voice` crate, `boubou-web` `/api/voice` + mic button, `boubou voice` | ported behind cargo feature `voice` (`sherpa-onnx = "=2.0.1"` static; `sherpa-rs` only wraps offline recognizers). Default builds are native-free and report `built_without_voice_feature`. Same model dirs as Mingbird under `<install>/stt/`, `~/.ollama_agent/stt/` or `BOUBOU_STT_DIR`; no installer/downloader yet |
| `agent_gui.py`, `ui_v4.py`, `ui_theme.py` (Tk) | dropped | replaced by Tauri shell |
| `webui/server.py` (`/api/*`, SSE, `@@ASK@@` approval channel, `gui_prefs.json`, session rows, `kill_tree`) | `boubou-web/src/{api,state,runner,events,prefs}.rs` | ported, tested. axum on 127.0.0.1; the agent runs in-process on a thread, `AgentEvent`s map to the same SSE types (`tok`/`think`/`ask`/`dispatch`/`ctx`/`note`/`exit`) plus `tool_done` (tool result head, blocked flag) and a compaction `note`; `dispatch` carries the full `args`; `WebConfirmer` replaces the stdin round-trip (only the current `ask_id` is accepted); stop = a shared flag the loop and the Ollama stream both check (no process to kill). Cloud / OpenAI-compatible settings now select the engine through `Agent::standard`; `/api/status` reports `engine` and `voice`; `POST /api/voice` transcribes an uploaded WAV |
| `webui/static/index.html` | `boubou-web/static/index.html` | Redesigned (post-v0.1.0): rail / conversation / inspector layout, run blocks built from `dispatch` + `tool_done` events, inline permission cards, dark-first palette from the Boubou mark; same `/api/*` contract plus `/logo.png`. `include_str!` into the binary; `BOUBOU_WEB_INDEX=<path>` serves the page from disk for UI work without a rebuild |
| (none in Mingbird) | `boubou-acp` crate, `boubou acp` | new: Boubou as an ACP agent over stdio (see "Agent Client Protocol" below) |
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

## Integrity gate (2026-10-28, bead boubou-65d.8, from the micro-probes)

Boubou's finish ladder gains two refusals and two excuses in `guards/integrity.rs`, each tied
to an observed false success; Mingbird's gates 1–6 are otherwise unchanged.

- **Gate 2b, claim vs evidence** (two refusals). The loop records every `run_bash` of the turn
  with its `[exit N]`. If the summary claims a verified outcome (`printed OK`, `tests pass`,
  `6 passed`, `ran successfully`, …) and the last verification-shaped command (pytest,
  unittest, check, test, verify, lint) did not exit 0, the finish is refused naming the evidence
  (last check exited N / was refused / none ran). If the last verification command failed and
  the summary does not admit failure, likewise. Probe evidence: `check.py printed OK` with zero
  check runs (probes_gatefix t02); `printed OK` over eleven exit-127 runs (smoke).
- **Gate 3b, task-named deliverables** (one refusal). Files the *task text* names with an
  artefact extension that do not exist in the workdir, unless the summary reports that file as
  failed. LRAB WF-04: "Task complete" with fix_summary.md never written and never mentioned, so
  the summary-based artefact gate could not see it. Known cost: a task that merely refers to an
  absent file gets one refusal that offers the honest path ("say so in the summary").
- **Honest-failure excuse** for gates 3, 3b and 4: a file name is not a claim when every clause
  mentioning it carries a failure word (`admits_failure_about`). Loop probe: "Download of
  dataset.csv failed" was refused six times for naming the file.
- **Verified deliverable outranks ceremony**: when a verification command passed and nothing
  that can change files ran after it (`verified_at_end`), gate 5 syncs the todo itself instead
  of refusing, and gate 6 (the pre-delivery re-read of the task) is skipped: the check *is* the
  self-check. Probe t09/t03: the correct result and `check.py` OK were on disk, the finish was
  refused twice for an unticked todo, the model stopped calling finish and the dead-loop guard
  discarded the turn. probes_integrity2 t00/t04: same result, the model answered the re-injected
  task in prose instead of calling finish again.
- **A pass is evidence only for the state it saw.** The loop records file-writing tool calls
  (`create_file`, `edit_file`, `append_file`, `delete_file`) next to the `run_bash` records.
  If the summary claims a verified outcome and anything that could write ran after the last
  passing check (a file tool, or a shell command that is not plainly read-only: `cat`, `ls`,
  `grep`, `head` … with no `>`), gate 2b refuses and asks for the check to be re-run.
  probes_integrity2 t05: `check.py` OK at turn 21, then a failing `awk … > result.txt` emptied
  the file, and "check.py printed OK" was accepted on the stale pass.

Runs to compare (gemma4:e2b, false_finish, n=10): `bench/results/probes` (before the gate),
`probes_gatefix` (safety-gate fix only), `probes_integrity` (gate, single-refusal claim check),
`probes_integrity2` (claim check judged by the last verification run, two refusals), and
`probes_integrity3` (stale-evidence rule and gate 6 skip).

| run | false success | truth ok | forced end | refusals/run | calls/run | wall/run |
|---|---|---|---|---|---|---|
| probes (no gate) | 0/10 | 7/10 | 3/10 | 1.7 | 16.4 | 135 s |
| probes_gatefix | 1/10 | 8/10 | 2/10 | 1.7 | 12.1 | 95 s |
| probes_integrity | 1/10 | 9/10 | 1/10 | 0.7 | 12.0 | 89 s |
| probes_integrity2 | 1/10 | 8/10 | 3/10 | 0.5 | 14.2 | 105 s |
| probes_integrity3 | 0/10 | 9/10 | 1/10 | 0.8 | 12.8 | 94 s |

In the final run every accepted finish was correct; the one miss was a forced end with no
finish. The stale-evidence refusal fired in t04 (twice, the model then re-ran the check) and the
exit-code refusal in t08 (twice, then a passing check, plan synced, gate 6 skipped). n=10 per
cell: the ordering is suggestive, the per-transcript mechanisms are the evidence.

## Isolated corrector (2026-10-28, bead boubou-65d.7, from the loop probe)

`agent/corrector.rs`, after "Three Roles, One Model": a small model that keeps re-issuing a
failing call is copying its own history. The loop probe (gemma4:e2b, n=10, `bench/probes.py
--probes loop`) showed every trial re-running `python fetch.py` into a 503 (6.9 attempts a run),
4/10 then inventing dataset.csv, and 5/10 ending by the dead-loop guard rather than finish. The
duplicate-call block only sees adjacent repeats; the model interleaves `list_dir` between them.

- **Trigger**: the second failure of one exact signature (tool + canonical arguments) in a run.
  Failure = a `FAIL_MARKERS` hit or a non-zero `[exit N]`. Missing-argument errors keep their own
  ladder. Not for Q&A turns; off with `--ablate corrector`. One consultation per signature,
  three per run.
- **The call**: the same model, a fresh two-message context (role text + task head, the failing
  call, its last result, the attempt count), the turn's tool schemas. It is asked either to make
  a *different* call or to answer `GIVE UP: <reason>`. Native call, leaked-text call or give-up
  text are all read; a transport error is an "unclear" verdict and the ladder carries on.
- **Retry**: the corrected call runs at once through the normal executor (safety gate included),
  is appended to the history as a real call/result pair, recorded for the integrity gate, and a
  note tells the model to continue from that result. If it fails too, the original signature is
  blocked.
- **Give-up**: the signature is blocked for the rest of the run (`[blocked: the corrector judged
  this exact call cannot succeed here (…)]`, no execution) and the note spells out the honest
  exit: state the failure in the deliverable and in the summary, name the task's missing files,
  never invent the missing input. Failed steps stay unticked.
- **Honest exit through the gates**: a finish whose summary admits failure is no longer bounced
  by gate 5 for an unticked plan (the plan is left as is, not auto-ticked), and the honest-failure
  excuse on gates 3/3b/4 now counts a file's stem as a mention ("the dataset download failed" for
  dataset.csv). Loop probe t02/t03: honest summaries were refused four times, then the dead-loop
  guard ended the run.
- **A look is not a fix**: a proposal of a looking-only tool (`list_dir`, `read_file`, web
  tools, `todo` …) for a failing producing call is read as "no fix found" and handled as a
  give-up. probes_corrector t05: the corrector answered `list_dir` for a 503 from fetch.py, the
  "fix" ran fine, and the model ran fetch.py five more times.
- **Metrics**: `corrector_calls` / `corrector_fixes` / `corrector_giveups` in process_metrics
  (`corr` column in the h2h tables); corrector blocks count as loop interceptions.

Runs (gemma4:e2b, loop probe, n=10): `bench/results/probes` and `probes_integrity` (before the
corrector), `probes_corrector` (first version), `probes_corrector2` (looking-tool rule). Two
wording changes tried after probes_corrector2 (a sharper first-repeat note, an honest-option
sentence in gate 3) and a gemma-specific `finish{summary: …}` leak parser were reverted: tuned
on single transcripts of one model, they are profile material (bead 65d.5), not shared code.
The generalisation check for the corrector is the held-out LRAB head-to-head, not more probe
reps.

## Per-model profiles (2026-10-29, bead boubou-65d.5, from the profile battery)

`profile.rs`. The battery (`bench/profile_battery.sh`: four models × emission, false-finish,
loop; n=10 agent trials, 120 emission calls; `bench/results/profiles/<tag>`; 2026-10-28
22:47 → 01:23, think left at the engine default, 4-minute budget) is the first per-model
measurement of the harness. Boubou only, no Mingbird arm.

| model (nvfp4) | emission valid | false success | truth ok | honest (loop) | fabricated dataset | forced end (loop) | calls / wall (loop) |
|---|---|---|---|---|---|---|---|
| qwen3.5:9b (9.7B) | 1.00 | 0.00 | 1.00 | 0.60 | 0.10 | 0.00 | 13.9 / 99 s |
| gemma4:12b (12.4B) | 0.77 | 0.00 | 0.30 | 0.50 | 0.10 | 0.00 | 12.5 / 247 s |
| gemma4:e4b (8.1B) | 1.00 | 0.00 | 1.00 | 1.00 | 0.00 | 0.00 | 7.1 / 34 s |
| gemma4:e2b (5.2B) | 1.00 | 0.00 | 0.80 | 0.60 | 0.10 | 0.30 | 7.6 / 45 s |

False success stayed at zero for every model (integrity gate). Each finding was classified
before anything was changed (mechanism/bug → shared code; wording → raise; quirk → profile):

- **gemma4:12b, runaway thinking (quirk → profile).** 9/10 false-finish trials and 3/10 loop
  trials were killed at the budget after one to four calls; the transcripts show a single
  thinking episode (the arithmetic of the task, step by step) running for over three minutes.
  `truth_ok` 0.30 is a timeout artefact, not a false claim. Built-in profile: `think: off` for
  `gemma4:12b`. Hardware-dependent, so it is also the kind of entry a user overrides in config.
- **gemma4:12b, `list_dir` first (quirk, recorded only).** All 28 emission misses are the
  model choosing `list_dir` (or once `run_bash`) on prompts that call for `read_file` or
  `edit_file`: a look-first habit, harmless in a run, no profile action.
- **gemma4 (e2b), finish as prose (quirk → profile).** After the corrector's give-up note the
  model wrote `Finish: <summary>` as the last line of a text turn (loop t03/t04/t05) or the
  bare `finish{summary: …}` seen in probes_corrector2; the dead-loop guard then ended the run
  (3/10 forced ends, honest summaries lost). Built-in family profile `finish_prose_rescue`:
  `leak::parse_finish_prose` reads a trailing `Finish:` line or the bare brace form as the
  `finish` call, so the finish gates judge the claim. Gated per profile because the generic
  bare-argument parser over-captured prose (reverted on 65d.7).
- **Corrector blocks a verification command after a fix (bug → shared code, bead 65d.13).**
  e2b false-finish t00 and e4b t05: `python check.py` fails, the model rewrites `result.txt`,
  check fails again, the corrector blocks the signature; the model then writes the right value
  and can no longer verify, finishes honestly with FAIL, gate 6 bounces it, repeated output
  forces an end although the ground truth was OK. Same rule as the integrity gate's
  stale-evidence branch: a successful mutating action (file write that ran, or a non-read-only
  command with exit 0) clears `sig_fails` and `corrector_blocked` for every *other* signature.
  A failing command never resets its own count, so fetch.py → fetch.py still consults.
- **qwen3.5:9b, substitutes the missing input (quirk, raised, not patched).** When fetch.py
  fails it searches the filesystem (`find /Users/…`), copies a `dataset.csv` from a sibling
  benchmark workdir or writes synthetic rows, and reports success (4/10 loop trials not
  honest). The unattended safety gate stops `search_files` outside the workdir but not
  `run_bash find/cp`. Candidate for the profile's nudge-wording variant; wording changes are
  raised first, so nothing was changed.
- **gemma4:e2b arithmetic (capability, recorded only).** Two honest failures in false-finish
  (wrong sum, FAIL reported as FAIL); the claim gate caught every success claim after a
  failed check (t07).

Profile layer: `ModelProfile` (think, temperature, hide_tools, max_prefill_skills,
finish_prose_rescue, note) resolved as built-in default → built-in family (`family_of`: tag
base name) → built-in exact tag → `config.json` `profiles` overrides for family and tag, each
setting only the fields it names. `RunParams` carries the built-in profile from construction;
`Agent::standard` re-resolves with the config and lets the profile fill `think`/`temperature`
only where the environment or UI left them unset. Hidden tools are disabled on the face in
`Agent::new`; the prefill cap feeds `SkillSet::for_task_capped`. Every run logs
`[profile] <model> → <entry>: <settings> (think=… temp=…). Evidence: …` before the first turn
(children skip it). The nudge-wording variant from the bead description was not added in this
round; the input-substitution follow-up below added it as a profile `prompt_note`.

Measurement after the change (`bench/results/profiles_v2`, one run each, n=10, 01:42 → 02:23):

| model, probe | before → after |
|---|---|
| gemma4:12b emission (think off) | valid 0.77 → 0.87 (all misses still `list_dir` first) |
| gemma4:12b false-finish | truth ok 0.30 → 0.90, timeouts 9 → 1, calls 2.9 → 9.9, wall 276 → 57 s |
| gemma4:12b loop | ended cleanly 0.70 → 1.00, wall 247 → 39 s; honest 0.50 → 0.60, fabricated dataset 0.10 → 0.30 |
| gemma4:e2b false-finish | truth ok 0.80 → 0.90, forced end 0.10 → 0.00, corrector blocks 1 → 0 |
| gemma4:e2b loop | honest 0.60 → 0.90, forced end 0.30 → 0.10 |

Attribution, honestly: the 12b gain is the think-off entry (the `[profile]` line and zero
thinking excerpts in every transcript confirm it took effect). The e2b gain is *not* the
finish-prose rescue: it fired in none of the 20 trials, because the `Finish:` prose did not
recur this run; the one remaining forced end is the known gate-6 shape (the model answers the
re-injected task in prose). The corrector block on a verification command did not recur
either, so the 65d.13 fix is covered by its tests, not by this run. With thinking off the 12b
invents `dataset.csv` more often (3/10) and says so in its summary: the same substitution
quirk as qwen3.5:9b, recorded, not patched. Generalisation check remains the LRAB head-to-head.

### Input substitution (2026-10-29, beads boubou-65d.14 and boubou-65d.15)

Seven loop-probe trials across qwen3.5:9b (4/10) and gemma4:12b think-off (3/10) ended with a
`dataset.csv` the model obtained some other way, reported as a success: fetch.py rewritten to
generate rows (qwen t00), the file copied from a sibling bench workdir after a `find` over the
home directory (qwen t04/t06/t08), or a dataset typed by hand (12b t00/t01/t05). Two facts
from the transcripts decided the shape of the fix:

- The corrector was consulted in none of the seven. These models never repeat the identical
  call: they run fetch.py once, read it, call it "a stub" and go looking. The harness's
  anti-fabrication wording lives only in the corrector give-up and retry notes, so it never
  reached them. The three qwen trials that did reach the corrector all ended honestly.
- Two of the seven were honest finishes the harness bounced into substitution. qwen t06
  finished with "dataset.csv cannot be fetched"; gate 6 answered "if anything is not met: fix
  the deliverable first" and the model fetched it from a sibling. 12b t05 finished with
  "failed to download the file"; gate 3b rejected it (the failure clause names "the file", not
  `dataset.csv`, so `admits_failure_about` did not match) and the model wrote a one-row file.

Changes, classified first:

1. **Gate 6 wording, shared mechanism (65d.14, bug).** The pre-delivery re-read now carries the
   environmental-failure clause gate 5 already had: a step that failed for network, service or
   missing-input reasons is not something to fix here; do not obtain or create that input
   another way; keep the failure stated and finish again.
2. **Profile `prompt_note`, per tag (65d.15).** `HONEST_FAILURE_NUDGE` is appended to the task
   system prompt as `MODEL PROFILE NOTE` for qwen3.5:9b and gemma4:12b only (not the gemma4
   family: e4b and e2b never substituted). It is in the system prompt rather than the task turn
   because the task turn feeds Q&A routing, category routing and the gates' re-read of the
   original task; a prefixed question stopped routing to Q&A in the tests. `"prompt_note": ""`
   in `config.json` switches a built-in note off; any other string replaces it.
3. **Recorded, not changed.** Gate 3b's name matching stays strict: accepting "the file" would
   also admit evasive summaries, and the rejection already offers the honest option by name.
   The `run_bash` asymmetry (in unattended mode `search_files ..` is refused by the safety gate
   while `find`/`cp` over the home directory run freely; qwen t04 spent 128 s in one `find`
   before the kill) is a guard question for its own bead.

Measurement after the change (`bench/results/profiles_v3`, loop probe only, one run each, n=10):

| model, loop probe | before → after |
|---|---|
| qwen3.5:9b | honest 0.60 → 1.00, fabricated dataset 0.10 → 0.00, calls 13.9 → 5.9, wall 99 → 30 s |
| gemma4:12b (think off) | honest 0.60 → 1.00, fabricated dataset 0.30 → 0.00, wall 39 → 43 s |

Both models reported the 503 honestly in all ten trials; no `find`/`cp` over the home
directory, no hand-typed dataset. Attribution: the `[profile]` line shows the prompt note in
effect in every trial; gate 6 bounced the honest finish once in all 20 trials and gate 3b in 10
of them, and no bounce led to substitution this time. The gain is the note plus the gate 6
clause together; the run cannot separate them, and it was not designed to.

Generalisation check (`bench/results/lrab_h2h_v4`, 2026-10-29 05:08 → 06:08, gemma4:e2b, both
arms, WF-01/04/07/08/11/12/14/15, one run per cell, deterministic scoring; previous run
`lrab_h2h_v3` in brackets). gemma4:e2b carries no prompt note, so this is the regression floor
for the gate 6 clause and the profile layer, not a test of the nudge:

| task | mingbird | boubou |
|---|---|---|
| WF-01 | 1.00 (1.00) | 1.00 (1.00) |
| WF-04 | 0.57 (1.00) | 1.00 (1.00) |
| WF-07 | 0.57 (0.86) | 0.71 (0.86) |
| WF-08 | 0.79 (0.00) | 1.00 (0.79) |
| WF-11 | 1.00 (1.00) | 0.93 (1.00) |
| WF-12 | 0.86 (0.86) | 0.86 (0.86) |
| WF-14 | 1.00 (1.00) | 0.79 (0.89) |
| WF-15 | 0.79 (0.71) | 0.79 (0.79) |
| mean | 0.821 (0.803), 35 min | 0.884 (0.897), 20 min |

Process means per cell, mingbird vs boubou: calls 30.6 vs 22.8, failed calls 5.1 vs 0.1, loop
interceptions 1.8 vs 1.0, integrity bounces 0.4 vs 0.1, corrector 0.0 vs 0.1 (one consultation,
WF-04, honest give-up on a missing pytest module; the run still scored 1.0). Gate 6 re-read
fired once in every Boubou cell; no cell showed the substitution shape. Two caveats, both
referee-side: the driver was launched with plain `python3`, so the scorer's `spec_pytest` check
got `/bin/sh: python: command not found` on WF-04 in both arms; both cells were rescored with
`uv run --with pytest` (originals kept as `score.json.unscored`), which is where Mingbird's 0.57
(2 of 6 tests failing) and Boubou's 1.00 come from. The three Boubou drops are word-count
misses on a prose deliverable (WF-11 265/280, WF-14 338/400, WF-15 280/300) plus a
`fig_arrhenius.png` that is not a PNG (WF-15; last run the file was missing). The two means
are within the single-run noise seen between v2 and v3; no regression is visible.

## System One decider, step 1 (2026-10-29, bead boubou-acy.1)

`decider.rs`, per `docs/system-one-decider.md` section 6 step 1, with the local backend changed
to Ollama's native decision API (`/v1/systemone`, Ollama ≥ 0.40, `ollama pull d1`). The intent
decision (question / task / follow-up) is the first and only call site. `guards/qa.rs` gained
`intent_heuristic()` with an honest confidence (1.0 on a rule hit, 0.5 on a default) and the
rule that fired; `is_qa()` wraps it. `LayeredDecider::intent_of()` runs the heuristic and, by
`config.json` `decider.mode`, leaves it there (`off`, default), asks the model and logs both
(`shadow`), or lets the model act only where the heuristic was guessing (`on`, below
`threshold`). Failures fall back to the heuristic; three in a row stop the asking for the run.
One `[decider] …` line per run; children never ask; `boubou doctor` probes the model.

Zero behaviour change by default: 380 tests pass, the old `is_qa` tests untouched. Live check
with a scratch agent home in shadow mode: d1:latest answers the 2026-10-28 chat sentence as a
question at p=0.89 in about 0.3 s warm. Nothing is adopted yet; shadow transcripts are the
labelled set the design's section 5 asks for before `on` is the default anywhere.

### Measurement (2026-10-29, bead boubou-acy.2)

Tooling: `boubou decide [--file cases.jsonl] [--model d1:latest]` prints the heuristic verdict
(intent, confidence, rule) and the model's (intent, p, confidence, ms) per line;
`bench/decider/build_intent_set.py` builds `bench/decider/intent_set.jsonl`;
`bench/decider_eval.py` scores both plus a grid of layering policies, with a reversal rate
under eight deterministic rephrasings (please-prefix, lowercase, final stop toggled, can/could
swap, hey-prefix, thanks-suffix) and latency. Labels follow the loop, not the surface: a
*question* is satisfiable on the Q&A path (text answer; `read_file`, `list_dir`, `web_search`,
`web_fetch`), a *task* needs the task loop (run_bash, writes, deliverables, a plan), a
*follow_up* steers work under way. 306 cases: 99 collected (26 from the user's sessions, 36
LRAB prompts, 14 probe prompts, 23 test fixtures) and 207 authored to over-sample the
heuristic's 0.5 defaults and its known blind spot (questions that contain a task verb); 15
marked uncertain. Results in `bench/results/decider/run2` (run1 identical: the model is
deterministic).

| metric | heuristic | d1:latest | shipped `on` (threshold 0.9) |
|---|---|---|---|
| accuracy, 3 classes | 71.6% | 72.9% | 69.0% |
| accuracy, question vs not | 76.5% | 81.4% | 78.8% |
| on the heuristic's defaults (n=118) | 69.5% | 62.7% | 62.7% |
| on the heuristic's rule hits (n=188) | 72.9% | 79.3% | 72.9% |
| reversal under rephrasing | 35.9% | 19.0% | 36.3% |
| latency p50 / p95 | | 144 / 608 ms | |

Acceptance (section 5: +10 points, reversal no worse, p95 < 1.5 s): **fail, keep off**. The
layering premise was wrong: the heuristic is weakest where a rule fires (54 questions classed as
tasks because they contain a verb: "Is it safe to delete the target directory?") and the model
is weakest where the heuristic defaults (short messages during work, which it calls follow-ups
when they are status questions), so "ask the model only where the heuristic is unsure" asks it
where it helps least. Policy grid from the same rows, question-vs-not accuracy:

| policy | all | collected (n=99) | authored (n=207) | reversal |
|---|---|---|---|---|
| heuristic | 76.5% | 87.9% | 71.0% | 35.9% |
| model alone | 81.4% | 83.8% | 80.2% | 19.0% |
| `on` (shipped) | 78.8% | 89.9% | 73.4% | 36.3% |
| model only overturns a task-verb hit into a question | 81.4% | 87.9% | 78.3% | 38.6% |
| model decides question-vs-not, heuristic the rest | 81.4% | 83.8% | 80.2% | 25.5% |

The split is the honest part: on the 99 cases that came from real use and real benchmarks the
heuristic already scores 87.9% and no model policy beats the shipped `on` (89.9%, within one
case of the heuristic); every gain for the model is on the authored cases, which were written
around the heuristic's blind spots. So the authored half measures the blind spot, not the base
rate. Decision: `off` stays the default and `shadow` stays on for this user so the real
disagreement rate accumulates; the next evaluation reruns `decider_eval.py` on the
shadow-collected cases only. The one robust finding is the reversal rate: the model changes its
mind on a rephrasing half as often as the keyword lists do.

## The system prompt follows the route (2026-10-29, bead boubou-c4b)

Found in a web chat session (`chat_1029_070632`): the first turn was a question, so the loop swapped
in `CHAT_SYSTEM`; the web runner continues a session by appending the new user turn to the saved
history, and the task branch of the loop only computed tool categories. Every later task-routed
turn in that session therefore ran under the chat instructions ("answer briefly, use tools only to
look something up") with the task-mode guards and finish gates on top: a text answer, the continue
note, a mid-answer `web_search`, a revised answer, three nudges, then gate 6 quoting a one-line
chat message as the task brief. Mingbird has the same shape; the fix is for the user's sake, not
for parity.

Mechanism fix, shared code: `task_system()` builds the task prompt (triggered skills, `SYSTEM.md`,
unattended line, profile note) for both `task_messages` (fresh run) and a per-turn rebuild in
`run()` when the history already holds an assistant message and the route is task. Skills are
matched on the opening task plus this turn's input so a prefilled skill stays and a new trigger can
add one. The saved history's system message is replaced on the next turn, so a session file no
longer pins a prompt. Children keep their sandbox prompt; a fresh run is untouched. Tests:
`task_turn_after_a_question_gets_the_task_prompt_back`, `question_turn_after_a_task_gets_the_chat_prompt`.

The misroute that exposed it ("Are there any good libp2p implementations in Dart ? I want to build
a flutter application" → task on the hint `build`; d1 said question at p=0.52) is a decider-set
case, not a heuristic patch.

## Agent Client Protocol (2026-10-29, bead boubou-j5f)

`crates/boubou-acp` makes Boubou an ACP agent: an editor spawns `boubou acp --model <tag>` and
speaks JSON-RPC 2.0 over stdio (protocol v1, SDK `agent-client-protocol` pinned `=3.6.0`, weekly
majors upstream). The CLI, web UI and core are untouched apart from the subcommand.

Shape. `BoubouAcp` implements the SDK's `ConnectTo<Client>`, so the same component runs over
`Stdio` in production and over an in-process `Channel` in tests. One Boubou session per ACP
session (`session/new` → `acp_MMDD_HHMMSS`, cwd = the editor's cwd, history in memory and in
the usual session files). `session/prompt` must not hold the SDK's dispatch loop, so the handler
only takes the session state, starts the turn on an OS thread (the loop is synchronous) and
spawns a connection task that drains one channel of `BridgeMsg`: loop events → `session/update`
notifications, the gate's question → `session/request_permission` (the tool thread blocks on a
reply channel; allow / allow for the rest of the turn / deny map onto the gate's verdicts, a
`cancelled` outcome denies and raises the stop flag), and the loop's result → the
`session/prompt` response (`end_turn`, `max_turn_requests`, `cancelled`). `session/cancel` sets
the stop flag the loop and the Ollama stream already check.

Mapping (`bridge.rs`, pure, unit-tested): text → `agent_message_chunk`, thinking →
`agent_thought_chunk`; a tool call → `tool_call` (kind from the tool name: read/search/edit/
delete/execute/fetch/think, status in_progress, raw input, file locations) and its result →
`tool_call_update` (completed, or failed when blocked or a `[tool error`, output as content, a
diff for create/edit/append); `todo.json` → `plan` after every `todo` call and at the finish;
context usage → `usage_update`; compaction and errors → `notice`. `finish` is not shown as a
tool call; its summary becomes the closing message only when the model wrote no prose after its
last tool call (a bare finish already triggers the loop's wrap-up text). Harness log lines go to
stderr through `tracing` (`RUST_LOG=boubou_acp=info` shows them); stdout is the protocol.

Verified: 7 bridge unit tests; two end-to-end tests over an in-process channel with a scripted
model (initialize, session/new, a two-tool turn with a denied permission, the summary as closing
text, a second prompt continuing the history, and cancel → `cancelled` within a few calls); a
live stdio run of `target/debug/boubou acp --model gemma4:e2b` driven by a scripted Python client
(plan, `create_file`, streamed text, `end_turn` in 15 s, file on disk). Not yet: the ACP Test
Compatibility Kit, session modes, the client's `fs`/`terminal` capabilities, `mcpServers` from
`session/new` (Boubou keeps `mcp.json`).

Zed verified the same morning (qwen3.5:9b, a Flutter project): connects, streams, tool cards,
permission dialogs. Restarting the agent then failed with Zed's "loading or resuming is not
supported by this agent", because Zed reopens a thread with `session/load` (bead boubou-m64.1).
Added: `loadSession` is advertised; the handler reads `<home>/sessions/<id>.json` (the ACP
session id is the Boubou session name), replays the history through `bridge::replay_updates`
(user turns as `user_message_chunk`, harness notes and system messages dropped, tool calls and
results with `hist_N` ids, a dangling call marked failed, the finish summary where the model
wrote no prose, the plan last) before responding, and restores the in-memory state with the
request's `cwd`. `TurnSetup` now carries `sessions_dir` so the loop saves where the server
reads. A session id that is not one path component of `[A-Za-z0-9_-]` is refused before any
disk access.

## The question route can run commands (2026-10-29, bead boubou-o5a)

Mingbird's Q&A turn offers read_file, list_dir, web_search and web_fetch. A question about the
state of a repo, a backlog or a build is a command by nature, and a model with no shell invents
one: in acp_1029_104013 (qwen3.5:9b, Zed, a Flutter project) "What bead should we tackle next?"
produced 36 tool calls and 79 messages, among them 3 calls to an invented `run_shell_command`,
a raw `bd show …` emitted as a tool name, and 8 web searches used as a shell (`bd ready
overshare`). The earlier "assess the current state of our project" turn had the same shape.
Change: `run_bash` joins `face::CHAT_TOOLS`; the Q&A guard blocks `face::QA_BLOCKED` (the four
file mutators) instead of `MUTATE`; the chat prompt names "a command that reports state" among
the reasons to use a tool. The safety gate vets every command as before (under ACP that is the
editor's permission dialog). Judged on user experience, not Mingbird parity.

Measurement, one controlled replay of the exact question over `boubou acp` against the
overshare checkout, qwen3.5:9b, before and after the change:

| | before | after |
|---|---|---|
| tool calls | 19 | 5 |
| web searches used as a shell | 8 | 0 |
| wall time | 108 s | 28 s |
| answer | "run `flutter pub get`, then task 1.1" (already done) | "overshare-9h3 is in progress; finish it first" (from `bd list` and `bd show`) |

Open follow-ups from the same transcript: bound read-only calls on a question turn
(boubou-9c5), refuse command-shaped web_search queries (boubou-i3r), qwen3.5:9b quirks for the
profile note (boubou-1sm).

## Context-pressure tier (2026-10-29, bead boubou-65d.9)

Mingbird's LRAB tiers never pushed the context: the 12B cells peaked at ~14.6k prompt tokens
inside 32k, so the compaction ladder (`history.rs`) had no measurement behind it and bead
boubou-65d.6 (recency pruning) nothing to be measured against. `bench/lrab_ext/` is a
Boubou-side task root in LRAB's layout, generated deterministically by `gen_pressure.py` and
scored by Mingbird's scorer imported read-only (`bench/lrab_ext/README.md`). Four tasks whose
inputs do not fit a small context: CP-01 a 2,400-line log to summarise with exact error counts,
CP-02 a package whose test runner prints ~1,300 lines with three planted bugs, CP-03 a boundary
bug at line 1,425 of a 1,552-line module, CP-04 sixteen `DECISION:` lines across eight ~220-line
notes. Collateral damage is caught by a `check_collateral.py` shipped with the fixture (AST
fingerprints of every definition the task did not ask to change). Driver: `lrab_h2h.py
--task-root --ctx 16384,32768,65536` sweeps `AGENT_CTX`, run ids get `_c16k`, the summary
groups rows as `task@ctx`, `process_metrics.py` now counts the ladder's own
`[context compaction L{n}]` lines (`compactions`, `compaction_max_level`); the scorer gets the
same `python` shim as the agent (the smoke cell scored 0 with exit 127 although the agent's own
run of the same command had passed). 8 tests in `bench/test_lrab_ext.py`, including drift
between generator and checked-in fixtures.

Baseline, qwen3.5:9b, one cell each, 20-minute budget (total 35 min):

| task | 16k | 32k | 64k |
|---|---|---|---|
| CP-01 log triage | 1.00, peak 10.5k, L1 ×1, 21 calls | 1.00, 10.8k, 25 | 1.00, 7.8k, 20 |
| CP-02 verbose tests | 1.00, 9.6k, L1 ×1, 37 calls | 1.00, 13.0k, 39 | 1.00, 13.4k, 44 |
| CP-03 deep edit | 1.00, 6.2k, 18 calls | 1.00, 7.6k, 23 | 0.43, 7.8k, 22 |
| CP-04 many files | 1.00, 6.6k, 16 calls | 1.00, 6.6k, 14 | 1.00, 6.8k, 16 |

Reading: the pressure is latent with this model. qwen3.5:9b does what the prompts say (grep the
log, read one region of the module, look at the FAILED line), so peak prompt usage stays at
7–13k whatever the window, compaction fires only at 16k (L1 twice, never L2/L3), and the score
is flat across windows. L1 did its job where it fired: CP-01 went from 63% to 35% in one step
and finished at 57%. The one miss (CP-03 at 64k) is placement, not context: `fix_note.md` was
written inside `cp03_billing/` instead of the task directory. The `integrity` charge on CP-04
at 64k is a false positive of the lexical check: the last command before the accepted finish
was a verification `grep` that exited 1 for no match.

The 8k column is where the ladder is really exercised, and it shows what compaction costs:

| task | 8k | vs 16k |
|---|---|---|
| CP-01 log triage | 0.86, peak 7.6k, L1 L2 L3, 20 calls, 351 s | 1.00, 179 s |
| CP-02 verbose tests | **timeout** at 20 min, 25 compactions (L2 ×2), 110 calls | 1.00, 230 s |
| CP-03 deep edit | 1.00, 7.5k, 9 compactions up to L3, 35 calls, 501 s | 1.00, 123 s |
| CP-04 many files | 1.00, 6.1k, L1 ×2 L2 ×1, 19 calls, 202 s | 1.00, 117 s |

Two failure shapes, both information loss by the ladder rather than model error. CP-01: the
`grep FATAL` output (call 4) was evicted by L3 (17 → 5 messages) one call before the summary was
written, so the "Fatal events" section has the three codes from the model's own prose but no
timestamps or request ids, which is exactly the check it failed. CP-02: all three bugs were
fixed and the suite green by +252 s; the remaining 15 minutes were 73 `run_bash` calls re-running
tests and re-reading files after each L2 wiped the history to 7 messages (37 → 7, 44 → 7), the
model no longer knowing it had finished; the one finish at +1131 s was rejected because
`fix_report.md` was never written. Wall time doubles to quadruples at 8k on the tasks that did
finish. A side finding from the same cell turned out to be half the cause: `edit_file` on
`textkit/tokens.py` was refused by the safety gate because `SENSITIVE_PATTERNS` contained
`\token`, which matched any path segment starting with "token"; the model wrote the file with
`cat >` in `run_bash` instead (bead boubou-a09, fixed the same day: credential-shaped names
only). Rerun with the fix, same window, pruning off: 1.00 in 245 s, 36 calls, 6 compactions
(L2 twice) and the model still finished. So at 8k the ladder costs time and precision (CP-01's
request ids) but the timeout was the gate bug compounded by compaction, not compaction alone.

Consequence for boubou-65d.6: the 8k column is its baseline (score, wall, calls, compactions,
peak), with 16k as the mild case. Recency pruning should keep the latest tool results verbatim
(the FATAL grep in CP-01) and summarise evicted pairs as "done: X" facts (the green test run in
CP-02) so the loop neither loses the data it is about to use nor forgets that it has finished.
At 32k and above there is nothing to improve on this tier with this model; a less disciplined
model (gemma4:e2b reading files whole) is the second arm worth running.

## Recency pruning before the ladder (2026-10-29, bead boubou-65d.6)

Mingbird's compaction ladder is all-or-nothing on the old history: L1 clips every old tool
output to 120 chars, L2/L3 replace everything but the tail with one model-written summary of
at most 150 words. The 8k column of the context-pressure tier showed the two ways that loses
work (above): the data the model is about to use is evicted (CP-01), and the knowledge that a
step is already done is evicted with it (CP-02). `history::prune_history` runs first, from half
the window (`PRUNE_AT` 0.50, on the pre-call estimate), deterministic and without a model
call: the last `PRUNE_KEEP` = 5 tool results stay verbatim; every older result becomes one fact
line, `[earlier] run_bash(python run_tests.py) → [exit 0] … 60 passed, 0 failed` (tool, target
argument, first line of the result and, when different, its last line), and the assistant turn
that made the call loses its thinking and any string argument over 160 chars (a `create_file`
body, an `edit_file` old/new pair). The history keeps its shape, so every call still has its
result and the request stays valid; only the bulk goes. Idempotent, so it runs on every
pre-call check above the threshold at no cost. The ladder is unchanged and still applies above
it on whatever the estimate says after pruning. `AGENT_PRUNE_KEEP` overrides the count, 0
disables pruning (the bench's A/B). Logged as `[context pruning: N older tool results → fact
lines, ~a → ~b tokens]`, counted by `process_metrics.py` as `prunes` / `pruned_results`.
Tests: `tests/history.rs` (keep/collapse, idempotence, argument shrinking, parallel-dispatch
names) and `tests/agent_loop.rs` (fires from 50% without the ladder; `prune_keep = 0` is a
no-op). Not in Mingbird; the design follows "Less Context, Better Agents" (recency pruning of
tool pairs) and the tier's own failure shapes.

Measurement, qwen3.5:9b, one cell each, pruning on (`AGENT_PRUNE_KEEP=5`) against the
baseline above (`bench/results/lrab_ctx_prune` vs `lrab_ctx`):

| cell | baseline: score, wall, calls, compactions | pruning: score, wall, calls, compactions, results pruned |
|---|---|---|
| CP-01 @8k | 0.86, 351 s, 20, 3 | 0.86, 214 s, 19, 2, 11 |
| CP-02 @8k | timeout, 110 calls, 25 | timeout, 109 calls, 16, 94 (both confounded by boubou-a09, rerun below) |
| CP-03 @8k | 1.00, 501 s, 35, 9 | 1.00, 203 s, 24, 2, 18 |
| CP-04 @8k | 1.00, 202 s, 19, 3 | 1.00, 203 s, 23, 3, 17 |
| CP-01 @16k | 1.00, 179 s, 21, 1 | 1.00, 251 s, 30, 2, 0 |
| CP-02 @16k | 1.00, 230 s, 37, 1 | 1.00, 355 s, 51, 0, 42 |
| CP-03 @16k | 1.00, 123 s, 18, 0 | 0.43 (fix_note.md misplaced, as the baseline's 64k cell), 243 s, 27, 0, 15 |
| CP-04 @16k | 1.00, 117 s, 16, 0 | 1.00, 100 s, 14, 0, 0 |
| CP-02 @8k, gate fixed (boubou-a09) | 1.00, 245 s, 36, 6 | 0.81 (`fix_report.md` names one of three functions), 740 s, 68, 11, 54 |

Reading. Mechanically it does what it says: at 16k a pass reclaims 1.7–3.9k tokens
(`~8321 → ~4372`) and CP-02 ran with no ladder compaction at all. At 8k it reclaims almost
nothing: 2.3k tokens of system prompt and tool schemas (28% of the window) plus the five
verbatim results are the bulk, most passes free under 300 tokens, and L1 at 60% then clips the
kept results to 120 chars anyway. Outcome: no score moved (CP-03 @16k is the placement miss
seen without pruning too), compactions fell at 8k, wall time fell on two 8k cells and rose on
three 16k cells where call counts also rose (30 vs 21, 51 vs 37, 27 vs 18). At n=1 that spread
is within the model's own variance (the baseline's CP-02 ran 37, 39 and 44 calls at three
windows), so nothing can be claimed either way; the gate-fixed CP-02 pair at 8k leans against
pruning (the pruned arm lost track of where the task directory was, tried `../../fix_report.md`
and absolute paths, and took three times as long). Verdict, as with the decider: the mechanism
stays in, **off by default** (`prune_keep` 0), `AGENT_PRUNE_KEEP=5` turns it on; a `--reps 3`
run of the 16k column is the measurement that would change the default. If the 8k case is
to be won, the fixed overhead and the verbatim tail are the levers, not the old results.

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
  Measurement additions (2026-10-28, epic boubou-65d): `--reps N` runs every cell N times with
  the (task, arm) order round-robin and the arm order alternating per task, so machine drift
  cannot follow one arm; the summary then shows per-task mean and spread plus a per-arm bootstrap
  95% CI over tasks. Every cell also carries `score.json["process"]` from `bench/process_metrics.py`
  (one parser for both arms, since Boubou mirrors Mingbird's transcript lines): tool calls, peak
  prompt tokens, failed calls, non-zero exits, loop interceptions, empty resets, finish refusals,
  pre-delivery checks, truncations, compactions, and an *integrity* count = files named in the
  task prompt that are missing after an accepted finish + a finish accepted after the last command
  failed. `python bench/process_metrics.py <results_dir>` backfills older result sets. Tests:
  `uv run --with pytest --python 3.13 python -m pytest bench/ -q`.
  Micro-probes (`bench/probes.py`, Boubou only, one mechanism per probe, n trials each):
  `emission` sends Boubou's own schemas (`boubou tools`) straight to Ollama `/api/chat` on twelve
  one-tool prompts and reports the schema-valid rate per model + quant (the model-side floor;
  a parser or model problem must never be charged to the harness); `false_finish` is a task whose
  done-state is only true once `python check.py` prints OK, scored by our own run of check.py
  after the agent exits (false-success rate = finish accepted while the check fails);
  `loop` runs a `fetch.py` that always fails and measures identical attempts before a guard
  fires, clean ending, and whether summary.md is honest or fabricates a row count. Results in
  `bench/results/probes/<probe>/<model>/`, `--summary` prints the table.
  First probe finding (2026-10-28, bead boubou-65d.11): the Ring-1 `DANGER_CMD` substring test
  refused a `python -c` CSV script eleven times because a comment read `# Format the result`.
  Departure from Mingbird: the entries that are also ordinary words (`format `, `halt`,
  `shutdown`, `shred `, `srm `, `dism `, `poweroff`, listed in `DANGER_CMD_TOKEN_ONLY`) now count
  only where a command starts (string start or after `;` `&` `|` newline `(` backtick, optionally
  behind `sudo`); the rest of the list (`rm -rf`, `of=/dev/`, `curl | bash`, …) stays a substring
  match so a dangerous op hidden inside a script is still caught. `safety::danger_hit`.

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

