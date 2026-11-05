# The MCP tool cache follows each server's config and binary (boubou-gj0)

A request from Choreo, 2026-11-06: when a host app updates its MCP server (a rebuilt bizframe),
rufous should not serve the old tool descriptions for up to five minutes. Only if cheap.

## The setting

`McpRegistry::manifest` keeps the probed tools of every server in memory and in
`<home>/mcp_manifest_cache.json`, as `{ts, data}`: one timestamp for the whole file, the tools by
server name. Within the five-minute TTL the cache is served as it is. Checked on 2026-11-06:

- **A changed server.** A new `command`, `args` or binary under the same name is served its old
  tools until the TTL runs out.
- **A new server.** The cache does not record which servers it was built from. A server that is
  not in a fresh cache is not probed until the TTL runs out, so its tools are missing. `rufous mcp
  add` and `remove` delete the cache for this reason (changelog), but servers that arrive in
  `session/new` bypass that: a second session within five minutes, with a server the first did
  not have, gets no tools from it.
- **A removed server.** Its cached tools are still in the manifest, and `tool_defs` lists them.

## Design

1. **A fingerprint per server.** A hash of the server's configuration as it is serialised
   (`command`, `args`, `env`, `url`, `headers`, `categories`, `tools`, `expose`), plus, for a
   stdio server, the size and modification time of its command binary: the path itself when the
   command has a `/`, else the first match on `PATH`. Files named in `args` are not included:
   bizframe's `--app` file changes with every edit the model makes, and would force a probe every
   turn. Headers and env are hashed, never stored.
2. **A stamp per server.** The cache file keeps its `{ts, data}` shape and gains
   `servers: {name: {fp, ts}}`. A file without it, or a server missing from it, counts as stale.
3. **Probe what is stale, only that.** A configured server is served from the cache when its stamp
   matches its fingerprint and is younger than the TTL. Every other configured server is probed.
   A failed probe keeps that server's previous tools, whatever their fingerprint, and is retried
   after 60 s, as today. `force` probes them all.
4. **Serve only what is configured.** The manifest returned holds the configured servers only, so a
   removed server's tools are gone at once. Entries for servers this registry does not configure
   stay in the file (another session may use them) until they are a day old.

Stat calls are cheap: one `metadata` per stdio server per lookup, plus a `PATH` walk for a bare
command name.

Not covered: a server launched through an interpreter or launcher (`node server.js`,
`uvx bizframe`), whose code changes while the command binary stays the same; and a `url` server,
whose code rufous cannot see. Both refresh at the TTL, or sooner when their configuration changes.

## Tests

- A fresh cache is not probed again; a changed `args`, or a touched command binary, probes that
  server only.
- A server added under a fresh cache is probed; a removed one is not in the manifest.
- A failed probe of a changed server keeps its old tools and shortens its stamp.
- A cache file in the old `{ts, data}` shape is read, and every server in it counts as stale.
