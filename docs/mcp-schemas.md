# MCP tool schemas: enum, required as sent, parameter descriptions for host tools (boubou-erw)

A request from Choreo, 2026-11-05, from their bizframe eval (qwen3.5:2b, qwen3.5:4b, gemma4:e2b
over ACP, `--policy general --host-tools on`).

## The setting

`compact_schema` (`mcp/manifest.rs`) shapes every MCP tool's input schema when the server is
probed (`list_probed`, `mcp/client.rs`). It was ported from the Python harness and keeps, per
parameter, only `{"type": …}`:

- **`enum` is dropped.** bizframe's `set_entity.kind` is
  `{"type": "string", "enum": ["master", "document", "log"]}` and reaches the model as
  `{"type": "string"}`; `set_field.type` loses its 12 values. The models wrote free-text labels
  for `kind`, and gemma4:e2b put the field type in the `options` array, because nothing in the
  text said which words `type` accepts.
- **Parameter descriptions are dropped**, so what each value means never arrives either.
- **A schema with no `required` list is sent as all-required.** bizframe's `show` has one
  optional parameter, `name` ("leave out for the whole app"), and the model is told it must
  pass it.

Small models follow the text they are given. The allowed values are the text that would settle
these calls, and the harness removes them.

## Design

1. **`enum` is kept on every parameter**, routed and host tools alike, when it is a list of
   strings, numbers or booleans. It costs a few tokens: about 15 for `kind`, about 40 for
   `type`.
2. **`required` is kept as the server sends it.** A missing or empty list means nothing is
   required. The rule that a tool with more than eight parameters keeps only the required ones
   stays, and applies only when the list names some; with none named all are kept, which is
   the set kept today.
3. **Parameter descriptions are kept for host tools.** The probe keeps each description on one
   line, cut to `PARAM_DESC_CAP` (120 characters), which bounds the cache and the prefill. The
   definition of a routed tool (`tool_defs`) strips them again, so an `mcp.json` server's tools
   cost what they did plus their enums. Host tools already carry 200-character descriptions
   and are capped at 15 (`docs/host-tools.md`). The host face is fixed for the session, so the
   prefix cache still holds.

Not changed: a parameter without a `type` is still dropped (an `anyOf` or a bare `enum`), and
`items` of an array parameter are still not described. The disk cache holds the compacted
schema; an entry written before this change expires within the five-minute TTL.

## Tests

- `compact_schema` keeps `enum` and capped descriptions, keeps `required` as sent, treats a
  missing list as none, and keeps the more-than-eight rule only when some are required.
- `tool_defs` gives a routed tool its enums without parameter descriptions; `host_face` gives a
  host tool both.

## Bench

Choreo's bizframe rig can run before and after on the three models; the build to point it at is
the commit that closes this bead.
