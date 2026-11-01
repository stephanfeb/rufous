#!/usr/bin/env python3
"""Turn a rufous session transcript into a replay corpus (bead boubou-bk9.13).

The tool calls a real model made are the test inputs the unit tests never had: a sed
replacement with a literal \\n, a `python -c` program with \\" quotes, a path that is a
directory. This reads `<name>.transcript.jsonl` from the sessions directory, takes the
assistant tool calls in an index range, rewrites the session's working directory to the
placeholder `{workdir}` and prints one JSON line per call:

    {"id": "<prefix>/<index>", "tool": "run_bash", "args": {...}, "expect": "as_written"}

`as_written` means `Executor::prepare` must hand the shell exactly that command, unblocked.
Hand-written entries in the same file may add `"fixture": {"files": {...}, "dirs": [...]}`
and an `expect` object (`starts_with`, `contains`, `not_contains`, `snapshots`) that is
checked through `run_tool`; `"expect": "blocked"` asserts the gate refuses the call.
`crates/rufous-kernel/tests/replay.rs` runs every `tests/replay/*.jsonl`.

    python3 -I bench/replay_extract.py ~/.ollama_agent/sessions/acp_1030_094413.transcript.jsonl \\
        --from 377 --workdir /Users/me/proj --prefix zed-2026-10-31 \\
        >> crates/rufous-kernel/tests/replay/zed_2026-10-31.jsonl
"""
import argparse
import json
import sys


def rewrite(value, workdir):
    if isinstance(value, str):
        return value.replace(workdir, "{workdir}") if workdir else value
    if isinstance(value, dict):
        return {k: rewrite(v, workdir) for k, v in value.items()}
    if isinstance(value, list):
        return [rewrite(v, workdir) for v in value]
    return value


def extract(lines, start, end, workdir, prefix, tools, max_chars):
    out, skipped = [], []
    for i, line in enumerate(lines):
        if i < start or (end is not None and i > end):
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if entry.get("role") != "assistant":
            continue
        for call in entry.get("tool_calls") or []:
            fn = call.get("function") or {}
            name = fn.get("name")
            if name not in tools:
                continue
            args = rewrite(fn.get("arguments") or {}, workdir)
            if len(json.dumps(args)) > max_chars:
                skipped.append((i, name, len(json.dumps(args))))
                continue
            out.append({"id": f"{prefix}/{i}", "tool": name, "args": args, "expect": "as_written"})
    return out, skipped


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("transcript")
    ap.add_argument("--from", dest="start", type=int, default=0, help="first transcript line index")
    ap.add_argument("--to", dest="end", type=int, default=None, help="last transcript line index")
    ap.add_argument("--workdir", default="", help="the session's working directory, rewritten to {workdir}")
    ap.add_argument("--prefix", default="session", help="id prefix")
    ap.add_argument("--tools", default="run_bash", help="comma-separated tool names to take")
    ap.add_argument("--max-chars", type=int, default=4000, help="skip calls whose arguments are longer")
    a = ap.parse_args(argv)
    with open(a.transcript, encoding="utf-8") as f:
        lines = f.read().splitlines()
    out, skipped = extract(lines, a.start, a.end, a.workdir.rstrip("/"), a.prefix,
                           set(a.tools.split(",")), a.max_chars)
    for e in out:
        print(json.dumps(e, ensure_ascii=False))
    for i, name, n in skipped:
        print(f"skipped {i} {name}: {n} chars of arguments", file=sys.stderr)
    print(f"{len(out)} entries", file=sys.stderr)


if __name__ == "__main__":
    main()
