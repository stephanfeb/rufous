# rufous as a sandboxed helper inside a host app (boubou-f8d)

Choreo's item 3 (requirements doc: https://claude.ai/artifact/3LnyuZaigSjibqGMGrfTDM). Draft, 2026-11-05.

## The setting

Choreo copies a pinned rufous into `Choreo.app/Contents/Helpers/rufous`, signs it with
`app-sandbox` + `inherit` under the hardened runtime, and starts `rufous acp --policy general`
itself. Inside the sandbox `HOME` is Choreo's container (`~/Library/Containers/<id>/Data`), the
`ollama` binary cannot be started, and any path the user did not share fails with `EPERM`. Choreo owns
the engine, the data directory and the process lifetime. rufous has to take all of its configuration
from flags and the environment, report access failures in words the model can act on, and leave
nothing running behind it.

What each ask meets today, checked on 2026-11-05:

| Ask | Today |
|---|---|
| `RUFOUS_HOME` as the data directory | Works (`config::agent_home`); documented only in a module comment. |
| Engine URL for LM Studio / llama-server | Only `config.json` `local_backend.base_url`. |
| Don't start the engine | `rufous acp` calls `OllamaProc::ensure(25 s)` and spawns `ollama serve` on a free port. Four 500s restart it. A refused connection is retried with back-off (3, 6, 12 … 60 s) for eight attempts, about 4.5 minutes, before the turn fails. |
| Sandbox refusals | `p.exists()` is false for a path the sandbox refuses, so `read_file` says `[not found: …]`. `list_dir` says `[not a dir: …]`, `search_files` says `(no matches)`, and the writes say `[tool error: Operation not permitted …]`. |
| `Home directory:` line | `dirs::home_dir()`, which is the container path inside the sandbox. |
| Binary | Links only `/System/Library/Frameworks` and `/usr/lib`, minos 11.0, default features off `voice`. No universal build script. |
| stdin closes | `rufous acp` exits at once, idle or mid-turn (fake engine, 2026-11-05). A `run_bash` command still running is left behind, re-parented to launchd (`sleep 301`, ppid 1). A stdio MCP server that ignores its own stdin closing would be left behind too. |

## Design

1. **`RUFOUS_HOME`** stays the host's way to set the data directory. `docs/acp.md` gets a section
   "Inside a host app" that names it with the other host settings below. No code change.

2. **`--base-url <url>`**, a global flag. It overrides `local_backend.base_url` for this process
   and is never written to `config.json`. It needs `--backend lmstudio|openai`, from the flag or the
   config. With Ollama, or with no backend named, it is an error at start that names `OLLAMA_HOST` as
   Ollama's setting. A silent fallback would send the model calls somewhere the host did not choose.

3. **`--no-start-engine`**, a global flag (`Config::no_start_engine`, runtime only). The engine
   belongs to someone else:
   - At start, `ensure` is not called. `rufous acp` makes one 2-second probe and logs
     `[rufous acp] engine <host> not answering; not starting it (--no-start-engine)`, then serves anyway.
     The host may start the engine after rufous.
   - The Ollama backend is built without an `OllamaProc`, so four 500s take the compaction path,
     as an OpenAI-compatible engine's do today. Nothing is killed or spawned.
   - A model call that cannot connect fails the turn at once. The backends report a refused or
     unresolvable connection as `ChatError::Unreachable` (reqwest `is_connect()`), distinct from
     `Transport`. `RetryLadder` gets `fail_fast_unreachable`, set from `RunParams::engine_unmanaged` (from
     `cfg.no_start_engine`), and answers `GiveUp` to it. The turn ends with
     `model server: <engine>: not reachable (<reason>); rufous does not start the engine under
     --no-start-engine`. Without the flag, `Unreachable` is classed as `Other` and nothing changes.
     A rufous-managed Ollama that is restarting is refused for a moment, and the back-off is what
     rides that out.

4. **`[no access: <path> (not shared with this assistant)]`.** One helper in `exec.rs` builds the
   text from an `io::Error` whose kind is `PermissionDenied`, which covers `EPERM` and `EACCES`.
   - `read_file` and `delete_file` look at `symlink_metadata` before `exists()`, which hides the
     error.
   - `list_dir` checks the `read_dir` error.
   - `search_files` checks its root `read_dir`. A refused subfolder below the root is skipped, as
     now.
   - `create_file`, `edit_file` and `append_file` check the store's read and write errors.
   - The opening goes into `ERROR_OPENINGS` and `FAIL_MARKERS`, so the failure ladders count it.

   Nothing changes for any other error, so the bench prompts and results are untouched. The words
   "not shared with this assistant" tell the model the path exists and that asking the user is the
   way forward. A model that searches elsewhere after `[not a dir]` is following the text it was
   given.

5. **The `Home directory:` line.** `config::user_home()` resolves the home the prompt names:
   - `RUFOUS_USER_HOME` (set by the global `--user-home <path>`) when given;
   - else nothing inside an App Sandbox (`APP_SANDBOX_CONTAINER_ID` is set);
   - else `dirs::home_dir()`.

   `fill_base` drops the whole line when there is none. A line that names the container would send
   the model to the wrong Desktop.

   I chose sandbox detection over "leave the line out when `RUFOUS_HOME` is set", because every bench
   sets `RUFOUS_HOME` (`acp_convo.py`, `host_tools.py`), and the prompt would change under them. The
   safety gate's `~` expansion keeps `dirs::home_dir()`, because that is what the shell `run_bash`
   starts will expand `~` to.

6. **Nothing left running.**
   - `exec.rs` keeps a registry of the process groups rufous has started and not yet reaped.
   - `run_command` registers its group leader after the spawn and removes it after the wait.
   - A stdio MCP server is spawned as its own group leader (`process_group(0)`) and stays registered
     for as long as its connection; `connect` returns a guard with the client.
   - `kill_live_groups()` sends `SIGKILL` to each group (`killpg`, as `kill_tree` does).
   - `rufous acp` calls it when `serve_stdio` returns (stdin closed), and on `SIGTERM`, `SIGHUP` or
     `SIGINT` before it exits. A `tokio::select!` over the server and the signals handles this.

   An `ollama serve` that rufous started is not touched; that is boubou-t70's question, and it
   does not arise under `--no-start-engine`.

7. **The binary.** `scripts/macos-universal.sh` builds both targets with
   `MACOSX_DEPLOYMENT_TARGET=11.0` and default features, and joins them with `lipo`. It checks
   that `otool -L` lists only `/System/Library/` and `/usr/lib/`, and prints `minos` and a SHA-256.
   It runs against `target/<triple>/release`, so it never touches the `target/release/rufous` the
   bench drivers launch. rufous has no JIT and asks for no entitlements, so hardened runtime with
   none must work. This is confirmed once Choreo signs the universal binary.

## Tests

- `config`: `--base-url` without lmstudio/openai is an error; with `lmstudio` it overrides the
  config URL. `user_home` follows the three rules (env-isolated).
- `ladders`: `Unreachable` → `GiveUp` with `fail_fast_unreachable`, `Backoff` without it.
- `agent_loop`: a backend that answers `Unreachable` ends the turn after one call with the
  `--no-start-engine` text, and without the flag it is retried.
- `exec`: a file the test makes unreadable (`chmod 000`) gives `[no access: …]` from `read_file`,
  `list_dir`, `search_files` and `create_file`. Running as root skips this test.
- `exec`: a registered `sleep` group is gone after `kill_live_groups()`.
- `prompt`: `fill_base` with no user home has no `Home directory:` line.
- By hand, with the fake engine in this design's checks: `--no-start-engine` against a closed port
  fails the turn in under 2 s. Closing stdin mid-`run_bash` leaves no `sleep` behind.

## Not in this change

- Cloud API keys from the environment (out of scope per Choreo).
- Detaching an `ollama serve` that rufous started (boubou-t70).
- A tagged release with checksums per change: the script prints the checksum, and the release
  cadence is a separate answer to Choreo.
