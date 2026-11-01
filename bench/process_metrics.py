#!/usr/bin/env python3
"""Process metrics for one LRAB cell, from the agent transcript and the finished workdir.

Both arms print the same transcript shape (rufous mirrors Mingbird's CLI output):
  [ctx: 1582/32768 = 4%]                       prompt tokens before a turn
  [N|+Ns] ⚙ tool {"args"...} -> result head     a tool call and the head of its result
  [N] ⚠️ text                                    a guard firing (English in rufous, Chinese in Mingbird)
  [N] 🐚 [exit N]: text                          a failed command's first stderr line (rufous)
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
  compactions           compaction ladder steps taken ([context compaction L1..3] / [上下文压缩 L1..3]
                        lines, failed levels excluded) plus context-overflow forced compactions
  compaction_max_level  the deepest ladder level reached (0 = none)
  prunes                recency-pruning passes ([context pruning: N older tool results …], rufous only)
  pruned_results        tool results collapsed to fact lines over the run
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
# A session log (`<session>.log`, bead boubou-bmq) puts a wall-clock stamp before each line.
STAMP = re.compile(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} ", re.M)
CTX = re.compile(r"\[ctx: (\d+)/(\d+) = \d+%\]")
CALL = re.compile(r"^\[(\d+)\|\+(\d+)s\] ⚙ (\S+) (.*?)(?: -> (.*))?$")
WARN = re.compile(r"^\[(\d+)\] ⚠️ ?(.*)$")
EXIT = re.compile(r"\[exit (-?\d+)\]")
# The compaction ladder's own line, printed as a plain log line (not a ⚠️ guard) by both arms:
#   [context compaction L2: 41 → 12 messages]   /   [上下文压缩 L2: 41 → 12 条消息]
COMPACT = re.compile(r"^\[(?:context compaction|上下文压缩) L(\d)(?P<failed>[^\]]*?(?:failed|失败))?")
PRUNE = re.compile(r"^\[context pruning: (\d+) older tool result")
# Idle-streak guard (boubou-9c5): `[12] ⚠️ idle streak: 8 read-only calls … nudge 1` / `… refused (2)`
IDLE = re.compile(r"^\[\d+\] ⚠️ idle streak: (\d+) read-only calls")
# Harness-caused damage (bead boubou-bk9.14). `[i] 🐚 [exit N]: <first stderr line>` follows a
# failed run_bash; a line the shell or an interpreter wrote about the command's own text is
# the class a harness rewrite of the command produces (2026-10-31: four in a row).
SHELL_LINE = re.compile(r"^\[\d+\] 🐚 \[exit (-?\d+)\]: (.*)$")
SHELL_ERR = re.compile(r"unescaped newline|unterminated string|command not found|syntax error|"
                       r"unexpected EOF|unexpected end of file|not a valid identifier|bad substitution", re.I)
STREAK_NOTE = "⚠️ streak note:"
BANNER = "===== TASK COMPLETE ====="
FORCED_END = ("[wrap-up]", "[收尾]", "===== MAX ITERATIONS =====")
# Files the task text names: `shortlist.md`, `clean_data.py`, `fig_arrhenius.png` …
NAMED_FILE = re.compile(r"\b([\w][\w\-]*\.(?:md|py|json|yaml|yml|csv|txt|png|svg|html|toml|sh))\b")
# Never deliverables even when named: the prompt itself and plan files.
NOT_DELIVERABLE = {"task_input.txt", "plan.md", "todo.json"}

# Guard classifier: (metric, [substrings]); English (rufous) and Chinese (Mingbird) side by side.
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
    return STAMP.sub("", ANSI.sub("", text))


def parse_transcript(text):
    """Counts and the call list from one transcript (ANSI allowed)."""
    m = {k: 0 for k, _ in GUARDS}
    m.update(tool_calls=0, failed_calls=0, refused_calls=0, nonzero_exits=0, peak_prompt_tokens=0,
             compaction_max_level=0, prunes=0, pruned_results=0,
             budget_nudges=0, idle_notes=0, idle_streak_max=0, forced_end=False, finish_accepted=False,
             corrector_calls=0, corrector_fixes=0, corrector_giveups=0,
             shell_errors=0, bak_calls=0, streak_notes=0, harness_notes=0)
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
        idl = IDLE.match(line)
        if idl:
            m["idle_notes"] += 1
            m["idle_streak_max"] = max(m["idle_streak_max"], int(idl.group(1)))
        sh = SHELL_LINE.match(line)
        if sh:
            if SHELL_ERR.search(sh.group(2)):
                m["shell_errors"] += 1
            continue
        pr = PRUNE.match(line)
        if pr:
            m["prunes"] += 1
            m["pruned_results"] += int(pr.group(1))
            continue
        k = COMPACT.match(line)
        if k and not k.group("failed"):
            m["compactions"] += 1
            m["compaction_max_level"] = max(m["compaction_max_level"], int(k.group(1)))
            continue
        c = CALL.match(line)
        if c:
            name, result = c.group(3), c.group(5) or ""
            status = ""
            if " ⟂" in result:
                # The status tag after the result head (bead boubou-bj4): `error`,
                # `nochange`, `refused:<kind>[:user]`; absent on a call that did its job.
                result, status = result.rsplit(" ⟂", 1)
                status = status.strip()
            m["tool_calls"] += 1
            calls.append((name, result))
            if ".bak" in (c.group(4) or ""):
                m["bak_calls"] += 1
            if status == "error" or (not status and result.startswith("[tool error:")):
                m["failed_calls"] += 1
            if status.startswith("refused"):
                m["refused_calls"] += 1
            ex = EXIT.search(result) if name == "run_bash" else None
            if ex and int(ex.group(1)) != 0:
                m["nonzero_exits"] += 1
            continue
        w = WARN.match(line)
        if w:
            msg = w.group(2)
            if msg.startswith("streak note:"):
                m["streak_notes"] += 1
                continue
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
    # Every note the loop put into the model's context short of a refusal.
    m["harness_notes"] = m["streak_notes"] + m["idle_notes"] + m["budget_nudges"]
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
