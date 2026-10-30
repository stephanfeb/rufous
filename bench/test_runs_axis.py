"""--runs (repeats per cell with mean and spread) and --workdir-name (the cell's directory
name, the last segment of the path the model is shown), bead boubou-z6k."""
import io
import json
import os
import sys
from contextlib import redirect_stdout

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lrab_h2h  # noqa: E402

EXT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bench", "lrab_ext")


def _score(task, total, rep=None, workdir="workdir", policy="coding", wall=30):
    d = {"task_id": task, "agent": "boubou", "total": total, "wall_seconds": wall,
         "failure_mode": "completed", "axes": lrab_h2h.cell_axes("gemma4:e2b", None, None, policy, workdir),
         "process": {"tool_calls": 4, "peak_prompt_tokens": 2000}}
    if rep is not None:
        d["rep"] = rep
    return d


def test_the_workdir_name_is_an_axis_in_the_run_id_and_the_row_label():
    assert lrab_h2h.cell_run_id("boubou", "ED-01", "m", None, None, None, set(), "coding") == "boubou_ED-01_pcoding"
    assert lrab_h2h.cell_run_id("boubou", "ED-01", "m", None, None, None, set(), "coding", "workdir") \
        == "boubou_ED-01_pcoding", "the default name adds nothing"
    assert lrab_h2h.cell_run_id("boubou", "ED-01", "m", None, None, 2, set(), "coding", "proj") \
        == "boubou_ED-01_pcoding_wproj_r2"
    assert lrab_h2h.cell_axes("m", None, None, "coding", "proj")["workdir"] == "proj"
    old = {"task_id": "ED-01", "agent": "boubou", "total": 1.0, "model": "m", "ctx": 32768}
    assert lrab_h2h.axes_of(old)["workdir"] == "workdir", "a score from before the axis existed"
    assert lrab_h2h.row_key(_score("ED-01", 1.0, workdir="proj"), ["workdir"]) == "ED-01@dir=proj"


def test_runs_show_per_task_spread_and_per_arm_run_means(tmp_path):
    # Two tasks × two runs, one preset: the second run collapses on ED-01 and lifts ED-03.
    totals = {("ED-01", 0): 1.0, ("ED-01", 1): 0.0, ("ED-03", 0): 0.29, ("ED-03", 1): 0.71}
    for (task, rep), total in totals.items():
        rid = lrab_h2h.cell_run_id("boubou", task, "gemma4:e2b", None, None, rep, set(), "coding")
        (tmp_path / rid).mkdir()
        json.dump(_score(task, total, rep), open(tmp_path / rid / "score.json", "w"))
    text = lrab_h2h.summary(str(tmp_path))
    assert "workdir=" not in text, "the default directory name is layout, not a setting"
    assert "| ED-01 | — | 0.50 [0.00–1.00] n=2 (completed) |" in text
    assert "| ED-03 | — | 0.50 [0.29–0.71] n=2 (completed) |" in text
    assert "boubou: tasks=2 cells=4 mean total=0.500" in text
    # Run 0 scored (1.0 + 0.29) / 2, run 1 (0.0 + 0.71) / 2; the sd across runs is the spread
    # a comparison between two results dirs has to clear.
    assert "run means 0.65/0.35, sd 0.21" in text
    # A single run per cell says nothing about runs.
    single = tmp_path / "single"
    single.mkdir()
    (single / "boubou_ED-01_pcoding").mkdir()
    json.dump(_score("ED-01", 1.0), open(single / "boubou_ED-01_pcoding" / "score.json", "w"))
    assert "run means" not in lrab_h2h.summary(str(single))
    assert lrab_h2h.spread([0.65, 0.35]) == pytest.approx(0.2121, abs=1e-3)
    assert lrab_h2h.spread([0.5]) == 0.0


def test_two_workdir_names_share_a_results_dir_and_label_their_rows(tmp_path):
    for name, total in (("workdir", 0.0), ("proj", 1.0)):
        rid = lrab_h2h.cell_run_id("boubou", "ED-01", "gemma4:e2b", None, None, None, set(), "coding", name)
        (tmp_path / rid).mkdir()
        json.dump(_score("ED-01", total, workdir=name), open(tmp_path / rid / "score.json", "w"))
    text = lrab_h2h.summary(str(tmp_path))
    assert "varying: workdir" in text
    assert "| ED-01@dir=workdir | — | 0.00" in text
    assert "| ED-01@dir=proj | — | 1.00" in text


def test_dry_run_lays_the_cell_out_under_the_named_directory(tmp_path, monkeypatch):
    if not os.path.isdir(lrab_h2h.LRAB):
        pytest.skip("Mingbird checkout not present")
    monkeypatch.setenv("BOUBOU_BIN", sys.executable)  # any executable: the command is only printed
    argv = ["lrab_h2h.py", "--arms", "boubou", "--task-root", EXT, "--tiers", "tier6_editor",
            "--tasks", "ED-01", "--policy", "coding", "--runs", "2", "--workdir-name", "proj",
            "--results", str(tmp_path), "--dry-run"]
    monkeypatch.setattr(sys, "argv", argv)
    out = io.StringIO()
    with redirect_stdout(out):
        lrab_h2h.main()
    lines = [l for l in out.getvalue().splitlines() if l.startswith("boubou_ED-01")]
    assert [l.split()[0] for l in lines] == ["boubou_ED-01_pcoding_wproj_r0", "boubou_ED-01_pcoding_wproj_r1"]
    assert all("workdir=proj" in l for l in lines)
    assert all(os.path.join("boubou_ED-01_pcoding_wproj_r0", "proj") in lines[0] for _ in [0])
    assert os.path.join("_wproj_r1", "proj", "task_input.txt") in lines[1]


def test_workdir_name_is_one_plain_segment(tmp_path, monkeypatch):
    if not os.path.isdir(lrab_h2h.LRAB):
        pytest.skip("Mingbird checkout not present")
    monkeypatch.setattr(sys, "argv", ["lrab_h2h.py", "--arms", "boubou", "--task-root", EXT,
                                      "--tiers", "tier6_editor", "--workdir-name", "a/b",
                                      "--results", str(tmp_path), "--dry-run"])
    with pytest.raises(SystemExit, match="plain directory name"):
        lrab_h2h.main()
    monkeypatch.setattr(sys, "argv", ["lrab_h2h.py", "--workdir-name", "proj", "--results", str(tmp_path),
                                      "--dry-run"])
    with pytest.raises(SystemExit, match="boubou only"):
        lrab_h2h.main()
