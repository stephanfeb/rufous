"""Unit tests for the bench-side process metrics and the replication summary.

Run: uv run --with pytest --python 3.13 python -m pytest bench/ -q
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lrab_h2h  # noqa: E402
import process_metrics as pm  # noqa: E402

BOUBOU = """\
I will plan.\x1b[2m[ctx: 1582/32768 = 4%]\x1b[0m
[0|+4s] ⚙ todo {"action":"create","items":["1. plan"]} -> [ ] 1. plan
[ctx: 2900/32768 = 9%]
[1|+7s] ⚙ run_bash {"command":"python -m pytest -q"} -> [exit 1]
[2|+9s] ⚙ create_file {"content":"x"} -> [tool error: create_file 缺少参数 'path'。必须提供参数: ["path"]。
[2] ⚠️ [missing argument] create_file missing argument 'path' 1 times in a row
[3] ⚠️ duplicate call blocked: read_file (1)
[4|+20s] ⚙ create_file {"path":"report.md","content":"# r"} -> [created report.md (3 bytes)]
[5|+30s] ⚙ finish {"summary":"done"} -> ⚠️ rejected
[5] ⚠️ plan check: plan names ["fix_summary.md"] but they are missing, finish rejected
[6] ⚠️ pre-delivery check: re-injected the original task (100 chars), asked for a self-check before finishing again
[7|+40s] ⚙ finish {"summary":"done"} -> [TASK_COMPLETE] done

===== TASK COMPLETE =====
[TASK_COMPLETE] done
"""

FORCED = """\
[0|+4s] ⚙ run_bash {"command":"x"} -> [exit 2]
[1|+9s] ⚙ finish {"summary":"done"} -> [TASK_COMPLETE] done
[1] ⚠️ plan out of sync (0/2), finish rejected
[3] ⏱ budget nudge injected (4min used)
[4] ⚠️ repeated output detected (1)
[wrap-up] this turn is sufficient, forcing an end (Q&A repetition / dead-loop guard)
"""

MINGBIRD = """\
[ctx: 1643/32768 = 5%]
[0|+8s] ⚙ todo {"action": "create", "items": ["1. plan"]} -> [ ] 1. plan
[1|+12s] ⚙ run_bash {"command": "python -m pytest -q"} -> [exit 1]
[2] ⚠️ 重复调用拦截: run_bash(1)
[3] ⚠️ 连续空轮×6,硬复位上下文(第 1/3 次,累计空轮 6)
[ctx: 20484/32768 = 62%]
[4] ⚠️ 测试未通过,拒绝 finish(第 1/3 次,满 3 次后放行)
[5|+60s] ⚙ run_bash {"command": "python -m pytest -q"} -> [exit 0]
[6|+70s] ⚙ finish {"summary": "ok"} -> [TASK_COMPLETE] ok

===== TASK COMPLETE =====
[TASK_COMPLETE] ok
"""

PROMPT = ("Write report.md and fix_summary.md with the exact filename given; keep plan in plan.md. "
          "Input is data.csv (fixture). Do not write task_input.txt.")


def test_boubou_transcript_counts_and_integrity(tmp_path):
    (tmp_path / "report.md").write_text("# r")
    (tmp_path / "data.csv").write_text("a,b")
    m = pm.cell_metrics(BOUBOU, PROMPT, str(tmp_path))
    assert m["tool_calls"] == 6
    assert m["peak_prompt_tokens"] == 2900
    assert m["failed_calls"] == 1
    assert m["nonzero_exits"] == 1
    assert m["loop_interceptions"] == 2          # missing-argument ladder + duplicate block
    assert m["finish_refusals"] == 1
    assert m["predelivery_checks"] == 1
    assert m["finish_accepted"] is True
    # The last run_bash before the accepted finish exited 1, and fix_summary.md was never written.
    assert m["finished_after_fail"] == 1
    assert m["named_files_missing"] == ["fix_summary.md"]
    assert m["integrity"] == 2


def test_mingbird_transcript_chinese_guards(tmp_path):
    m = pm.cell_metrics(MINGBIRD, "Write out.md.", str(tmp_path))
    assert m["tool_calls"] == 4
    assert m["peak_prompt_tokens"] == 20484
    assert m["loop_interceptions"] == 1
    assert m["empty_resets"] == 1
    assert m["finish_refusals"] == 1
    assert m["finished_after_fail"] == 0         # the retry passed before finish
    assert m["named_files_missing"] == ["out.md"]
    assert m["integrity"] == 1


def test_unfinished_run_has_no_integrity_charge(tmp_path):
    text = '[0|+4s] ⚙ run_bash {"command":"x"} -> [exit 2]\n'
    m = pm.cell_metrics(text, "Write never.md.", str(tmp_path))
    assert m["finish_accepted"] is False
    assert m["integrity"] == 0
    assert m["named_files_missing"] == []


def test_rejected_finish_then_forced_end_is_not_an_accepted_finish(tmp_path):
    # The `⚙ finish -> [TASK_COMPLETE]` line is printed before the gate's rejection; only the
    # banner means the finish stood. Here the wrap-up guard ended the run instead.
    m = pm.cell_metrics(FORCED, "Write never.md.", str(tmp_path))
    assert m["finish_accepted"] is False
    assert m["forced_end"] is True
    assert m["budget_nudges"] == 1
    assert m["finish_refusals"] == 1 and m["loop_interceptions"] == 1
    assert m["integrity"] == 0
    # Mingbird's wrap-up line counts the same way.
    m = pm.cell_metrics(FORCED.replace("[wrap-up] this turn is sufficient", "[收尾] 本轮已足够"), "", str(tmp_path))
    assert m["forced_end"] is True


def test_named_files_skip_plan_and_prompt_and_dedupe():
    assert pm.named_files(PROMPT) == ["report.md", "fix_summary.md", "data.csv"]
    assert pm.named_files("see a.py, a.py and fig.png") == ["a.py", "fig.png"]


def _score(results, agent, tid, rep, total, process=None):
    d = results / f"{agent}_{tid}_r{rep}"
    d.mkdir(parents=True)
    body = {"task_id": tid, "agent": agent, "total": total, "wall_seconds": 60,
            "failure_mode": "completed", "rep": rep}
    if process is not None:
        body["process"] = process
    (d / "score.json").write_text(json.dumps(body))


def test_summary_with_replications_reports_spread_and_ci(tmp_path):
    proc = {k: 0 for _l, k in lrab_h2h.PROCESS_COLS}
    proc.update(tool_calls=10, peak_prompt_tokens=5000, named_files_missing=[])
    for rep, (mb, bb) in enumerate([(1.0, 0.5), (0.5, 1.0), (0.75, 0.75)]):
        _score(tmp_path, "mingbird", "WF-01", rep, mb, proc)
        _score(tmp_path, "boubou", "WF-01", rep, bb, proc)
        _score(tmp_path, "mingbird", "WF-02", rep, 1.0, proc)
        _score(tmp_path, "boubou", "WF-02", rep, 1.0, proc)
    text = lrab_h2h.summary(str(tmp_path))
    assert "| WF-01 | 0.75 [0.50–1.00] n=3 (completed) | 0.75 [0.50–1.00] n=3 (completed) |" in text
    assert "mingbird: tasks=2 cells=6 mean total=0.875" in text
    assert "95% CI [" in text
    assert "| calls | 10.0 | 10.0 |" in text
    assert (tmp_path / "summary.md").exists()


def test_bootstrap_ci_brackets_the_mean_and_needs_two_tasks():
    assert lrab_h2h.bootstrap_ci([0.9]) is None
    lo, hi = lrab_h2h.bootstrap_ci([0.5, 0.7, 0.9, 1.0])
    assert lo <= 0.775 <= hi
    assert 0.5 <= lo < hi <= 1.0


def test_round_robin_alternates_arm_order():
    # Mirror of main()'s cell ordering: (ti + rep) parity flips the arm order.
    arms = ["mingbird", "boubou"]
    order = [(arms if (ti + rep) % 2 == 0 else arms[::-1])[0]
             for rep in range(2) for ti in range(3)]
    assert order == ["mingbird", "boubou", "mingbird", "boubou", "mingbird", "boubou"]


def test_corrector_lines_are_counted_separately():
    text = (
        "[3] ⚠️ corrector consulted: run_bash failed 2× with identical arguments; verdict: give up: HTTP 503\n"
        "[4] ⚠️ blocked by corrector verdict: run_bash\n"
        "[7] ⚠️ corrector consulted: edit_file failed 2× with identical arguments; verdict: retry create_file → fix ran ok\n"
    )
    m = pm.parse_transcript(text)
    assert m["corrector_calls"] == 2
    assert m["corrector_giveups"] == 1
    assert m["corrector_fixes"] == 1
    assert m["loop_interceptions"] == 1
    assert m["finish_refusals"] == 0


HARNESS_DAMAGE = """\
[0|+4s] ⚙ run_bash {"command":"sed -i '' \"s/a/b\\nc/\" lib/x.dart"} -> [exit 1]
[0] 🐚 [exit 1]: sed: 1: "s/a/b ...": unescaped newline inside substitute pattern
[1|+6s] ⚙ run_bash {"command":"python3 -c \"print(1)\""} -> [exit 127]
[1] 🐚 [exit 127]: sh: line 2: import: command not found
[2|+8s] ⚙ run_bash {"command":"rm -f /Users/me/proj/lib/networking/host_service.dart.bak 2>&1 || true"} -> [exit 0]
[3|+9s] ⚙ run_bash {"command":"dart analyze lib/"} -> [exit 1]
[3] 🐚 [exit 1]: error - main.dart:14:7 - The named parameter 'debugLocale' isn't defined.
[4|+10s] ⚙ run_bash {"command":"flutter clean"} -> [exit 0]
[4] ⚠️ streak note: run_bash 5 in a row, last call repeated
[5] ⚠️ idle streak: 12 read-only calls without progress, nudge 1
[6] ⏱ budget nudge injected (4min used)
[7|+20s] ⚙ run_bash {"command":"flutter clean"} -> [exit 0]
[7] ⚠️ run_bash 8 in a row is now disabled
"""


def test_a_session_log_reads_like_a_run_log():
    # `<session>.log` (bead boubou-bmq) stamps each line and adds `[turn]` markers; the
    # counters must not move.
    stamped = "\n".join(
        "2026-10-31 17:16:44 " + l for l in ("[turn] Create a note\n" + BOUBOU).splitlines())
    plain = pm.parse_transcript(BOUBOU)
    logged = pm.parse_transcript(stamped)
    for k in ("tool_calls", "failed_calls", "nonzero_exits", "peak_prompt_tokens",
              "loop_interceptions", "finish_refusals", "finish_accepted", "harness_notes"):
        assert logged[k] == plain[k], (k, logged[k], plain[k])
    assert logged["tool_calls"] == 6


def test_harness_damage_counters():
    m = pm.parse_transcript(HARNESS_DAMAGE)
    # Two shell-level failures (the sed newline and the split python program); the dart
    # analyzer's own error is the program's, not the shell's.
    assert m["shell_errors"] == 2
    assert m["nonzero_exits"] == 3
    assert m["bak_calls"] == 1
    assert m["streak_notes"] == 1
    assert m["idle_notes"] == 1 and m["budget_nudges"] == 1
    assert m["harness_notes"] == 3
    assert m["loop_interceptions"] == 1, "the disable line counts as before"
    assert [k for _l, k in lrab_h2h.PROCESS_COLS][-3:] == ["shell_errors", "bak_calls", "harness_notes"]


def test_the_status_tag_counts_failures_and_refusals_without_reading_the_text(tmp_path):
    text = (
        '[0|+1s] ⚙ create_file {"path":"a.txt"} -> [some words the model reads] ⟂refused:boundary:user\n'
        '[1|+2s] ⚙ edit_file {"path":"a.txt"} -> [other words] ⟂error\n'
        '[2|+3s] ⚙ edit_file {"path":"a.txt"} -> [no change: …] ⟂nochange\n'
        '[3|+4s] ⚙ run_bash {"command":"x"} -> [exit 0]\n'
        '[4|+5s] ⚙ create_file {"content":"x"} -> [tool error: missing path]\n'
    )
    m = pm.parse_transcript(text)
    assert m["tool_calls"] == 5
    assert m["refused_calls"] == 1
    assert m["failed_calls"] == 2, "the tag counts the first, the untagged opening the last"
