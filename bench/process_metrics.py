#!/usr/bin/env python3
"""Process metrics for one LRAB cell, from the agent transcript and the finished workdir.

Both arms print the same transcript shape (Boubou mirrors Mingbird's CLI output):
  [ctx: 1582/32768 = 4%]                       prompt tokens before a turn
  [N|+Ns] ⚙ tool {"args"...} -> result head     a tool call and the head of its result
  [N] ⚠️ text                                    a guard firing (English in Boubou, Chinese in Mingbird)
so one parser scores both. The counts reported, alongside the artefact score:

  tool_calls            ⚙ lines
  peak_prompt_tokens    max ctx numerator
  failed_calls          results that start with [tool error: (missing/invalid arguments, refused paths)
  nonzero_exits         run_bash results with [exit N], N != 0
  loop_interceptions    duplicate-call blocks, repeat/talk-streak stops, disabled-tool hard resets
  empty_resets          consecutive-empty-turn context resets
  finish_refusals       finish rejected by any gate (hollow, tests, artefact, plan, sync)
  predelivery_checks    the original task re-injected before finishing
  truncations           generation budget burnt / tool call cut by done_reason=length
  compactions           context-overflow forced compaction
  budget_nudges         ⏱ time-budget nudges injected
  forced_end            the run was ended by the wrap-up/dead-loop guard or the iteration cap,
                        not by an accepted finish ([wrap-up] / [收尾] / ===== MAX ITERATIONS)
  finish_accepted       the "===== TASK COMPLETE =====" banner both arms print once a finish
                        passes every gate (a `⚙ finish … -> [TASK_COMPLETE]` line alone is not
                        enough: the gate's ⚠️ rejection follows it on the next line)
  integrity             completion claims that contradict ground truth (see below)
  corrector_calls       isolated-corrector consultations (a call failed twice identically)
  corrector_fixes       consultations whose corrected call ran without error
  corrector_giveups     consultations that judged the step blocked (honest exit requested)

Integrity is the sum of two checks made only when the run ended through finish:
  named_files_missing   files named with an extension in the task prompt that do not exist in
                        the workdir (the harness let a finish through with deliverables absent)
  finished_after_fail   the last run_bash before the accepted finish exited non-zero
These are lexical, not judged: Advani (arXiv:2606.09863) finds LLM judges cannot detect false
success while cheap text detectors can.

Usage: python bench/process_metrics.py <results_dir>   # backfill "process" into every score.json
"""
import glob
import json
import os
import re
import sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")
CTX = re.compile(r"\[ctx: (\d+)/(\d+) = \d+%\]")
CALL = re.compile(r"^\[(\d+)\|\+(\d+)s\] ⚙ (\S+) (.*?)(?: -> (.*))?$")
WARN = re.compile(r"^\[(\d+)\] ⚠️ ?(.*)$")
EXIT = re.compile(r"\[exit (-?\d+)\]")
BANNER = "===== TASK COMPLETE ====="
FORCED_END = ("[wrap-up]", "[收尾]", "===== MAX ITERATIONS =====")
# Files the task text names: `shortlist.md`, `clean_data.py`, `fig_arrhenius.png` …
NAMED_FILE = re.compile(r"\b([\w][\w\-]*\.(?:md|py|json|yaml|yml|csv|txt|png|svg|html|toml|sh))\b")
# Never deliverables even when named: the prompt itself and plan files.
NOT_DELIVERABLE = {"task_input.txt", "plan.md", "todo.json"}

# Guard classifier: (metric, [substrings]); English (Boubou) and Chinese (Mingbird) side by side.
GUARDS = [
    ("finish_refusals", ["finish rejected", "rejected a hollow finish", "拒绝 finish", "拒绝假 finish"]),
    ("predelivery_checks", ["pre-delivery check", "交付自查"]),
    ("empty_resets", ["consecutive empty turns", "连续空轮"]),
    ("loop_interceptions", [
        "duplicate call blocked", "重复调用拦截",
        "repeated output detected",
        "consecutive text-only turns",
        "after being disabled", "被禁用后仍被调用",
        "is now disabled", "已被禁用", "现被禁用",
        "times in a row",                              # missing-argument ladder / edit_file streak
        "blocked by corrector verdict",
    ]),
    ("truncations", ["done_reason=length", "tool call truncated", "生成预算被烧尽"]),
    ("compactions", ["compaction", "压缩"]),
    ("blocked_calls", ["Q&A guard", "问答守护", "outside the working directory", "工作目录之外"]),
]


def _strip(text):
    return ANSI.sub("", text)


def parse_transcript(text):
    """Counts and the call list from one transcript (ANSI allowed)."""
    m = {k: 0 for k, _ in GUARDS}
    m.update(tool_calls=0, failed_calls=0, nonzero_exits=0, peak_prompt_tokens=0,
             budget_nudges=0, forced_end=False, finish_accepted=False,
             corrector_calls=0, corrector_fixes=0, corrector_giveups=0)
    calls = []
    clean = _strip(text)
    m["finish_accepted"] = BANNER in clean
    m["forced_end"] = any(f in clean for f in FORCED_END) and not m["finish_accepted"]
    for raw in clean.splitlines():
        line = raw.rstrip()
        if line.startswith("[") and "⏱" in line:
            m["budget_nudges"] += 1
            continue
        for a, _b in CTX.findall(line):
            m["peak_prompt_tokens"] = max(m["peak_prompt_tokens"], int(a))
        c = CALL.match(line)
        if c:
            name, result = c.group(3), c.group(5) or ""
            m["tool_calls"] += 1
            calls.append((name, result))
            if result.startswith("[tool error:"):
                m["failed_calls"] += 1
            ex = EXIT.search(result) if name == "run_bash" else None
            if ex and int(ex.group(1)) != 0:
                m["nonzero_exits"] += 1
            continue
        w = WARN.match(line)
        if w:
            msg = w.group(2)
            if "corrector consulted" in msg:
                m["corrector_calls"] += 1
                if "fix ran ok" in msg:
                    m["corrector_fixes"] += 1
                elif "give up" in msg:
                    m["corrector_giveups"] += 1
                continue
            for metric, needles in GUARDS:
                if any(n in msg for n in needles):
                    m[metric] += 1
                    break
    # The finish that stood is the last ⚙ finish (the banner follows it); was the last command
    # before it a failure?
    m["finished_after_fail"] = 0
    if m["finish_accepted"]:
        last_finish = max((i for i, (n, _r) in enumerate(calls) if n == "finish"), default=len(calls))
        last_exit = 0
        for n, r in calls[:last_finish]:
            if n == "run_bash":
                ex = EXIT.search(r)
                last_exit = int(ex.group(1)) if ex else last_exit
        m["finished_after_fail"] = int(last_exit != 0)
    return m


def named_files(prompt):
    seen = []
    for f in NAMED_FILE.findall(prompt):
        if f not in NOT_DELIVERABLE and f not in seen:
            seen.append(f)
    return seen


def missing_named_files(prompt, workdir):
    if not os.path.isdir(workdir):
        return []
    present = set()
    for root, _dirs, files in os.walk(workdir):
        present.update(files)
    return [f for f in named_files(prompt) if f not in present]


def cell_metrics(transcript_text, prompt, workdir):
    m = parse_transcript(transcript_text)
    missing = missing_named_files(prompt, workdir) if m["finish_accepted"] else []
    m["named_files_missing"] = missing
    m["integrity"] = len(missing) + (m["finished_after_fail"] if m["finish_accepted"] else 0)
    return m


def task_prompt_for(score, lrab_root):
    """Find the task prompt for a scored cell from Mingbird's task tree (read-only)."""
    tid = score.get("task_id", "")
    for sub in ("tasks", "tasks_heldout"):
        for p in glob.glob(os.path.join(lrab_root, sub, "*", f"{tid}.json")):
            try:
                return json.load(open(p, encoding="utf-8")).get("prompt", "")
            except (OSError, ValueError):
                pass
    return ""


def annotate_dir(cell_dir, prompt):
    """Compute metrics for one results cell and write them under score.json["process"]."""
    sp = os.path.join(cell_dir, "score.json")
    tp = os.path.join(cell_dir, "transcript.txt")
    if not (os.path.exists(sp) and os.path.exists(tp)):
        return None
    score = json.load(open(sp, encoding="utf-8"))
    text = open(tp, encoding="utf-8", errors="replace").read()
    score["process"] = cell_metrics(text, prompt, os.path.join(cell_dir, "workdir"))
    with open(sp, "w", encoding="utf-8") as f:
        json.dump(score, f, ensure_ascii=False, indent=2)
    return score["process"]


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    results = os.path.abspath(sys.argv[1])
    here = os.path.dirname(os.path.abspath(__file__))
    lrab = os.environ.get("MINGBIRD_ROOT") or os.path.join(
        os.path.dirname(os.path.dirname(here)), "Mingbird-agent")
    lrab = os.path.join(lrab, "bench", "lrab")
    for sp in sorted(glob.glob(os.path.join(results, "*", "score.json"))):
        cell_dir = os.path.dirname(sp)
        score = json.load(open(sp, encoding="utf-8"))
        pm = annotate_dir(cell_dir, task_prompt_for(score, lrab))
        if pm:
            print(f"{os.path.basename(cell_dir):>22}: calls={pm['tool_calls']:>3} "
                  f"peak={pm['peak_prompt_tokens']:>6} fail={pm['failed_calls']} "
                  f"loop={pm['loop_interceptions']} refuse={pm['finish_refusals']} "
                  f"integrity={pm['integrity']} missing={pm['named_files_missing']}")


if __name__ == "__main__":
    main()
