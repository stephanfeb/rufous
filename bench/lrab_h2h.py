#!/usr/bin/env python3
"""LRAB head-to-head: Mingbird (Python) vs Boubou (this Rust port) on Mingbird's benchmark.

Mingbird is a third-party project and is never modified: its runner is invoked as-is for the
`mingbird` arm, its tasks/fixtures are read in place, and its scorer (`score_task`) is imported
read-only as the single referee for both arms. The `boubou` arm is driven here with the same
workdir preparation, env knobs, kill-resume protocol and failure-mode classification as
Mingbird's `_cmd_mingbird`, so the two arms differ only in the runtime under test.

Usage (the mingbird arm needs `requests` and `beautifulsoup4` in the interpreter):
  uv run --with requests --with beautifulsoup4 python bench/lrab_h2h.py --model gemma4:e2b
  python bench/lrab_h2h.py --arms boubou --tasks WF-01,WF-07 --timeout-min 20
  python bench/lrab_h2h.py --summary            # table from an existing results dir

Env: MINGBIRD_ROOT (default: sibling `../Mingbird-agent`), BOUBOU_BIN (default: PATH, then
this checkout's target/release then target/debug), AGENT_CTX / AGENT_SLIM_OUTPUT pass through.
"""
import argparse
import glob
import json
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BOUBOU_ROOT = os.path.dirname(HERE)
MINGBIRD_ROOT = os.environ.get("MINGBIRD_ROOT") or os.path.join(
    os.path.dirname(BOUBOU_ROOT), "Mingbird-agent")
LRAB = os.path.join(MINGBIRD_ROOT, "bench", "lrab")
BENCH_HOME = os.path.expanduser("~/.mingbird_bench")   # same isolated home as the mingbird arm
TIERS = ["tier1_retrieval", "tier2_synthesis", "tier3_workflow", "tier4_longhorizon"]


# ---------------------------------------------------------------- boubou arm
def boubou_exe():
    cands = []
    if os.environ.get("BOUBOU_BIN"):
        cands.append(os.environ["BOUBOU_BIN"])
    on_path = shutil.which("boubou")
    if on_path:
        cands.append(on_path)
    exe = "boubou.exe" if os.name == "nt" else "boubou"
    cands += [os.path.join(BOUBOU_ROOT, "target", "release", exe),
              os.path.join(BOUBOU_ROOT, "target", "debug", exe)]
    for c in cands:
        if os.path.isfile(c):
            return c
    raise FileNotFoundError("boubou binary not found; set BOUBOU_BIN or run "
                            "`cargo build --release -p boubou-cli` (tried: %s)" % ", ".join(cands))


def prepare_workdir(task, workdir):
    """Mirror of run_bench.prepare_workdir: fixtures copied in, prompt in task_input.txt."""
    shutil.rmtree(workdir, ignore_errors=True)
    os.makedirs(workdir, exist_ok=True)
    for rel in task.get("fixtures", []):
        src = os.path.join(LRAB, rel)
        if os.path.isfile(src):
            shutil.copy2(src, workdir)
        elif os.path.isdir(src):
            shutil.copytree(src, os.path.join(workdir, os.path.basename(src)), dirs_exist_ok=True)
    with open(os.path.join(workdir, "task_input.txt"), "w", encoding="utf-8") as f:
        f.write(task["prompt"])


def boubou_cmd(workdir, model):
    """Same contract and env as Mingbird's `_cmd_mingbird`; no --new so phase 2 of a
    kill-resume run continues from .agent_state.json."""
    env = dict(os.environ)
    env.pop("AGENT_STREAM", None)
    env["BOUBOU_HOME"] = BENCH_HOME
    env["MINGBIRD_HOME"] = BENCH_HOME
    env["AGENT_CTX"] = os.environ.get("AGENT_CTX", "32768")
    env["AGENT_SLIM_OUTPUT"] = os.environ.get("AGENT_SLIM_OUTPUT", "0")
    env["AGENT_UNATTENDED"] = "1"
    env.setdefault("PYTHONUTF8", "1")
    return [boubou_exe(), "run", model, os.path.join(workdir, "task_input.txt"), workdir], env


def _tree_kill(p):
    if p.poll() is None:
        try:
            if os.name == "nt":
                subprocess.run(["taskkill", "/F", "/T", "/PID", str(p.pid)],
                               capture_output=True, timeout=20)
            else:
                os.killpg(os.getpgid(p.pid), 9)
        except Exception:
            try:
                p.kill()
            except Exception:
                pass


def _spawn(cmd, env, cwd):
    kw = {"stdout": subprocess.PIPE, "stderr": subprocess.PIPE, "text": True,
          "encoding": "utf-8", "errors": "replace", "env": env, "cwd": cwd}
    if os.name != "nt":
        kw["start_new_session"] = True   # own process group so the whole tree can be killed
    return subprocess.Popen(cmd, **kw)


def _run_phase(cmd, env, cwd, timeout_s):
    t0 = time.time()
    p = _spawn(cmd, env, cwd)
    try:
        out, err = p.communicate(timeout=timeout_s)
        return p.returncode, out or "", err or "", time.time() - t0, False
    except subprocess.TimeoutExpired:
        _tree_kill(p)
        out, err = p.communicate()
        return -9, out or "", err or "", time.time() - t0, True


def run_boubou(task, workdir, model, timeout_min, kill_at_pct):
    """Returns (rc, stdout, stderr, wall, meta). meta mirrors run_bench's resume dict."""
    cmd, env = boubou_cmd(workdir, model)
    budget = timeout_min * 60
    if not kill_at_pct:
        rc, out, err, wall, timed_out = _run_phase(cmd, env, workdir, budget)
        return rc, out, err, wall, ({"protocol": "single", "timed_out": True} if timed_out else None)
    kill_s = budget * kill_at_pct / 100.0
    meta = {"protocol": "kill_resume", "kill_at_pct": kill_at_pct,
            "kill_at_seconds": round(kill_s, 1), "budget_seconds": budget, "killed": False}
    rc, out1, err1, w1, killed = _run_phase(cmd, env, workdir, kill_s)
    meta["attempt1_wall"] = round(w1, 1)
    meta["attempt1_killed"] = killed
    if not killed:
        return rc, out1, err1, w1, meta
    meta["killed"] = True
    print(f"[kill-resume] phase1 killed at {kill_s:.0f}s, relaunching same command", flush=True)
    rc2, out2, err2, w2, timed_out = _run_phase(cmd, env, workdir, budget - kill_s)
    meta["attempt2_wall"] = round(w2, 1)
    if timed_out:
        meta["phase2_timeout"] = True
    return (rc2, out1 + "\n--- KILLED AT kill_at_pct, RELAUNCHED ---\n" + out2,
            err1 + err2, w1 + w2, meta)


def wait_for_quiet(workdir, quiet_secs=20, max_wait=180):
    def state():
        st = []
        for root, _, files in os.walk(workdir):
            for f in files:
                try:
                    p = os.path.join(root, f)
                    st.append((p, os.path.getsize(p)))
                except OSError:
                    pass
        return sorted(st)
    deadline = time.time() + max_wait
    last, since = state(), time.time()
    while time.time() < deadline:
        time.sleep(5)
        cur = state()
        if cur != last:
            last, since = cur, time.time()
        elif time.time() - since >= quiet_secs:
            return True
    return False


# Mingbird's "resets had no effect, exiting gracefully" log line (Chinese, written as escapes).
MINGBIRD_EMPTY_SPIN = "\u590d\u4f4d\u65e0\u6548,\u4f18\u96c5\u9000\u51fa"


def classify(rc, wall, transcript, score, workdir):
    """run_bench's failure-mode ladder, applied to the boubou arm verbatim."""
    if rc != 0:
        mode = "crash" if wall < 60 else "error"
        if mode == "crash" and "UnicodeEncodeError" in transcript:
            mode = "gbk_degraded"
        elif MINGBIRD_EMPTY_SPIN in transcript or "3 resets had no effect" in transcript:
            mode = "empty_spin"
        return mode
    if score.get("total", 0) == 0:
        artifacts = [f for f in os.listdir(workdir)
                     if f not in ("task_input.txt", "aging_data.csv")
                     and not f.startswith(".") and f != "__pycache__"]
        plan_only = artifacts and all(f in ("plan.md", "todo.json") for f in artifacts)
        if not artifacts:
            if "task_input" in transcript and "Read" in transcript:
                return "stall"
            return "early_finish" if wall < 30 else "no_artifacts"
        if plan_only:
            return "early_finish"
    return "completed"


def score_with_mingbird(task_path, workdir, judge):
    sys.path.insert(0, os.path.join(LRAB, "scoring"))
    from score_task import score_task   # Mingbird's referee, imported read-only
    return score_task(task_path, workdir, judge=bool(judge), judge_model=judge or "ornith-1.5:35b")


def run_boubou_arm(task_path, task, model, out_dir, run_id, timeout_min, kill_at_pct, judge):
    workdir = os.path.join(out_dir, "workdir")
    prepare_workdir(task, workdir)
    rc, out, err, wall, meta = run_boubou(task, workdir, model, timeout_min, kill_at_pct)
    with open(os.path.join(out_dir, "transcript.txt"), "w", encoding="utf-8") as f:
        f.write(out)
        if err:
            f.write("\n--- STDERR ---\n" + err)
    base = {"task_id": task["id"], "agent": "boubou", "model": model,
            "wall_seconds": round(wall, 1), "exit_code": rc, "run_id": run_id}
    if meta and (meta.get("timed_out") or meta.get("phase2_timeout")):
        score = dict(base, failure_mode="timeout", resume=meta,
                     failure_note=f"agent exceeded {timeout_min} min wall budget")
    else:
        wait_for_quiet(workdir)
        score = score_with_mingbird(task_path, workdir, judge)
        score.update(base)
        score["failure_mode"] = classify(rc, wall, out + err, score, workdir)
        if meta:
            score["resume"] = meta
    with open(os.path.join(out_dir, "score.json"), "w", encoding="utf-8") as f:
        json.dump(score, f, ensure_ascii=False, indent=2)
    return score


# -------------------------------------------------------------- mingbird arm
def run_mingbird_arm(task_path, model, results, run_id, timeout_min, kill_at_pct, judge, log):
    """Mingbird's own runner, unmodified. It writes <results>/<run_id>/score.json itself."""
    cmd = [sys.executable, os.path.join(LRAB, "runners", "run_bench.py"), "--agent", "mingbird",
           "--task", task_path, "--model", model, "--results", results,
           "--timeout-min", str(timeout_min), "--run-id", run_id]
    if kill_at_pct:
        cmd += ["--kill-at-pct", str(kill_at_pct)]
    if judge:
        cmd += ["--judge", judge]
    subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, cwd=MINGBIRD_ROOT)
    sp = os.path.join(results, run_id, "score.json")
    return json.load(open(sp, encoding="utf-8")) if os.path.exists(sp) else {}


# ------------------------------------------------------------------- driver
def list_tasks(tiers, ids, heldout):
    root = os.path.join(LRAB, "tasks_heldout" if heldout else "tasks")
    paths = []
    for tier in tiers:
        paths += sorted(glob.glob(os.path.join(root, tier, "*.json")))
    tasks = [(p, json.load(open(p, encoding="utf-8"))) for p in paths]
    if ids:
        tasks = [(p, t) for p, t in tasks if t["id"] in ids]
    return tasks


def cell(d):
    if not d:
        return "—"
    if d.get("failure_mode") == "timeout":
        return "timeout"
    return f"{d.get('total', 0):.2f} ({d.get('wall_seconds', 0):.0f}s, {d.get('failure_mode')})"


def summary(results):
    rows = {}
    for sp in glob.glob(os.path.join(results, "*", "score.json")):
        d = json.load(open(sp, encoding="utf-8"))
        rows.setdefault(d["task_id"], {})[d["agent"]] = d
    lines = ["| task | mingbird | boubou |", "|---|---|---|"]
    tot = {"mingbird": [], "boubou": []}
    for tid in sorted(rows, key=lambda x: (not x.startswith("WF"), x)):
        r = rows[tid]
        lines.append(f"| {tid} | {cell(r.get('mingbird'))} | {cell(r.get('boubou'))} |")
        for a in tot:
            if a in r:
                d = r[a]
                tot[a].append((0 if d.get("failure_mode") == "timeout" else d.get("total", 0),
                               d.get("wall_seconds", 0)))
    for a, v in tot.items():
        if v:
            lines.append(f"\n{a}: n={len(v)} mean total={sum(x for x, _ in v) / len(v):.3f} "
                         f"wall={sum(w for _, w in v) / 60:.0f} min")
    text = "\n".join(lines)
    with open(os.path.join(results, "summary.md"), "w", encoding="utf-8") as f:
        f.write(text + "\n")
    return text


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model", default="gemma4:e2b")
    ap.add_argument("--arms", default="mingbird,boubou")
    ap.add_argument("--tiers", default=",".join(TIERS[:3]), help="comma list; default tiers 1-3")
    ap.add_argument("--tasks", default="", help="comma list of task ids to restrict to")
    ap.add_argument("--heldout", action="store_true", help="use tasks_heldout instead of tasks")
    ap.add_argument("--results", default=os.path.join(BOUBOU_ROOT, "bench", "results", "lrab_h2h"))
    ap.add_argument("--timeout-min", type=int, default=20)
    ap.add_argument("--kill-at-pct", type=int, default=None, help="force kill-resume at this %% of budget")
    ap.add_argument("--judge", default="", help="judge model; empty = deterministic scoring only")
    ap.add_argument("--summary", action="store_true", help="only print the table for --results")
    ap.add_argument("--dry-run", action="store_true", help="print the commands and exit")
    a = ap.parse_args()
    a.results = os.path.abspath(a.results)   # the boubou arm runs with cwd=workdir
    os.makedirs(a.results, exist_ok=True)
    if a.summary:
        print(summary(a.results))
        return
    if not os.path.isdir(LRAB):
        sys.exit(f"Mingbird checkout not found at {MINGBIRD_ROOT} (set MINGBIRD_ROOT)")
    arms = [x for x in a.arms.split(",") if x]
    ids = {x for x in a.tasks.split(",") if x}
    for task_path, task in list_tasks(a.tiers.split(","), ids, a.heldout):
        kill_at = a.kill_at_pct if a.kill_at_pct is not None else task.get("kill_at_pct")
        budget = a.timeout_min
        for arm in arms:
            run_id = f"{arm}_{task['id']}"
            out_dir = os.path.join(a.results, run_id)
            if os.path.exists(os.path.join(out_dir, "score.json")):
                print(f"skip {run_id} (scored)", flush=True)
                continue
            if a.dry_run:
                if arm == "boubou":
                    print(run_id, boubou_cmd(os.path.join(out_dir, "workdir"), a.model)[0])
                else:
                    print(run_id, "run_bench.py --agent mingbird --task", task_path)
                continue
            os.makedirs(out_dir, exist_ok=True)
            print(f"### {run_id} start {time.strftime('%H:%M:%S')} budget={budget}m"
                  + (f" kill_at={kill_at}%" if kill_at else ""), flush=True)
            t0 = time.time()
            if arm == "boubou":
                sc = run_boubou_arm(task_path, task, a.model, out_dir, run_id, budget, kill_at, a.judge)
            else:
                with open(os.path.join(out_dir, "runner.log"), "w", encoding="utf-8") as log:
                    sc = run_mingbird_arm(task_path, a.model, a.results, run_id, budget, kill_at, a.judge, log)
            print(f"    {run_id}: total={sc.get('total')} ms={sc.get('milestone_score')} "
                  f"fs={sc.get('final_score')} wall={sc.get('wall_seconds')} "
                  f"mode={sc.get('failure_mode')} exit={sc.get('exit_code')} "
                  f"({time.time() - t0:.0f}s)", flush=True)
    print("\nMATRIX DONE\n" + summary(a.results), flush=True)


if __name__ == "__main__":
    main()
