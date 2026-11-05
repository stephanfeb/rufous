# Per-call token usage (boubou-70l)

Choreo's item 6 (requirements doc: https://claude.ai/artifact/3LnyuZaigSjibqGMGrfTDM). 2026-11-05.

## The setting

Choreo meters cost per run and per model. For every model call it needs the input tokens, the
output tokens, the model id and the engine. Local models cost nothing there, so this matters
once a cloud backend is used. What rufous reports today, checked on 2026-11-05:

- `usage_update` carries the context occupancy (`used`, `size`), from `AgentEvent::ContextUsage`.
  The loop sends it after the main model call, and only when the engine reports a prompt
  count.
- A turn makes model calls besides the main one:
  - the compaction summary (`history.rs` `summarise`, from four places in the loop);
  - the corrector's consultation (`corrector::consult`);
  - the wrap-up after a bare finish (`run.rs`).

  None of these is reported anywhere except, for some, the session log.
- The model id the loop passes is not always the one billed. The cloud backend sends its
  configured `model_override`.
- The cloud flavour does not stream, so its `usage` block arrives. A local OpenAI-compatible
  engine streams without `stream_options.include_usage`, so its counts may be missing. Local
  calls are free in Choreo.

## Design

1. **Meter at the backend, not at each call site.** `chat.rs` gets `CallUsage { model,
   purpose, input_tokens, output_tokens }` and `Metered<'a, B>`, a `ChatBackend` that wraps
   the loop's backend for the length of `run`. Every successful `chat` records one
   `CallUsage`. A call added later is metered without anyone remembering to. `purpose` is set
   by the loop before each kind of call: `turn` for the main call, `compaction`, `corrector`,
   `wrap_up`. A failed or cancelled call records nothing, because there are no counts to
   report. `compact` sets and restores the purpose itself, since every compaction goes
   through it.

2. **The billed model id.** `ChatBackend::model_id(&self, requested) -> String`, which by
   default returns `requested`. `OpenAiBackend` returns its `model_override` when it has one.
   `Metered` records that id.

3. **Events.** `ContextUsage` gains `call: Option<CallUsage>`. The loop drains the meter
   right after the main call:
   - any side calls recorded since the last drain go out first, each as
     `AgentEvent::ModelCall { call, used, limit }`, where `used` and `limit` are the context
     as last reported;
   - the main call rides on its `ContextUsage`. When the engine reported no prompt count,
     so no `ContextUsage` is sent, the main call goes out as a `ModelCall` too.

   Calls after the last main call (a compaction, the wrap-up) are drained before the
   loop's `Finished`, and by `save!` on every other exit.

4. **ACP.** Each model call is exactly one `usage_update` with
   `_meta.rufous.call = {"model", "engine", "purpose", "input_tokens", "output_tokens"}`.
   - `engine` is the object `session/new` already returns in `_meta.rufous.engine`
     (`label`, `url`, `host`, `local`), so Choreo can join the two and tell free local
     calls from paid ones.
   - A token count the engine did not report is `null`, not 0.
   - The main call's update is the context update it always was, with the call added. A side
     call's update repeats the last context numbers.

   The bridge gets the engine object from the factory's `session_meta` when the turn starts.

5. **CLI and web UI.** They ignore `ModelCall` beyond the existing `[timing:]` log line, as
   they do `Egress`.

Not covered:
- The children of a parallel dispatch, which are separate rufous processes. Dispatch is not
  on the ACP surfaces.
- The decision model (`LayeredDecider`), which is a local Ollama classifier.
- A local engine that streams without reporting usage, whose counts stay `null`.

## Tests

- `chat`: `Metered` records model, purpose and counts per successful call, records nothing on
  an error, and uses `model_id`. `OpenAiBackend::model_id` returns the override.
- `agent_loop`: a scripted backend whose turn compacts and then finishes with a wrap-up yields
  `ContextUsage{call: turn}` for each main call and `ModelCall` for the compaction and the
  wrap-up, before `Finished`.
- bridge: `ContextUsage` with a call gives a `usage_update` with `_meta.rufous.call` and the
  engine; `ModelCall` gives one with the last context; null counts stay null.
- ACP (`acp_turn.rs`): every `usage_update` of a scripted turn carries `_meta.rufous.call`
  with the model id and the factory's engine.
