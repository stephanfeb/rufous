#!/usr/bin/env python3
"""Host-tool sweep (bead boubou-c9a, docs/host-tools.md "the cap").

Drives `rufous acp --policy general` the way Choreo does: a loopback MCP server
(`host_mcp.py`, N automations) passed in `session/new`, one fresh session per prompt. Per
model x N x run it records, for every prompt, which host tool ran (from the server's own
call log), whether the reply quotes what it returned, and the first model call's prompt
tokens and prefill seconds (the session log's `[timing: …]`).

Host prompts need one automation; plain prompts need none, and a host call there is a
distraction. N=0 runs without the server: the baseline for the plain prompts.

    python3 bench/host_tools.py --model qwen3.5:4b,gemma4:e2b --tools 0,5,10,15,25 --runs 3 \\
        --results bench/results/host_tools/2026-11-05
    python3 bench/host_tools.py --summary --results bench/results/host_tools/2026-11-05

The rig never starts Ollama itself: the user runs it.
"""
import argparse
import glob
import json
import os
import re
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from acp_convo import AcpClient, AcpError, run_turn, DEFAULT_BINARY, ROOT  # noqa: E402
from host_mcp import HostServer  # noqa: E402

# (id, prompt, the host tool it needs or None, fewest tools for the prompt to apply, check)
# check: "need" = that tool runs; "none" = no host call; "forbid" = that tool does not run;
# "soft" = recorded, never a failure (docs/host-tool-mentions.md, the bench table).
PROMPTS = [
    ("hello", 'Use the Choreo "Say Hello" automation to greet Ada, then tell me exactly what it returned.', "say_hello", 1, "need"),
    ("hello_q", "What did Say Hello return for Ada?", "say_hello", 1, "need"),
    ("weekly", "Run my weekly report automation.", "weekly_report", 2, "need"),
    ("tidy", "Can you tidy my downloads with the automation for that?", "tidy_downloads", 3, "need"),
    ("weekly_q", "Has Weekly Report run today?", "weekly_report", 2, "soft"),
    ("tidy_what", "What does Tidy Downloads do?", "tidy_downloads", 3, "soft"),
    ("dont", "Don't run Weekly Report, just tell me what it would make.", "weekly_report", 2, "forbid"),
    ("hi", "Hello!", None, 0, "none"),
    ("capital", "What is the capital of France?", None, 0, "none"),
    ("haiku", "Write a haiku about autumn.", None, 0, "none"),
    ("file", "Create a file ideas.md with three ideas for a birthday party.", None, 0, "none"),
]
# --arm: what AGENT_ABLATION switches off (bead boubou-8c4). The hold arm was removed after
# the 2026-11-05 reading (results/host_tools/2026-11-05_arm_*); `note` is the default build.
ARMS = {
    "none": "host_mention_note",
    "note": "",
}
TIMING = re.compile(r"\[timing: load [\d.]+s · prompt (\d+) tok ([\d.]+)s · gen (\d+) tok ([\d.]+)s\]")


def first_timing(home):
    logs = sorted(glob.glob(os.path.join(home, "sessions", "*.log")), key=os.path.getmtime)
    if not logs:
        return None
    turn = False
    for line in open(logs[-1], encoding="utf-8", errors="replace"):
        if "[turn] " in line:
            turn = True
        m = TIMING.search(line)
        if m and turn:
            return {"prompt_tok": int(m.group(1)), "prefill_s": float(m.group(2))}
    return None


def run_cell(binary, model, n_tools, ctx, cell_dir, turn_timeout, user_home, cap, arm):
    path = os.path.join(cell_dir, "score.json")
    if os.path.exists(path):
        return json.load(open(path))
    shutil.rmtree(cell_dir, ignore_errors=True)
    home = os.path.join(cell_dir, "home")
    work = os.path.join(cell_dir, "work")
    os.makedirs(os.path.join(home, "sessions"))
    os.makedirs(work)
    cfg = os.path.join(user_home, "config.json")
    if os.path.exists(cfg):
        shutil.copy(cfg, os.path.join(home, "config.json"))
    env = dict(os.environ, RUFOUS_HOME=home, RUFOUS_HOST_TOOL_CAP=str(cap),
               AGENT_ABLATION=ARMS[arm])
    env.pop("AGENT_UNATTENDED", None)
    server = HostServer(n_tools).start() if n_tools else None
    servers = [server.acp_entry()] if server else []
    cmd = [binary, "acp", "--policy", "general", "--model", model, "--ctx", str(ctx)]
    stderr = open(os.path.join(cell_dir, "stderr.log"), "w", encoding="utf-8")
    rows = []
    client = None
    try:
        client = AcpClient(cmd, env, stderr)
        client.request("initialize", {"protocolVersion": 1, "clientCapabilities": {
            "fs": {"readTextFile": False, "writeTextFile": False}}}, lambda m: None, 60)
        for pid, say, need, min_tools, check in PROMPTS:
            if need and n_tools < min_tools:
                continue
            ns = client.request("session/new", {"cwd": os.path.abspath(work), "mcpServers": servers},
                                lambda m: None, 60)
            rec = run_turn(client, ns["sessionId"], say, "allow", turn_timeout)
            calls = server.take_calls() if server else []
            ran = [c["tool"] for c in calls]
            hit = need in ran if need else None
            quoted = any(c["run"] in rec["reply"] for c in calls if c["tool"] == need) if need else None
            if check == "need":
                ok = hit and (quoted or need != "say_hello")
            elif check == "forbid":
                ok = not hit
            elif check == "soft":
                ok = True
            else:
                ok = not ran
            # A turn the engine or the session failed is no reading, never a pass.
            ok = ok and not rec["error"]
            rows.append({"prompt": pid, "need": need if check == "need" else None, "check": check, "ran": ran, "ok": ok, "hit": hit,
                         "quoted": quoted, "reply": rec["reply"][:400], "stop": rec["stop"],
                         "error": rec["error"], "wall_s": rec["wall_s"],
                         "timing": first_timing(home)})
    except (AcpError, TimeoutError, OSError) as e:
        rows.append({"prompt": "session", "error": str(e), "ok": False})
    finally:
        if client:
            client.close()
        if server:
            server.stop()
        stderr.close()
    score = {"model": model, "tools": n_tools, "arm": arm, "cell": os.path.basename(cell_dir), "rows": rows}
    json.dump(score, open(path, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    return score


def summary(results_dir):
    cells = [json.load(open(p)) for p in sorted(glob.glob(os.path.join(results_dir, "*", "score.json")))]
    groups = {}
    for c in cells:
        groups.setdefault((c["model"], c["tools"]), []).append(c)
    print(f"{'model':<14} {'N':>3} {'host ok':>9} {'plain ok':>9} {'quoted':>7} {'tok':>6} {'prefill s':>9} {'wall s':>7}")
    for (model, n), cs in sorted(groups.items()):
        rows = [r for c in cs for r in c["rows"] if r.get("prompt") != "session"]
        host = [r for r in rows if r.get("need")]
        plain = [r for r in rows if r.get("check", "none" if not r.get("need") else "need") == "none"]
        hello = [r for r in host if r["need"] == "say_hello"]
        tim = [r["timing"] for r in rows if r.get("timing")]
        frac = lambda rs: f"{sum(r['ok'] for r in rs)}/{len(rs)}" if rs else "-"
        avg = lambda xs: sum(xs) / len(xs) if xs else 0
        print(f"{model:<14} {n:>3} {frac(host):>9} {frac(plain):>9} "
              f"{(str(sum(bool(r['quoted']) for r in hello)) + '/' + str(len(hello))) if hello else '-':>7} "
              f"{avg([t['prompt_tok'] for t in tim]):>6.0f} {avg([t['prefill_s'] for t in tim]):>9.2f} "
              f"{avg([r['wall_s'] for r in rows]):>7.1f}")
    print("\nper prompt (ok/runs; soft prompts: host call/runs):")
    per = {}
    for c in cells:
        for r in c["rows"]:
            if r.get("prompt") == "session":
                continue
            k = (c["model"], c["tools"], r["prompt"])
            v = per.setdefault(k, [0, 0])
            v[0] += bool(r["ran"]) if r.get("check") == "soft" else bool(r["ok"])
            v[1] += 1
    for k in sorted(per):
        print("  ", k, "%d/%d" % tuple(per[k]))
    misses = [(c["model"], c["tools"], r["prompt"], r["ran"], r["reply"][:120])
              for c in cells for r in c["rows"] if not r.get("ok") and r.get("prompt") != "session"]
    if misses:
        print("\nmisses:")
        for m in misses:
            print("  ", m)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model", default="qwen3.5:4b")
    ap.add_argument("--tools", default="0,5,10,15,25", help="comma list of host tool counts")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--ctx", type=int, default=32000)
    ap.add_argument("--cap", type=int, default=1000, help="RUFOUS_HOST_TOOL_CAP for the run (inline all)")
    ap.add_argument("--results", default=os.path.join(ROOT, "bench", "results", "host_tools", "latest"))
    ap.add_argument("--binary", default=DEFAULT_BINARY)
    ap.add_argument("--turn-timeout-min", type=float, default=5)
    ap.add_argument("--user-home", default=os.path.expanduser("~/.ollama_agent"))
    ap.add_argument("--arm", default="note", choices=sorted(ARMS),
                    help="host mention note on (bead boubou-8c4); none = switched off")
    ap.add_argument("--summary", action="store_true")
    a = ap.parse_args()
    if a.summary:
        return summary(a.results)
    os.makedirs(a.results, exist_ok=True)
    for model in a.model.split(","):
        for run in range(1, a.runs + 1):
            for n in [int(x) for x in a.tools.split(",")]:
                cell = os.path.join(a.results, f"{model.replace(':', '_')}__n{n}__r{run}")
                t = time.monotonic()
                s = run_cell(a.binary, model, n, a.ctx, cell, a.turn_timeout_min * 60, a.user_home, a.cap,
                             a.arm)
                ok = sum(bool(r.get("ok")) for r in s["rows"])
                print(f"{model} N={n} run {run}: {ok}/{len(s['rows'])} ok ({time.monotonic() - t:.0f}s)", flush=True)
    summary(a.results)


if __name__ == "__main__":
    main()
