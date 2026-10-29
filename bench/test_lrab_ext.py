"""Tests for the context-pressure tier (bench/lrab_ext, bead boubou-65d.9): the generator is
deterministic and matches the checked-in fixtures, the planted bugs fail before and pass after
the intended fixes, the collateral checkers catch unrelated edits, the task files' expected
values agree with the fixtures, and the bench driver resolves a custom task root and an
AGENT_CTX sweep.

Run: uv run --with pytest --python 3.13 python -m pytest bench/ -q
"""
import filecmp
import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lrab_ext"))
import gen_pressure  # noqa: E402
import lrab_h2h  # noqa: E402
import process_metrics as pm  # noqa: E402

EXT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "lrab_ext")
FIX = os.path.join(EXT, "fixtures")


def run(cmd, cwd):
    r = subprocess.run([sys.executable] + cmd, cwd=cwd, capture_output=True, text=True, timeout=120)
    return r.returncode, r.stdout


def same_tree(a, b):
    cmp = filecmp.dircmp(a, b)
    if cmp.left_only or cmp.right_only or cmp.diff_files or cmp.funny_files:
        return False
    return all(same_tree(os.path.join(a, d), os.path.join(b, d)) for d in cmp.common_dirs)


def test_generator_is_deterministic_and_matches_checked_in_tier(tmp_path):
    gen_pressure.generate(str(tmp_path / "a"))
    gen_pressure.generate(str(tmp_path / "b"))
    assert same_tree(str(tmp_path / "a"), str(tmp_path / "b"))
    assert same_tree(str(tmp_path / "a" / "fixtures"), FIX), "fixtures drifted from the generator: rerun gen_pressure.py"
    assert same_tree(str(tmp_path / "a" / "tasks"), os.path.join(EXT, "tasks"))


def test_cp01_counts_in_task_match_the_log():
    task = json.load(open(os.path.join(EXT, "tasks", "tier5_context", "CP-01.json")))
    log = open(os.path.join(FIX, "cp01_server.log")).read().splitlines()
    assert len(log) == 2400
    counts = {}
    for line in log:
        m = re.search(r" ERROR \[\w+\] (E\d{4}) ", line)
        if m:
            counts[m.group(1)] = counts.get(m.group(1), 0) + 1
    assert counts == {c: n for c, _comp, _msg, n in gen_pressure.ERROR_CODES}
    fatal_reqs = [re.search(r"req=(req-\w+)", line).group(1) for line in log if " FATAL " in line]
    assert fatal_reqs == task["pressure"]["fatal_requests"]
    # the count groups name the right number for each code (first variant is "CODE | N")
    groups = task["final_artifacts"][1]["params"]["groups"]
    assert {g[0].split(" | ")[0]: int(g[0].split(" | ")[1]) for g in groups} == counts


def _fix_cp02(pkg):
    norm = os.path.join(pkg, "textkit", "normalize.py")
    s = open(norm).read().replace('_WS = re.compile(r" +")', '_WS = re.compile(r"[ \\t]+")')
    open(norm, "w").write(s)
    tok = os.path.join(pkg, "textkit", "tokens.py")
    s = open(tok).read().replace("range(len(tokens) - n)]", "range(len(tokens) - n + 1)]")
    open(tok, "w").write(s)
    st = os.path.join(pkg, "textkit", "stats.py")
    s = open(st).read().replace("        return v[n // 2]\n    return v[n // 2]\n",
                                "        return v[n // 2]\n    return (v[n // 2 - 1] + v[n // 2]) / 2\n")
    open(st, "w").write(s)


def test_cp02_planted_bugs_fail_then_pass_and_collateral_is_caught(tmp_path):
    pkg = str(tmp_path / "cp02_pkg")
    shutil.copytree(os.path.join(FIX, "cp02_pkg"), pkg)
    rc, out = run(["run_tests.py"], pkg)
    assert rc == 1
    failed = re.search(r"^FAILED: (.*)$", out, re.M).group(1)
    assert all(name in failed for name in ("test_collapse_spaces_tabs", "test_ngrams_bigrams", "test_median_even"))
    assert out.strip().splitlines()[-1] == "51 passed, 9 failed"
    assert len(out.splitlines()) > 1000, "the runner must be verbose enough to pressure the context"
    _fix_cp02(pkg)
    rc, out = run(["run_tests.py"], pkg)
    assert rc == 0 and out.strip().splitlines()[-1] == "60 passed, 0 failed"
    rc, out = run(["check_collateral.py"], pkg)
    assert (rc, out.strip()) == (0, "COLLATERAL OK")
    fmt = os.path.join(pkg, "textkit", "format.py")
    open(fmt, "a").write("\n\ndef pluralise(n, word):\n    return f'{n} {word}s'\n")
    rc, out = run(["check_collateral.py"], pkg)
    assert rc == 1 and "format.py:pluralise" in out


def test_cp03_boundary_bug_fails_then_passes_and_collateral_is_caught(tmp_path):
    d = str(tmp_path / "cp03_billing")
    shutil.copytree(os.path.join(FIX, "cp03_billing"), d)
    task = json.load(open(os.path.join(EXT, "tasks", "tier5_context", "CP-03.json")))
    src = open(os.path.join(d, "pricing.py")).read()
    assert src.count("\n") >= 1500 and task["pressure"]["target_line"] > 1200
    rc, out = run(["test_pricing.py"], d)
    assert rc == 1 and out.strip().splitlines()[-1] == "10 passed, 3 failed"
    for t in ("platinum", "gold", "silver"):
        src = src.replace(f'if units > TIER_THRESHOLDS["{t}"]', f'if units >= TIER_THRESHOLDS["{t}"]')
    open(os.path.join(d, "pricing.py"), "w").write(src)
    rc, out = run(["test_pricing.py"], d)
    assert rc == 0 and out.strip().splitlines()[-1] == "13 passed, 0 failed"
    assert run(["check_collateral.py"], d) == (0, "COLLATERAL OK\n")
    open(os.path.join(d, "pricing.py"), "w").write(src.replace("factor: float = 1.03", "factor: float = 1.04"))
    rc, out = run(["check_collateral.py"], d)
    assert rc == 1 and "helper_03" in out


def test_cp04_every_scorer_key_matches_exactly_one_decision_line():
    task = json.load(open(os.path.join(EXT, "tasks", "tier5_context", "CP-04.json")))
    notes = sorted(os.listdir(os.path.join(FIX, "cp04_notes")))
    assert len(notes) == 8
    decisions = [line for f in notes for line in open(os.path.join(FIX, "cp04_notes", f)).read().splitlines()
                 if line.startswith("- DECISION:")]
    assert len(decisions) == 16
    for grp in task["final_artifacts"][0]["params"]["groups"]:
        assert sum(1 for d in decisions if any(v in d for v in grp)) == 1, grp


def test_driver_lists_custom_root_and_resolves_fixtures_against_it(tmp_path):
    assert lrab_h2h.tiers_under(EXT) == ["tier5_context"]
    tasks = lrab_h2h.list_tasks(["tier5_context"], set(), False, EXT)
    assert [t["id"] for _p, t in tasks] == ["CP-01", "CP-02", "CP-03", "CP-04"]
    _p, cp03 = tasks[2]
    wd = str(tmp_path / "workdir")
    lrab_h2h.prepare_workdir(cp03, wd, EXT)
    assert os.path.isfile(os.path.join(wd, "cp03_billing", "pricing.py"))
    assert open(os.path.join(wd, "task_input.txt")).read() == cp03["prompt"]


def test_driver_ctx_sweep_names_cells_and_groups_the_summary(tmp_path):
    assert lrab_h2h.ctx_suffix(16384) == "_c16k" and lrab_h2h.ctx_suffix(65536) == "_c64k"
    _cmd, env = lrab_h2h.boubou_cmd(str(tmp_path), "m", 16384)
    assert env["AGENT_CTX"] == "16384"
    for ctx, total in ((16384, 0.4), (65536, 0.9)):
        d = tmp_path / f"boubou_CP-01{lrab_h2h.ctx_suffix(ctx)}"
        d.mkdir()
        json.dump({"task_id": "CP-01", "agent": "boubou", "total": total, "wall_seconds": 60, "ctx": ctx,
                   "failure_mode": "completed", "process": {"tool_calls": 5, "peak_prompt_tokens": ctx // 2,
                                                            "compactions": 2 if ctx == 16384 else 0}},
                  open(d / "score.json", "w"))
    text = lrab_h2h.summary(str(tmp_path))
    assert "| CP-01@16k | — | 0.40" in text and "| CP-01@64k | — | 0.90" in text
    assert "| compact | — | 1.0 |" in text


def test_compaction_lines_are_counted_for_both_arms():
    boubou = ("[ctx: 30000/32768 = 91%]\n[context compaction L1: truncated old tool outputs]\n"
              "[context compaction L2: 41 → 12 messages]\n[context compaction L3 failed, level skipped: x]\n"
              "[5|+9s] ⚙ finish {} -> [TASK_COMPLETE] ok\n")
    m = pm.parse_transcript(boubou)
    assert (m["compactions"], m["compaction_max_level"], m["peak_prompt_tokens"]) == (2, 2, 30000)
    mingbird = "[上下文压缩 L1: 截断旧工具输出]\n[上下文压缩 L2 失败,本层跳过: KeyError x]\n[上下文压缩 L3: 40 → 3 条消息]\n"
    m = pm.parse_transcript(mingbird)
    assert (m["compactions"], m["compaction_max_level"]) == (2, 3)
    assert pm.parse_transcript("[3] ⚠️ context-overflow error → forcing L3 compaction and retrying\n")["compactions"] == 1
