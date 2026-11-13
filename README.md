# rufous

A local-model autonomous agent, written in Rust. rufous is the successor to
[Mingbird](https://arxiv.org/abs/2610.02001), the Python/Tk original, and keeps its two ideas:

- **Tiny prefill.** System prompt plus nine core tool schemas, about 1250 tokens, so a 1B–4B
  model running under Ollama can drive it.
- **Progressive disclosure.** Web, memory and MCP tools are enabled on demand with `enable_tools`
  (the file tools are always there);
  skills are listed by name and loaded only when used, as flat `.md` files or as `SKILL.md` folders
  in the Agent Skills convention (`.agents/skills`, `.claude/skills`); a skill with `triggers:` or
  `autoload:` in its frontmatter is prefilled for matching tasks, and a `SYSTEM.md` or `AGENTS.md` in
  the working directory adds per-folder instructions (see `docs/skills.md`).
- **Hooks.** A project or a user runs their own commands at the loop's events (`SessionStart`,
  `PreToolUse`, `PostToolUse`, `Stop`) from a `hooks.json` in the shape Claude Code, Codex and
  Cursor share; a project's `.claude/settings.json` hooks run as they are (see `docs/hooks.md`).
- **MCP from the project.** A `.mcp.json` in the working directory, in the `mcpServers` shape the
  other harnesses write, adds its servers to the home `mcp.json`; under an editor the servers the
  client passes at `session/new` join them for that session (see `docs/skills.md`).

The name: the rufous hummingbird, *Selasphorus rufus*, weighs about three grams and migrates
from Mexico to Alaska and back every year. Small does not have to mean limited, which is the bet
this harness makes on small models. *Rufous* means rust-coloured; it is written in Rust. (Until
the alpha the project was called boubou, after the Southern boubou, a bush-shrike.)

## Install

```sh
curl -fsSL https://rufous.chat/install.sh | sh
```

The script downloads the release archive for your machine from this repository's GitHub
releases, checks its SHA-256 and puts `rufous` in `~/.local/bin` (no sudo). `RUFOUS_VERSION=v0.3.0`
pins a release; `RUFOUS_INSTALL_DIR` picks another directory. Builds exist for macOS (Apple
silicon, Intel), Linux (x86_64, arm64; glibc 2.35 or newer) and Windows (a zip on the releases
page). Then run a model under [Ollama](https://ollama.com) and check the setup:

```sh
ollama pull qwen3.5:4b
rufous doctor
```

A release is cut by pushing a `v*` tag: `.github/workflows/release.yml` builds each target and
publishes the archives the script fetches.

## Status

Core in progress. Typed protocol, config, skills, Ollama client, the five-ring safety gate,
the tool executor (files, commands, todo, crawl guard), all pure guards and the agent loop
(finish gates, anti-loop ladders, compaction, checkpoint/session/transcript files) are ported
and tested. `rufous run <model> <taskfile> <workdir>` works end to end against a local Ollama;
sessions it writes load in Mingbird and vice versa. Web search/fetch (shared result cache),
long-term memory, parallel `batch_tools` and MCP (stdio and streamable HTTP via `rmcp`, tools
flattened into the categories as `server.tool`) are in. `rufous doctor` probes every MCP server;
`rufous tool <name> '<json>'` runs any single tool, built-in or `server.tool`, for debugging.
Web search defaults to Brave, then DuckDuckGo, then Bing (`AGENT_SEARCH_BACKENDS` overrides); a query that names a site is fetched directly first, and results sharing no word with the query are dropped.
`rufous serve` runs the local web UI: a session rail, the conversation with per-turn run blocks
(every tool call with its step time), inline permission cards, a pinned composer with the model and
Online/Offline controls, and an inspector with the live plan, activity and files; dark by default,
light available; the agent runs in-process. `cargo run -p rufous-app` opens the same UI in a native window.
A chat turn runs under the `general` turn policy (`docs/refactor.md`): it ends when the model
replies or calls `finish`, as in an editor; "Batch task mode" in the settings opts a session into
the `unattended` policy, Mingbird's task harness (finish gates, test guard, continue notes).
Parallel dispatch is in and off by default: with `"parallel": {"enabled": true, "child_model":
"<tag>"}` in `config.json`, a plan with enough simple single-file items is farmed out to
sandboxed child processes (`AGENT_CHILD_SANDBOX=1`, default-deny tool face, writes only inside
the child directory, audit log, exit 53 on a severe violation); each child runs at
`parallel.child_ctx`, the context window the capacity plan budgets for it. The hook looks at the
plan when the loop starts and once more right after the model writes it, so a fresh task can
dispatch (Mingbird looked only before the first turn, when a new task has no plan yet). `rufous
plan` shows what this machine could run. The engine is named by `--backend ollama|lmstudio|openai` or `"backend"` in `config.json`
(else `local_backend.type`, as in Mingbird) and logged as `[engine] …` at run start: Ollama (restarted
automatically after four consecutive 500s; a server rufous did not start is given time to be
relaunched by its own supervisor, the Ollama app or systemd, and a bound port is never spawned
over, so there is only ever one server), an OpenAI-compatible local server (LM Studio,
llama-server) or a cloud provider. Model-specific settings come from a per-model profile
(`think`, `temperature`, hidden tools, prefill cap, the gemma4 finish-prose rescue, a
`prompt_note` appended to the task prompt), logged as `[profile] …` at run start;
`"profiles": {"gemma4:12b": {"think": true, "prompt_note": ""}}` in `config.json` overrides a
built-in entry per family or tag (an empty note switches the built-in one off). A System One
decider (`"decider": {"mode": "shadow"}`, model `d1:latest` through Ollama's native decision
API) can shadow or take over the question-or-task decision; off by default. Voice input is behind the `voice` cargo feature
(`cargo build --features voice`, sherpa-onnx linked statically): `rufous voice <wav>` and the
web UI's mic button transcribe with the bundled sherpa-onnx English model from `~/.ollama_agent/stt/`
or `RUFOUS_STT_DIR`, falling back to an Ollama audio model. `rufous acp --model <tag>` serves the
[Agent Client Protocol](https://agentclientprotocol.com) over stdio, so an editor (Zed, JetBrains,
Neovim) can run rufous as its agent: streamed text and thoughts, tool calls with status and diffs,
the plan, and the safety gate's questions as the editor's permission dialog (see
[docs/acp.md](docs/acp.md)). Next: voice model installer, Tauri bundling, the LRAB bench adapter.
The Python → Rust module map is in [docs/PORTING.md](docs/PORTING.md); dated engineering
notes are in [docs/changelog.md](docs/changelog.md); the plan for separating the small-model
kernel from the turn policies, towards a general-purpose harness and a coding harness over
small models, is in [docs/refactor.md](docs/refactor.md).

`run_python` and the three skills that depended on it (office_word, office_excel, image_batch)
are not carried over; rufous ships no Python runtime.

## Compatibility with Mingbird

rufous reads Mingbird's data directory unchanged (`~/.ollama_agent`, or `RUFOUS_HOME` /
`MINGBIRD_HOME`): the same `config.json`, `mcp.json`, `skills/` and sessions. Values
Mingbird accepts load here too (a boolean `slim_output`, numbers in `ollama_env`, `"1"` for
`offline_mode`), and a key rufous cannot read is dropped with a warning on stderr rather than
stopping the binary; only a file that is not JSON is an error.
Tool schemas are the same JSON Mingbird sends, so prompts and the bench numbers carry over.

## Build

```sh
cargo build --release            # core + CLI (rufous)
cargo build --release -p rufous-app   # Tauri desktop shell (heavier; needs the Tauri prerequisites)
cargo test
```

Windows binaries link the C runtime statically (see `.cargo/config.toml`). To cross-compile
from macOS or Linux: `cargo install cargo-xwin && cargo xwin build --release --target x86_64-pc-windows-msvc`.

## CLI

```sh
rufous doctor                    # config, Ollama reachability, models, skills, MCP servers
rufous mcp add framit -- node /path/to/framit/dist/cli.js serve   # as `claude mcp add`
rufous mcp add -s project -t http -H "Authorization: Bearer …" remote https://host/mcp
rufous mcp list | get <name> | remove <name>
rufous models
rufous skills [workdir]
rufous tools --enable web_fetch  # print the schemas the model receives
rufous run <model> <taskfile> <workdir> [--session NAME] [--new] [--append]
rufous chat <model> <workdir>   # the general-purpose harness at a terminal: same turn policy as the web UI
rufous acp --model <model>      # Agent Client Protocol over stdio, for an editor (--policy general for a desktop app)
```

Environment variables are inherited from Mingbird: `OLLAMA_HOST`, `OLLAMA_BIN`, `AGENT_CTX`,
`AGENT_TEMP`, `AGENT_NUMPREDICT`, `AGENT_THINK`, `AGENT_SYSTEM_FILE`, `AGENT_SLIM_OUTPUT`,
`AGENT_OFFLINE`. `RUFOUS_API_KEY` is the API key of the engine `--backend` names, over
`local_backend.api_key`, and `RUFOUS_CLOUD_API_KEY` the cloud provider's, over `cloud.api_key`: for
a host that keeps keys in a keychain and must not write them to `config.json` (neither is ever
written back). Under an editor, which passes no environment, `rufous acp --ctx <tokens>` sets
the context window instead (see `docs/acp.md`). Every named session also keeps the loop's protocol
lines in `~/.ollama_agent/sessions/<name>.log` (`AGENT_SESSION_LOG=0` turns that off; `bench/session_metrics.py <log>` reads one).
The scope gate asks before the model, told to work on beads, issues or documents, first writes a
source file or runs a build (`"scope_gate": "on" | "shadow" | "off"` in `config.json`; see `docs/acp.md`). The switches take `1/0`, `on/off`, `yes/no` or `true/false`; a value that is
not a number or a switch word is logged and ignored rather than stopping the agent, as in
Mingbird. Command-output trimming follows `AGENT_SLIM_OUTPUT`, then `config.json`
`slim_output` (`auto`/`on`/`off`), then the context size (on at 32K and below).
Ctrl-C in `run` or `chat` ends the run after the current step with the
session saved; a running command's process tree and any dispatched children are terminated.
A second Ctrl-C quits at once.

## Layout

```
crates/rufous-kernel   the small-model agent loop and the product-neutral parts: protocol, tools,
                       safety, config, skills, MCP, Ollama/OpenAI clients, the turn policies
crates/rufous-general  general-purpose harness: the `general` policy, its prompt and skills (`skills/`),
                       the local web UI (`rufous serve`), the terminal chat (`rufous chat`)
crates/rufous-coding   coding harness: the `coding` policy, its prompt and skills (`skills/`),
                       the Agent Client Protocol server (`rufous acp`)
crates/rufous-voice    speech-to-text adapter (sherpa-onnx, Ollama audio fallback)
crates/rufous-cli      `rufous` binary: wires the crates together, plus `run` for the batch harness
crates/rufous-app      Tauri shell around rufous-general (not built by default)
docs/skills.md       the skills directories, precedence, and how to write one
docs/PORTING.md      Python → Rust module map and port status
docs/changelog.md    dated engineering log (what shipped, what was measured)
docs/refactor.md     kernel-and-policies refactor: intent and progress
docs/guide.md        Effective Agentic Coding with Smaller Models: prep, harness, measurement (in progress)
```

## License

Apache-2.0, as Mingbird.
