#!/usr/bin/env python3
"""Scripted multi-turn conversations over ACP (bead boubou-6gq).

Drives `boubou acp` the way an editor does: newline-delimited JSON-RPC over stdio,
`initialize`, `session/new`, one `session/prompt` per turn, `session/update` notifications
collected, `session/request_permission` answered from the script (default deny, every ask
recorded). No `fs` capability is advertised, so the agent's file tools use the disk.

One cell is model x script x run, in its own folder: `home/` is BOUBOU_HOME (the user's
`config.json` copied in, so hosts and profiles apply; the session's files and its `.log`
land in `home/sessions/`), `work/` is a fresh copy of the fixture. Per turn the rig records
the agent's text, the tool calls the session log lists between `[turn]` markers, the
permission asks and the stop reason, then checks the script's expectations.

    python3 bench/acp_convo.py --model qwen3.5:4b,gemma4:e4b --ctx 32000 --runs 3 \\
        --results bench/results/convo/4b_2026-10-31
    python3 bench/acp_convo.py --summary --results bench/results/convo/4b_2026-10-31

Scripts are `bench/convo/*.json`; see `bench/convo/README.md` for the expectation keys.
The rig never starts Ollama itself: the user runs it.
"""
import argparse
import glob
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SCRIPTS_DIR = os.path.join(HERE, "convo")
FIXTURE_ROOTS = [os.path.join(HERE, "convo", "fixtures"), os.path.join(HERE, "lrab_ext", "fixtures")]
DEFAULT_BINARY = os.path.join(ROOT, "target", "release", "boubou")

SOURCE_EXT = {"rs", "py", "dart", "js", "jsx", "ts", "tsx", "mjs", "cjs", "go", "java", "kt",
              "kts", "swift", "c", "cc", "cpp", "cxx", "h", "hpp", "cs", "rb", "php", "sh",
              "bash", "zsh", "sql", "scala", "ex", "exs", "erl", "hs", "lua", "pl", "r", "m",
              "mm", "vue", "svelte"}
WRITE_TOOLS = {"create_file", "edit_file", "append_file"}
STAMP = re.compile(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} ")
CALL = re.compile(r"^\[(\d+)\|\+(\d+)s\] ⚙ (\S+) (.*?)(?: -> (.*))?$")
PATH_IN_ARGS = re.compile(r'"path"\s*:\s*"([^"]*)"')


# ---------------------------------------------------------------- ACP client

class AcpError(Exception):
    pass


class AcpClient:
    """One agent process; requests block, notifications and agent requests queue up."""

    def __init__(self, cmd, env, log_stderr):
        self.proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=log_stderr, env=env, text=True, bufsize=1)
        self.inbox = queue.Queue()
        self.next_id = 1
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                self.inbox.put(json.loads(line))
            except json.JSONDecodeError:
                self.inbox.put({"_garbage": line})
        self.inbox.put(None)

    def send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params, on_message, timeout):
        """Send a request; feed everything else to `on_message` until our reply arrives.
        `on_message(msg)` may return a result object for an agent request (it is answered)."""
        rid = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deadline = time.monotonic() + timeout
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                raise TimeoutError(method)
            try:
                msg = self.inbox.get(timeout=min(left, 1.0))
            except queue.Empty:
                continue
            if msg is None:
                raise AcpError("agent closed its stdout")
            if msg.get("id") == rid and ("result" in msg or "error" in msg):
                if "error" in msg:
                    raise AcpError(json.dumps(msg["error"]))
                return msg["result"]
            self._dispatch(msg, on_message)

    def _dispatch(self, msg, on_message):
        if "method" in msg and "id" in msg:  # agent -> client request
            result = on_message(msg)
            if result is None:
                result = {}
            self.send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
        else:
            on_message(msg)

    def drain(self, on_message, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                msg = self.inbox.get(timeout=0.2)
            except queue.Empty:
                continue
            if msg is None:
                return
            self._dispatch(msg, on_message)

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


# ---------------------------------------------------------------- one turn

def run_turn(client, session_id, say, permission, timeout):
    """Returns the turn record: text, asks, updates count, stop reason, wall seconds."""
    text = []
    asks = []
    updates = 0
    notices = []

    def on_message(msg):
        nonlocal updates
        method = msg.get("method")
        if method == "session/update":
            updates += 1
            upd = (msg.get("params") or {}).get("update") or {}
            kind = upd.get("sessionUpdate")
            if kind == "agent_message_chunk":
                c = upd.get("content") or {}
                if c.get("type") == "text":
                    text.append(c.get("text", ""))
            elif kind == "notice" or kind == "usage_update":
                notices.append(upd)
            return None
        if method == "session/request_permission":
            p = msg.get("params") or {}
            tc = p.get("toolCall") or {}
            asks.append({"title": tc.get("title"), "rawInput": tc.get("rawInput"),
                         "answer": permission})
            options = {o.get("optionId") for o in p.get("options") or []}
            option = permission if permission in options else ("deny" if "deny" in options else next(iter(options), "deny"))
            return {"outcome": {"outcome": "selected", "optionId": option}}
        return None

    t0 = time.monotonic()
    stop = None
    error = None
    try:
        result = client.request("session/prompt",
                                {"sessionId": session_id, "prompt": [{"type": "text", "text": say}]},
                                on_message, timeout)
        stop = result.get("stopReason")
    except TimeoutError:
        error = "timeout"
        client.notify("session/cancel", {"sessionId": session_id})
        client.drain(on_message, 30)
        stop = "cancelled_by_rig"
    except AcpError as e:
        error = str(e)
    wall = time.monotonic() - t0
    return {"say": say, "reply": "".join(text), "asks": asks, "updates": updates,
            "stop": stop, "error": error, "wall_s": round(wall, 1), "notices": notices}


# ---------------------------------------------------------------- session log

def session_log_turns(home):
    """The tool calls per turn from `home/sessions/*.log`, split on `[turn]` markers.
    Returns a list (one entry per turn) of {"calls": [{"tool", "args"}], "lines": n}."""
    logs = sorted(glob.glob(os.path.join(home, "sessions", "*.log")))
    turns = []
    if not logs:
        return turns
    cur = None
    for raw in open(logs[-1], encoding="utf-8", errors="replace"):
        line = STAMP.sub("", raw.rstrip("\n"))
        if line.startswith("[turn] "):
            cur = {"calls": [], "lines": 0, "ctx_peak": 0, "compactions": 0, "scope_class": None}
            turns.append(cur)
            continue
        if cur is None:
            continue
        cur["lines"] += 1
        m = CALL.match(line)
        if m:
            result, status = split_status(m.group(5) or "")
            cur["calls"].append({"tool": m.group(3), "args": m.group(4), "result": result, "status": status})
        m = re.search(r"\[ctx: (\d+)/(\d+)", line)
        if m:
            cur["ctx_peak"] = max(cur["ctx_peak"], int(m.group(1)))
        if line.startswith("[context compaction") or "上下文压缩" in line:
            cur["compactions"] += 1
        m = re.match(r"\[scope\] class=(\w+)", line)
        if m:
            cur["scope_class"] = m.group(1)
    return turns


def is_source_path(p):
    return p.rsplit(".", 1)[-1].lower() in SOURCE_EXT if "." in os.path.basename(p) else False


# Result openings of a refused or failed call, for logs written before the status tag
# (bead boubou-bj4); a tagged log says `⟂refused:<kind>` and the prefixes are not consulted.
REFUSED = ("[scope gate", "[safety gate", "[blocked", "[tool error", "[child safety gate")


def split_status(result):
    """`<result head> ⟂<tag>` → (head, tag); an untagged result → (result, "")."""
    if " ⟂" in result:
        head, tag = result.rsplit(" ⟂", 1)
        return head, tag.strip()
    return result, ""


def did_not_land(call):
    """The call was refused or failed: by its status tag when the log has one, else by the
    result's opening."""
    status = call.get("status", "")
    if status:
        return status != "nochange" and status != ""
    return call["result"].startswith(REFUSED)


def source_writes(calls):
    """Source-file writes that happened: a call the gate refused is an attempt, counted under
    `asks` when it was asked about, not a write."""
    out = []
    for c in calls:
        if c["tool"] in WRITE_TOOLS and not did_not_land(c):
            m = PATH_IN_ARGS.search(c["args"])
            if m and is_source_path(m.group(1)):
                out.append(m.group(1))
    return out


# ---------------------------------------------------------------- checks

# A turn whose instruction says this must leave every earlier section standing (bead
# boubou-dby): the words are the script's own, under bench/convo, so matching them scores the
# fixture, never the model.
KEEP_WORDS = "Change nothing else"


def passed_file_checks(turn, fails):
    """The (file, regex) pairs of this turn's `file_regex` that its `fails` do not name."""
    out = set()
    for key, rx in ((turn.get("expect") or {}).get("file_regex") or {}).items():
        if f"{key} does not match /{rx}/" not in fails:
            out.add((key.split("#", 1)[0], rx))
    return out


def carried_checks(turns, i, passed=None):
    """The `file_regex` pairs of turns before `i` that turn `i` must keep satisfied, or `{}`.

    Only for a turn whose `say` has KEEP_WORDS. Keyed `<file>#t<n>` (n = the turn that set the
    check; `body_of` reads the file before `#`), since every section check of one file shares
    the plain key. A (file, regex) pair the turn checks itself is left to it; a pair several
    turns set (the title) is carried once. `passed` is the set of pairs that held at their own
    turn: a section never written is not "lost" later (bead boubou-f1x); `None` carries all."""
    say = turns[i].get("say", "")
    if KEEP_WORDS.lower() not in say.lower():
        return {}
    own = {(k.split("#", 1)[0], rx)
           for k, rx in ((turns[i].get("expect") or {}).get("file_regex") or {}).items()}
    keep, seen = {}, set()
    for n, t in enumerate(turns[:i], 1):
        for key, rx in ((t.get("expect") or {}).get("file_regex") or {}).items():
            pair = (key.split("#", 1)[0], rx)
            if pair in own or pair in seen or (passed is not None and pair not in passed):
                continue
            seen.add(pair)
            keep[f"{pair[0]}#t{n}"] = rx
    return keep


def check_turn(expect, turn, log_turn, workdir):
    """Returns the list of failed expectations (empty = pass)."""
    fails = []
    reply = turn["reply"]
    calls = (log_turn or {}).get("calls", [])
    if turn.get("error"):
        fails.append(f"turn error: {turn['error']}")
    if "stop_reason" in expect and turn.get("stop") != expect["stop_reason"]:
        fails.append(f"stop {turn.get('stop')!r} != {expect['stop_reason']!r}")
    rxs = expect.get("reply_regex", [])
    for rx in ([rxs] if isinstance(rxs, str) else rxs):
        if not re.search(rx, reply, re.S | re.I):
            fails.append(f"reply does not match /{rx}/")
    if "reply_not_regex" in expect and re.search(expect["reply_not_regex"], reply, re.S | re.I):
        fails.append(f"reply matches forbidden /{expect['reply_not_regex']}/")
    if "reply_max_lines" in expect:
        n = len([l for l in reply.strip().splitlines() if l.strip()])
        if n > expect["reply_max_lines"]:
            fails.append(f"reply has {n} lines > {expect['reply_max_lines']}")
    if expect.get("reply_nonempty") and not reply.strip():
        fails.append("empty reply")
    def where(rel):
        return os.path.expanduser(rel) if rel.startswith(("/", "~")) else os.path.join(workdir, rel)

    for rel in expect.get("files_exist", []):
        if not os.path.exists(where(rel)):
            fails.append(f"missing {rel}")
    for rel in expect.get("files_absent", []):
        if os.path.exists(where(rel)):
            fails.append(f"present {rel}")
    def body_of(key):
        # `path#tag` lets one file carry several regexes
        p = where(key.split("#", 1)[0])
        return open(p, encoding="utf-8", errors="replace").read() if os.path.exists(p) else ""

    for key, rx in (expect.get("file_regex") or {}).items():
        if not re.search(rx, body_of(key), re.S | re.M):
            fails.append(f"{key} does not match /{rx}/")
    for key, rx in (expect.get("file_not_regex") or {}).items():
        if re.search(rx, body_of(key), re.S | re.M):
            fails.append(f"{key} matches forbidden /{rx}/")
    for key, rx in (expect.get("keep_file_regex") or {}).items():
        if not re.search(rx, body_of(key), re.S | re.M):
            fails.append(f"earlier section lost: {key} /{rx}/")
    if expect.get("no_source_writes"):
        sw = source_writes(calls)
        if sw:
            fails.append(f"source writes: {sw}")
    for tool in expect.get("no_tool", []):
        if any(c["tool"] == tool for c in calls):
            fails.append(f"used {tool}")
    if "max_calls" in expect and len(calls) > expect["max_calls"]:
        fails.append(f"{len(calls)} calls > {expect['max_calls']}")
    if "permission_asked" in expect and bool(turn["asks"]) != bool(expect["permission_asked"]):
        fails.append(f"permission asked={bool(turn['asks'])}, expected {expect['permission_asked']}")
    if "asked_target_regex" in expect:
        # At least one ask in the turn was about this call: `<tool> <target>` from the
        # dialog's rawInput (bead boubou-zjy: a pass from an unrelated ask hid an unasked write).
        rx = expect["asked_target_regex"]
        shown = [f"{(a.get('rawInput') or {}).get('tool', '?')} {(a.get('rawInput') or {}).get('target', a.get('title', ''))}"
                 for a in turn["asks"]]
        if not any(re.search(rx, s_) for s_ in shown):
            fails.append(f"no ask matches /{rx}/ (asks: {shown})")
    return fails


# ---------------------------------------------------------------- cells

def find_fixture(name):
    for root in FIXTURE_ROOTS:
        p = os.path.join(root, name)
        if os.path.isdir(p):
            return p
    raise SystemExit(f"fixture {name!r} not found under {FIXTURE_ROOTS}")


def load_scripts(only):
    out = []
    for p in sorted(glob.glob(os.path.join(SCRIPTS_DIR, "*.json"))):
        s = json.load(open(p, encoding="utf-8"))
        if only and s["id"] not in only:
            continue
        out.append(s)
    return out


def run_cell(binary, model, ctx, script, cell_dir, turn_timeout, user_home):
    if os.path.exists(os.path.join(cell_dir, "score.json")):
        return json.load(open(os.path.join(cell_dir, "score.json")))
    shutil.rmtree(cell_dir, ignore_errors=True)
    home = os.path.join(cell_dir, "home")
    work = os.path.join(cell_dir, "work")
    os.makedirs(os.path.join(home, "sessions"))
    cfg = os.path.join(user_home, "config.json")
    if os.path.exists(cfg):
        shutil.copy(cfg, os.path.join(home, "config.json"))
    shutil.copytree(find_fixture(script["fixture"]), work)
    for rel in script.get("absent_before", []):
        p = os.path.expanduser(rel)
        if os.path.exists(p):
            os.remove(p)
    env = dict(os.environ, BOUBOU_HOME=home)
    env.pop("AGENT_UNATTENDED", None)
    cmd = [binary, "acp", "--model", model, "--ctx", str(script.get("ctx") or ctx)]
    stderr = open(os.path.join(cell_dir, "stderr.log"), "w", encoding="utf-8")
    t_cell = time.monotonic()
    turns = []
    client = None
    try:
        client = AcpClient(cmd, env, stderr)
        client.request("initialize", {"protocolVersion": 1, "clientCapabilities": {
            "fs": {"readTextFile": False, "writeTextFile": False}}}, lambda m: None, 60)
        ns = client.request("session/new", {"cwd": os.path.abspath(work), "mcpServers": []},
                            lambda m: None, 60)
        sid = ns["sessionId"]
        passed = set()
        for i, t in enumerate(script["turns"]):
            rec = run_turn(client, sid, t["say"], t.get("permission", "deny"), turn_timeout)
            # Checked now, not at the end: a later turn may create or change the files this
            # turn's expectations are about.
            lts = session_log_turns(home)
            rec["log"] = lts[i] if i < len(lts) else None
            expect = dict(t.get("expect", {}))
            keep = carried_checks(script["turns"], i, passed)
            if keep:
                expect["keep_file_regex"] = keep
            rec["fails"] = check_turn(expect, rec, rec["log"], work)
            passed |= passed_file_checks(t, rec["fails"])
            turns.append(rec)
            if rec.get("error") and rec["error"] != "timeout":
                break
    except (AcpError, TimeoutError, OSError) as e:
        turns.append({"say": "", "reply": "", "asks": [], "updates": 0, "stop": None,
                      "error": f"session: {e}", "wall_s": 0, "notices": []})
    finally:
        if client:
            client.close()
        stderr.close()
    results = []
    for i, t in enumerate(script["turns"]):
        rec = turns[i] if i < len(turns) else {"say": t["say"], "reply": "", "asks": [], "updates": 0,
                                               "stop": None, "error": "not reached", "wall_s": 0,
                                               "log": None, "fails": ["not reached"]}
        lt = rec.get("log")
        fails = rec.get("fails", [])
        results.append({"turn": i + 1, "say": t["say"], "pass": not fails, "fails": fails,
                        "reply": rec["reply"], "asks": rec["asks"], "stop": rec["stop"],
                        "wall_s": rec["wall_s"], "calls": len((lt or {}).get("calls", [])),
                        "source_writes": source_writes((lt or {}).get("calls", [])),
                        "ctx_peak": (lt or {}).get("ctx_peak", 0),
                        "compactions": (lt or {}).get("compactions", 0),
                        "scope_class": (lt or {}).get("scope_class")})
    guarded = [r for r in results if r["scope_class"] in ("tracker", "docs")]
    score = {
        "model": model, "script": script["id"], "cell": os.path.basename(cell_dir),
        "turns": len(results), "passed": sum(r["pass"] for r in results),
        "asks": sum(len(r["asks"]) for r in results),
        "calls": sum(r["calls"] for r in results),
        "guarded_source_writes": sum(len(r["source_writes"]) for r in guarded),
        "compactions": sum(r["compactions"] for r in results),
        "ctx_peak": max([r["ctx_peak"] for r in results] or [0]),
        "wall_s": round(time.monotonic() - t_cell, 1),
        "turn_results": results,
    }
    with open(os.path.join(cell_dir, "turns.jsonl"), "w", encoding="utf-8") as f:
        for r in results:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    json.dump(score, open(os.path.join(cell_dir, "score.json"), "w", encoding="utf-8"),
              ensure_ascii=False, indent=1)
    return score


# ---------------------------------------------------------------- summary

def summary(results_dir):
    rows = []
    for sp in sorted(glob.glob(os.path.join(results_dir, "*", "*", "score.json"))):
        rows.append(json.load(open(sp, encoding="utf-8")))
    if not rows:
        print("no cells under", results_dir)
        return
    print(f"{'model':<14} {'script':<16} {'run':>3} {'turns':>7} {'asks':>4} {'calls':>5} "
          f"{'gsw':>3} {'compact':>7} {'peak':>6} {'wall':>6}  failures")
    for r in rows:
        run = r["cell"].rsplit("_r", 1)[-1]
        fails = "; ".join(f"t{t['turn']}: {f}" for t in r["turn_results"] for f in t["fails"])
        print(f"{r['model']:<14} {r['script']:<16} {run:>3} {r['passed']:>3}/{r['turns']:<3} "
              f"{r['asks']:>4} {r['calls']:>5} {r['guarded_source_writes']:>3} {r['compactions']:>7} "
              f"{r['ctx_peak']:>6} {r['wall_s']:>6.0f}  {fails[:120]}")
    # per model x script mean over runs
    by = {}
    for r in rows:
        by.setdefault((r["model"], r["script"]), []).append(r["passed"] / max(r["turns"], 1))
    print("\nmean turn pass rate per model x script (n runs):")
    for (m, s), v in sorted(by.items()):
        print(f"  {m:<14} {s:<16} {sum(v)/len(v):.2f}  n={len(v)}")
    print("\ngsw = source-file writes in turns the scope gate classed as tracker or docs; "
          "asks = permission requests the rig answered.")


def rescore(results_dir):
    """Lost sections in finished cells: every KEEP_WORDS turn's carried checks against the
    final `work/` folder (the cells keep it). Prints, never rewrites score.json."""
    scripts = {s["id"]: s for s in load_scripts([])}
    lost_cells = 0
    rows = sorted(glob.glob(os.path.join(results_dir, "*", "*", "score.json")))
    for sp in rows:
        cell = os.path.dirname(sp)
        sc = json.load(open(sp, encoding="utf-8"))
        script = scripts.get(sc["script"])
        work = os.path.join(cell, "work")
        if not script or not os.path.isdir(work):
            continue
        lost = {}
        # The per-turn results the cell kept: a check that failed at its turn is not carried.
        fails_by_turn = {}
        tj = os.path.join(cell, "turns.jsonl")
        if os.path.exists(tj):
            for line in open(tj, encoding="utf-8"):
                r = json.loads(line)
                fails_by_turn[r["turn"]] = r.get("fails", [])
        passed = set()
        for i in range(len(script["turns"])):
            keep = carried_checks(script["turns"], i, passed)
            passed |= passed_file_checks(script["turns"][i], fails_by_turn.get(i + 1, []))
            if keep:
                for f in check_turn({"keep_file_regex": keep}, {"reply": "", "asks": [], "stop": None,
                                                                 "error": None}, None, work):
                    lost[f] = i + 1
        if lost:
            lost_cells += 1
            print(f"{sc['model']} {sc['cell']}: " + "; ".join(f"{f} (by t{i})" for f, i in lost.items()))
    print(f"{lost_cells} of {len(rows)} cells lost an earlier section")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model", default="qwen3.5:4b", help="model tag, or a comma list")
    ap.add_argument("--scripts", default="", help="comma list of script ids (default all)")
    ap.add_argument("--ctx", type=int, default=32000, help="context window unless the script sets one")
    ap.add_argument("--runs", type=int, default=1)
    ap.add_argument("--results", default=os.path.join(ROOT, "bench", "results", "convo", "latest"))
    ap.add_argument("--binary", default=DEFAULT_BINARY)
    ap.add_argument("--turn-timeout-min", type=float, default=10)
    ap.add_argument("--user-home", default=os.path.expanduser("~/.ollama_agent"))
    ap.add_argument("--summary", action="store_true", help="only print the table for --results")
    ap.add_argument("--rescore", action="store_true",
                    help="lost earlier sections in the finished cells under --results (final files)")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()
    if a.summary:
        summary(a.results)
        return
    if a.rescore:
        rescore(a.results)
        return
    only = [s for s in a.scripts.split(",") if s]
    scripts = load_scripts(only)
    if not scripts:
        sys.exit("no scripts matched")
    for model in [m for m in a.model.split(",") if m]:
        for s in scripts:
            for r in range(a.runs):
                cell = os.path.join(a.results, model.replace(":", "_"), f"{s['id']}_r{r}")
                if a.dry_run:
                    print(f"would run {model} {s['id']} run {r} -> {cell}")
                    continue
                sc = run_cell(a.binary, model, a.ctx, s, cell, a.turn_timeout_min * 60, a.user_home)
                print(f"{model} {s['id']} r{r}: {sc['passed']}/{sc['turns']} turns, "
                      f"{sc['asks']} asks, {sc['calls']} calls, {sc['wall_s']:.0f}s")
    if not a.dry_run:
        print()
        summary(a.results)


if __name__ == "__main__":
    main()
