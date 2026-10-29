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
  python bench/lrab_h2h.py --reps 3             # 3 replications, (task, arm) cells round-robin
  python bench/lrab_h2h.py --summary            # table from an existing results dir
  python bench/lrab_h2h.py --arms boubou --task-root bench/lrab_ext --ctx 16384,32768,65536
                                                # Boubou-side tiers (context pressure), AGENT_CTX sweep
Every cell also gets process metrics (tool calls, peak prompt tokens, guard firings, integrity;
see process_metrics.py) under score.json["process"], and the summary reports per-arm means with
a bootstrap 95% CI over tasks when there is more than one replication.

Env: MINGBIRD_ROOT (default: sibling `../Mingbird-agent`), BOUBOU_BIN (default: PATH, then
this checkout's target/release then target/debug), AGENT_CTX / AGENT_SLIM_OUTPUT pass through.

Task roots: a task root holds `tasks/<tier>/*.json` and `fixtures/`, Mingbird's LRAB layout. The
default root is Mingbird's `bench/lrab`; `--task-root` points at a Boubou-side root such as
`bench/lrab_ext` (the context-pressure tier). Mingbird's own runner resolves fixtures against its
own tree, so the mingbird arm only runs on the default root; the scorer is the same for both.
"""
import argparse
import glob
import json
import os
import shutil
import subprocess
import sys
import random
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import process_metrics  # noqa: E402  (same directory)

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


def prepare_workdir(task, workdir, task_root=LRAB):
    """Mirror of run_bench.prepare_workdir: fixtures copied in, prompt in task_input.txt."""
    shutil.rmtree(workdir, ignore_errors=True)
    os.makedirs(workdir, exist_ok=True)
    for rel in task.get("fixtures", []):
        src = os.path.join(task_root, rel)
        if os.path.isfile(src):
            shutil.copy2(src, workdir)
        elif os.path.isdir(src):
            shutil.copytree(src, os.path.join(workdir, os.path.basename(src)), dirs_exist_ok=True)
    with open(os.path.join(workdir, "task_input.txt"), "w", encoding="utf-8") as f:
        f.write(task["prompt"])


def boubou_cmd(workdir, model, ctx=None):
    """Same contract and env as Mingbird's `_cmd_mingbird`; no --new so phase 2 of a
    kill-resume run continues from .agent_state.json. `ctx` overrides AGENT_CTX for the cell."""
    env = dict(os.environ)
    env.pop("AGENT_STREAM", None)
    env["BOUBOU_HOME"] = BENCH_HOME
    env["MINGBIRD_HOME"] = BENCH_HOME
    env["AGENT_CTX"] = str(ctx) if ctx else os.environ.get("AGENT_CTX", "32768")
    env["AGENT_SLIM_OUTPUT"] = os.environ.get("AGENT_SLIM_OUTPUT", "0")
    env["AGENT_UNATTENDED"] = "1"
    env.setdefault("PYTHONUTF8", "1")
    ensure_python_on_path(env)
    return [boubou_exe(), "run", model, os.path.join(workdir, "task_input.txt"), workdir], env


def ensure_python_on_path(env):
    """The tasks say `python …`; macOS/Linux often only ship `python3`. Under `uv run` the venv
    provides `python`, under plain `python3` it does not (probe smoke 2026-10-28: every command
    exited 127). Prepend a shim dir with `python` -> this interpreter when it is missing."""
    if os.name == "nt" or shutil.which("python", path=env.get("PATH")):
        return env
    shim = os.path.join(BENCH_HOME, "bin")
    os.makedirs(shim, exist_ok=True)
    link = os.path.join(shim, "python")
    if os.path.islink(link) and os.readlink(link) != sys.executable:
        os.remove(link)
    if not os.path.exists(link):
        os.symlink(sys.executable, link)
    env["PATH"] = shim + os.pathsep + env.get("PATH", "")
    return env


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


def run_boubou(task, workdir, model, timeout_min, kill_at_pct, ctx=None):
    """Returns (rc, stdout, stderr, wall, meta). meta mirrors run_bench's resume dict."""
    cmd, env = boubou_cmd(workdir, model, ctx)
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
    # script_pass checks run `python …` through the shell in this process's environment; give
    # the scorer the same `python` shim the agent gets (CP-03 smoke 2026-10-29: exit 127, score 0
    # although the agent's own run of the same command had passed).
    ensure_python_on_path(os.environ)
    sys.path.insert(0, os.path.join(LRAB, "scoring"))
    from score_task import score_task   # Mingbird's referee, imported read-only
    return score_task(task_path, workdir, judge=bool(judge), judge_model=judge or "ornith-1.5:35b")


def run_boubou_arm(task_path, task, model, out_dir, run_id, timeout_min, kill_at_pct, judge,
                   task_root=LRAB, ctx=None):
    workdir = os.path.join(out_dir, "workdir")
    prepare_workdir(task, workdir, task_root)
    rc, out, err, wall, meta = run_boubou(task, workdir, model, timeout_min, kill_at_pct, ctx)
    with open(os.path.join(out_dir, "transcript.txt"), "w", encoding="utf-8") as f:
        f.write(out)
        if err:
            f.write("\n--- STDERR ---\n" + err)
    base = {"task_id": task["id"], "agent": "boubou", "model": model,
            "wall_seconds": round(wall, 1), "exit_code": rc, "run_id": run_id,
            "ctx": int(boubou_cmd(workdir, model, ctx)[1]["AGENT_CTX"])}
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
    score["process"] = process_metrics.cell_metrics(out + err, task["prompt"], workdir)
    with open(os.path.join(out_dir, "score.json"), "w", encoding="utf-8") as f:
        json.dump(score, f, ensure_ascii=False, indent=2)
    return score


# -------------------------------------------------------------- mingbird arm
def run_mingbird_arm(task_path, task, model, results, run_id, timeout_min, kill_at_pct, judge, log):
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
    if not os.path.exists(sp):
        return {}
    process_metrics.annotate_dir(os.path.join(results, run_id), task["prompt"])
    return json.load(open(sp, encoding="utf-8"))


# ------------------------------------------------------------------- driver
def tiers_under(task_root, heldout=False):
    """Every tier directory under a task root, in name order (tier5_context, …)."""
    root = os.path.join(task_root, "tasks_heldout" if heldout else "tasks")
    return sorted(d for d in os.listdir(root) if os.path.isdir(os.path.join(root, d))) if os.path.isdir(root) else []


def list_tasks(tiers, ids, heldout, task_root=LRAB):
    root = os.path.join(task_root, "tasks_heldout" if heldout else "tasks")
    paths = []
    for tier in tiers:
        paths += sorted(glob.glob(os.path.join(root, tier, "*.json")))
    tasks = [(p, json.load(open(p, encoding="utf-8"))) for p in paths]
    if ids:
        tasks = [(p, t) for p, t in tasks if t["id"] in ids]
    return tasks


def ctx_suffix(ctx):
    """Run-id suffix for a context-size cell: 16384 -> _c16k."""
    return f"_c{int(ctx) // 1024}k"


def row_key(d):
    """Summary row: the task, or task@ctx when the results sweep AGENT_CTX."""
    return d["task_id"] + (f"@{int(d['ctx']) // 1024}k" if d.get("ctx_sweep") else "")


def cell(d):
    if not d:
        return "—"
    if d.get("failure_mode") == "timeout":
        return "timeout"
    return f"{d.get('total', 0):.2f} ({d.get('wall_seconds', 0):.0f}s, {d.get('failure_mode')})"


def cell_total(d):
    return 0.0 if d.get("failure_mode") == "timeout" else float(d.get("total", 0) or 0)


def cell_reps(ds):
    """One task × arm over its replications: mean and spread."""
    v = [cell_total(d) for d in ds]
    m = sum(v) / len(v)
    if len(v) == 1:
        return cell(ds[0])
    modes = sorted({d.get("failure_mode", "?") for d in ds})
    return f"{m:.2f} [{min(v):.2f}–{max(v):.2f}] n={len(v)} ({'/'.join(modes)})"


def bootstrap_ci(task_means, n=2000, seed=0):
    """95% CI of the arm mean, resampling tasks with replacement (tasks are the unit)."""
    if len(task_means) < 2:
        return None
    rng = random.Random(seed)
    k = len(task_means)
    boots = sorted(sum(rng.choice(task_means) for _ in range(k)) / k for _ in range(n))
    return boots[int(0.025 * n)], boots[int(0.975 * n) - 1]


PROCESS_COLS = [("calls", "tool_calls"), ("peak ctx", "peak_prompt_tokens"), ("compact", "compactions"),
                ("fail", "failed_calls"), ("loop", "loop_interceptions"), ("refuse", "finish_refusals"),
                ("empty", "empty_resets"), ("integrity", "integrity"), ("corr", "corrector_calls")]


def summary(results):
    rows = {}
    scores = [json.load(open(sp, encoding="utf-8")) for sp in glob.glob(os.path.join(results, "*", "score.json"))]
    # A results dir with more than one AGENT_CTX is a sweep: rows are task@ctx, not task.
    sweep = len({d.get("ctx") for d in scores if d.get("ctx")}) > 1
    for d in scores:
        d["ctx_sweep"] = sweep
        rows.setdefault(row_key(d), {}).setdefault(d["agent"], []).append(d)
    arms = ["mingbird", "boubou"]
    order = sorted(rows, key=lambda x: (not x.startswith("WF"), x))
    lines = ["| task | mingbird | boubou |", "|---|---|---|"]
    per_task = {a: [] for a in arms}
    for tid in order:
        r = rows[tid]
        lines.append(f"| {tid} | {cell_reps(r['mingbird']) if 'mingbird' in r else '—'} "
                     f"| {cell_reps(r['boubou']) if 'boubou' in r else '—'} |")
        for a in arms:
            if a in r:
                per_task[a].append((sum(cell_total(d) for d in r[a]) / len(r[a]),
                                    sum(d.get("wall_seconds", 0) for d in r[a])))
    for a in arms:
        v = per_task[a]
        if not v:
            continue
        means = [m for m, _ in v]
        reps = sum(len(rows[t].get(a, [])) for t in order)
        line = (f"\n{a}: tasks={len(v)} cells={reps} mean total={sum(means) / len(means):.3f} "
                f"wall={sum(w for _, w in v) / 60:.0f} min")
        ci = bootstrap_ci(means)
        if ci and reps > len(v):
            line += f" 95% CI [{ci[0]:.3f}, {ci[1]:.3f}]"
        lines.append(line)
    # Process table: per-arm means over cells, then per task.
    have = [d for r in rows.values() for ds in r.values() for d in ds if "process" in d]
    if have:
        lines.append("\n| process (mean per cell) | " + " | ".join(arms) + " |")
        lines.append("|---|" + "---|" * len(arms))
        for label, key in PROCESS_COLS:
            vals = []
            for a in arms:
                cells = [d["process"].get(key, 0) for t in order for d in rows[t].get(a, []) if "process" in d]
                vals.append(f"{sum(cells) / len(cells):.1f}" if cells else "—")
            lines.append(f"| {label} | " + " | ".join(vals) + " |")
        row_cols = "/".join(label for label, key in PROCESS_COLS if key != "empty_resets")
        lines.append("\n| task | " + " | ".join(f"{a} {row_cols}" for a in arms) + " |")
        lines.append("|---|" + "---|" * len(arms))
        for tid in order:
            parts = []
            for a in arms:
                ds = [d for d in rows[tid].get(a, []) if "process" in d]
                if not ds:
                    parts.append("—")
                    continue
                pm = [sum(d["process"].get(k, 0) for d in ds) / len(ds) for _l, k in PROCESS_COLS if k != "empty_resets"]
                parts.append("/".join(f"{x:.0f}" for x in pm))
            lines.append(f"| {tid} | " + " | ".join(parts) + " |")
        missing = [(d["agent"], d["task_id"], d["process"]["named_files_missing"]) for d in have
                   if d["process"].get("named_files_missing")]
        if missing:
            lines.append("\nfinished with named deliverables missing: " +
                         "; ".join(f"{a} {t} {m}" for a, t, m in missing))
    text = "\n".join(lines)
    with open(os.path.join(results, "summary.md"), "w", encoding="utf-8") as f:
        f.write(text + "\n")
    return text


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model", default="gemma4:e2b")
    ap.add_argument("--arms", default="mingbird,boubou")
    ap.add_argument("--task-root", default=LRAB,
                    help="directory holding tasks/<tier>/*.json and fixtures/ (default: Mingbird's LRAB)")
    ap.add_argument("--tiers", default="",
                    help="comma list; default tiers 1-3 under LRAB, every tier under another root")
    ap.add_argument("--ctx", default="",
                    help="comma list of AGENT_CTX values to sweep (boubou arm); run ids get _c16k etc.")
    ap.add_argument("--tasks", default="", help="comma list of task ids to restrict to")
    ap.add_argument("--heldout", action="store_true", help="use tasks_heldout instead of tasks")
    ap.add_argument("--results", default=os.path.join(BOUBOU_ROOT, "bench", "results", "lrab_h2h"))
    ap.add_argument("--timeout-min", type=int, default=20)
    ap.add_argument("--kill-at-pct", type=int, default=None, help="force kill-resume at this %% of budget")
    ap.add_argument("--judge", default="", help="judge model; empty = deterministic scoring only")
    ap.add_argument("--reps", type=int, default=1,
                    help="replications per cell; cells run round-robin over (rep, task, arm) with the "
                         "arm order alternating per task so machine drift cannot follow one arm")
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
    task_root = os.path.abspath(a.task_root)
    arms = [x for x in a.arms.split(",") if x]
    if "mingbird" in arms and os.path.realpath(task_root) != os.path.realpath(LRAB):
        sys.exit("the mingbird arm resolves fixtures against Mingbird's own LRAB root, so a custom "
                 "--task-root runs with --arms boubou only")
    ids = {x for x in a.tasks.split(",") if x}
    tiers = [t for t in a.tiers.split(",") if t] or (
        TIERS[:3] if os.path.realpath(task_root) == os.path.realpath(LRAB) else tiers_under(task_root, a.heldout))
    tasks = list_tasks(tiers, ids, a.heldout, task_root)
    ctxs = [int(x) for x in a.ctx.split(",") if x] or [None]
    cells = []
    for rep in range(a.reps):
        for ctx in ctxs:
            for ti, (task_path, task) in enumerate(tasks):
                arm_order = arms if (ti + rep) % 2 == 0 else arms[::-1]
                for arm in arm_order:
                    cells.append((rep, ctx, task_path, task, arm))
    for rep, ctx, task_path, task, arm in cells:
        kill_at = a.kill_at_pct if a.kill_at_pct is not None else task.get("kill_at_pct")
        budget = a.timeout_min
        run_id = f"{arm}_{task['id']}" + (ctx_suffix(ctx) if ctx else "") + (f"_r{rep}" if a.reps > 1 else "")
        out_dir = os.path.join(a.results, run_id)
        if os.path.exists(os.path.join(out_dir, "score.json")):
            print(f"skip {run_id} (scored)", flush=True)
            continue
        if a.dry_run:
            if arm == "boubou":
                cmd, env = boubou_cmd(os.path.join(out_dir, "workdir"), a.model, ctx)
                print(run_id, f"AGENT_CTX={env['AGENT_CTX']}", cmd)
            else:
                print(run_id, "run_bench.py --agent mingbird --task", task_path)
            continue
        os.makedirs(out_dir, exist_ok=True)
        print(f"### {run_id} start {time.strftime('%H:%M:%S')} budget={budget}m"
              + (f" ctx={ctx}" if ctx else "") + (f" kill_at={kill_at}%" if kill_at else ""), flush=True)
        t0 = time.time()
        if arm == "boubou":
            sc = run_boubou_arm(task_path, task, a.model, out_dir, run_id, budget, kill_at, a.judge,
                                task_root, ctx)
        else:
            with open(os.path.join(out_dir, "runner.log"), "w", encoding="utf-8") as log:
                sc = run_mingbird_arm(task_path, task, a.model, a.results, run_id, budget, kill_at, a.judge, log)
        if a.reps > 1 and sc:
            sc["rep"] = rep
            with open(os.path.join(out_dir, "score.json"), "w", encoding="utf-8") as f:
                json.dump(sc, f, ensure_ascii=False, indent=2)
        pm = sc.get("process", {})
        print(f"    {run_id}: total={sc.get('total')} ms={sc.get('milestone_score')} "
              f"fs={sc.get('final_score')} wall={sc.get('wall_seconds')} "
              f"mode={sc.get('failure_mode')} exit={sc.get('exit_code')} "
              f"calls={pm.get('tool_calls')} peak={pm.get('peak_prompt_tokens')} "
              f"compact={pm.get('compactions')} "
              f"loop={pm.get('loop_interceptions')} refuse={pm.get('finish_refusals')} "
              f"integrity={pm.get('integrity')} ({time.time() - t0:.0f}s)", flush=True)
    print("\nMATRIX DONE\n" + summary(a.results), flush=True)


if __name__ == "__main__":
    main()
