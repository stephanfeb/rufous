# Permission waits that survive a restart (boubou-lbo)

Choreo's item 5 (requirements doc: https://claude.ai/artifact/3LnyuZaigSjibqGMGrfTDM). 2026-11-05.

## The setting

Choreo runs on an always-on Mac, often with nobody watching. A `session/request_permission` can wait
for hours, until the user opens the app or answers a notification. The turn thread already blocks
with no timeout (`rrx.recv()` in `AcpConfirmer`), which is right. What it sends and what survives
a restart, checked on 2026-11-05:

- **The request.** `ask_permission` (`lib.rs`) sends a `ToolCallUpdate` whose title is the gate's
  action and target (`access: /Users/…/notes.md`) and whose `rawInput` is
  `{tool, action, target, workdir}`. The call's arguments are not in it: not the new file text,
  not the command. They went out earlier, in the `tool_call` notification. The diff only arrives in
  the `tool_call_update` after the call has run. A user answering hours later, from a
  notification, sees what kind of thing is asked, not what it will do.
- **A restart.** The loop writes the checkpoint (`<sessions>/<name>.state.json`) after the
  model's tool-call message is pushed and before any tool runs, so the pending call is on disk.
  `<name>.json`, which `session/load` reads, is saved only when a turn ends. If rufous dies
  mid-wait, `session/load` replays the previous turn's end. The user's prompt and the call that
  was asked about are both gone, and nothing says so.

## Design

1. **The request carries the whole proposed action.** `ask_permission` takes the call in flight
   from the bridge, which holds its name and arguments, and builds the request from it. The
   title is still the gate's action, because it says why rufous is asking. Added:
   - `rawInput.args`, the call's complete arguments. The existing `tool`, `action`, `target` and
     `workdir` keys stay as they are.
   - `locations`, as for the `tool_call`.
   - `content`: for a write (`create_file`, `edit_file`, `append_file`), the `Diff` the result
     would carry, with the full new text, or the old and new text of an edit; for `run_bash`, the
     exact command; for `web_fetch`, the URL; for an MCP call, the server, the tool and the
     arguments as JSON.

   A client that shows only the request now shows what will happen. With no call in flight (no
   such path today), the request is what it is now.

2. **A marker while a question is open.** Before the request goes out, `drive_turn` writes
   `<sessions>/<name>.asking.json` = `{"tool", "args", "asked_at"}`, and removes it once there is
   an answer, a cancellation or an error. A marker found later means the process ended while the
   user was being asked.

3. **`session/load` recovers the interrupted turn.** When the marker exists and the checkpoint ends
   in an assistant message with tool calls that have no results, the checkpoint is the history,
   since it holds the user's prompt and the call. Each of those calls gets a result:
   - the call named in the marker:
     `[not run: rufous stopped while this call was waiting for the user's permission, so nothing
     was done. If the user wants it now, make this same call again; they will be asked again.]`
   - any earlier call in the same message, which ran but whose result was never saved:
     `[interrupted: rufous stopped before this call's result was saved; what it did is unknown.
     Check before repeating it.]`
   - any later call in the same message, which never started:
     `[not run: rufous stopped before this call started, so nothing was done.]`

   The recovered history is written to `<name>.json`, the meta status becomes `interrupted`,
   the marker is removed, and the replay
   shows the call failed with that text. Both openings join the failure markers, so the replay
   marks them failed. A marker without a matching checkpoint tail is stale: it is removed and
   the session loads as before.

4. **"Go ahead" asks again.** The next prompt runs on the recovered history. The model reads the
   not-run result, makes the same call again, and the gate asks again, because an "allow for the
   rest of this turn" from before never carries into a new turn. Small models follow the text
   they are given, not the state behind it, so the note tells the model what to do in words.

Not changed: the wait itself has no timeout, and nothing on the rufous side wakes a sleeping host.

## Tests

- bridge or `lib.rs`: a permission request for `create_file`, `edit_file` and `run_bash` carries
  `rawInput.args`, the diff or the command as content, and the locations.
- `session`: `recover_asking` turns a checkpoint whose last call matches the marker into a history
  with the not-run result. An earlier call in the same message gets the interrupted result. A
  stale marker is removed and changes nothing.
- ACP (`acp_turn.rs`): a session whose files hold a marker and a checkpoint is loaded. The replay
  ends with that call failed with the not-run text, the next prompt sees it in its history, and
  the marker is gone.
- By hand: the debug binary against the fake engine asks for a `run_bash` permission, is killed
  mid-wait, and is restarted with `session/load`.

## Checked by hand (2026-11-05)

The debug binary ran against a fake engine (`fake_engine3.py` in the session scratchpad), which
proposes a `create_file` outside the workspace:

- The request carried `rawInput.args` with the path and content, the diff and the location. The
  marker was on disk while the question was open.
- After a `SIGKILL` mid-question, the file was not written and `<name>.json` did not exist.
- On restart, `session/load` replayed the prompt and the call, failed with the not-run text, and
  the marker was gone.
- "go ahead" brought the same call back as a new question. Allowed, the file was written and the
  turn ended `end_turn`.
