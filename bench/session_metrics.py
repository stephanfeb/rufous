"""Process metrics for one live session's log (`~/.ollama_agent/sessions/<name>.log`).

The ACP server and the CLI write the loop's protocol lines there (bead boubou-bmq); this
prints the same counters `process_metrics.parse_transcript` computes for a bench cell, so a
Zed session is read with the bench's eyes. Usage:

    python3 bench/session_metrics.py ~/.ollama_agent/sessions/acp_1031_160446.log [--json]

Turn boundaries (`[turn] …` lines) are listed first; the counters cover the whole file.
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import process_metrics as pm  # noqa: E402

SHOW = ("tool_calls", "failed_calls", "nonzero_exits", "shell_errors", "bak_calls",
        "streak_notes", "idle_notes", "harness_notes", "budget_nudges",
        "loop_interceptions", "finish_refusals", "corrector_calls", "peak_prompt_tokens",
        "compaction_max_level", "prunes", "finish_accepted", "forced_end")


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) != 1:
        sys.exit(__doc__)
    text = open(args[0], encoding="utf-8", errors="replace").read()
    m = pm.parse_transcript(text)
    turns = [pm._strip(l) for l in text.splitlines() if "[turn] " in l]
    if "--json" in sys.argv:
        print(json.dumps({"turns": turns, "metrics": m}, ensure_ascii=False, indent=1))
        return
    for i, t in enumerate(turns, 1):
        print(f"turn {i}: {t[t.index('[turn] ') + 7:]}")
    for k in SHOW:
        if k in m:
            print(f"{k:>22}: {m[k]}")


if __name__ == "__main__":
    main()
