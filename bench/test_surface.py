"""Tests for the surface tiers (bench/lrab_ext/gen_surface.py; docs/refactor.md step 10): the
generator is deterministic and matches the checked-in tree, each editor task fails before its
intended change and passes after it, the referee scripts answer the way the scorer expects, the
chat tasks' expected values agree with the fixtures, and the bench driver threads a `--policy`
axis through the command, the run id, the axes and the summary, writing the final reply to
`reply.md` for a product-policy cell.

Run: uv run --with pytest --python 3.13 python -m pytest bench/ -q
"""
import json
import os
import re
import shutil
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lrab_ext"))
import gen_surface  # noqa: E402
import lrab_h2h  # noqa: E402
from test_lrab_ext import same_tree  # noqa: E402

EXT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "lrab_ext")
FIX = os.path.join(EXT, "fixtures")


def run(cmd, cwd):
    r = subprocess.run([sys.executable] + cmd, cwd=cwd, capture_output=True, text=True, timeout=120)
    return r.returncode, r.stdout + r.stderr


def rewrite(path, old, new):
    text = open(path, encoding="utf-8").read()
    assert old in text, (path, old)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text.replace(old, new))


def task(tier, tid):
    return json.load(open(os.path.join(EXT, "tasks", tier, f"{tid}.json"), encoding="utf-8"))


def workdir_for(tmp_path, tier, tid):
    """The task's fixtures laid out as the driver lays them out for the agent."""
    t = task(tier, tid)
    wd = str(tmp_path / tid)
    lrab_h2h.prepare_workdir(t, wd, EXT)
    return t, wd


def test_generator_is_deterministic_and_matches_the_checked_in_tree(tmp_path):
    gen_surface.generate(str(tmp_path / "a"))
    gen_surface.generate(str(tmp_path / "b"))
    assert same_tree(str(tmp_path / "a"), str(tmp_path / "b"))
    for sub in os.listdir(str(tmp_path / "a" / "fixtures")):
        assert same_tree(str(tmp_path / "a" / "fixtures" / sub), os.path.join(FIX, sub)), \
            f"fixtures/{sub} drifted from the generator: rerun gen_surface.py"
    for tier in ("tier6_editor", "tier7_chat"):
        assert same_tree(str(tmp_path / "a" / "tasks" / tier), os.path.join(EXT, "tasks", tier))


def test_briefs_carry_no_batch_preamble_and_land_at_the_workdir_root(tmp_path):
    for tier in ("tier6_editor", "tier7_chat"):
        for name in sorted(os.listdir(os.path.join(EXT, "tasks", tier))):
            t = json.load(open(os.path.join(EXT, "tasks", tier, name), encoding="utf-8"))
            assert "numbered plan" not in t["prompt"] and "autonomous agent" not in t["prompt"], t["id"]
            assert t["plan_steps"] == [] and t["required_plan_steps_min"] == 0
    _t, wd = workdir_for(tmp_path, "tier6_editor", "ED-01")
    assert os.path.isfile(os.path.join(wd, "slug.py")), "files, not a fixture subdirectory"
    _t, wd = workdir_for(tmp_path, "tier7_chat", "CH-03")
    assert os.path.isdir(os.path.join(wd, "inbox")) and os.path.isfile(os.path.join(wd, "check_tidy.py"))


def test_ed01_fails_then_passes_and_collateral_is_caught(tmp_path):
    _t, wd = workdir_for(tmp_path, "tier6_editor", "ED-01")
    rc, out = run(["test_slug.py"], wd)
    assert rc == 1 and "3 passed, 3 failed" in out, out
    rc, out = run(["check_collateral.py"], wd)
    assert rc == 1 and "COLLATERAL PENDING: slug.py: slugify, _NON_WORD unchanged" in out, out
    p = os.path.join(wd, "slug.py")
    src = open(p, encoding="utf-8").read()
    fixed = src.replace('re.compile(r"[^a-z0-9]")', 're.compile(r"[^a-z0-9]+")') \
               .replace("    s = _NON_WORD.sub(\"-\", s)\n    return s", "    s = _NON_WORD.sub(\"-\", s)\n    return s.strip(\"-\")")
    assert fixed != src
    open(p, "w", encoding="utf-8").write(fixed)
    rc, out = run(["test_slug.py"], wd)
    assert rc == 0 and "6 passed, 0 failed" in out, out
    assert "COLLATERAL OK" in run(["check_collateral.py"], wd)[1]
    # Touching another definition is collateral.
    open(p, "a", encoding="utf-8").write("\n\ndef slug_path(section, title):\n    return title\n")
    assert "COLLATERAL CHANGED: slug.py:slug_path" in run(["check_collateral.py"], wd)[1]


def test_ed02_fails_then_passes_and_the_note_is_the_named_deliverable(tmp_path):
    t, wd = workdir_for(tmp_path, "tier6_editor", "ED-02")
    rc, out = run(["test_rates.py"], wd)
    assert rc == 1 and "3 passed, 3 failed" in out, out
    assert "COLLATERAL PENDING" in run(["check_collateral.py"], wd)[1]
    p = os.path.join(wd, "rates.py")
    rewrite(p, "if amount > lower:", "if amount >= lower:")
    rc, out = run(["test_rates.py"], wd)
    assert rc == 0 and "6 passed, 0 failed" in out, out
    assert "COLLATERAL OK" in run(["check_collateral.py"], wd)[1]
    assert [a["artifact"] for a in t["final_artifacts"]] == ["fix_note.md", "fix_note.md"], \
        "the note is the deliverable the named-file gate would ask for"


def test_ed03_reference_title_case_satisfies_the_referee(tmp_path):
    t, wd = workdir_for(tmp_path, "tier6_editor", "ED-03")
    assert "TITLE_CASE FAIL" in run(["check_title_case.py"], wd)[1]
    assert "COLLATERAL PENDING: textfmt.py: title_case unchanged" in run(["check_collateral.py"], wd)[1]
    assert [a["artifact"] for a in t["final_artifacts"]] == ["textfmt.py"], \
        "the existing tests pass before any work, so they are a condition of the checker, not a final check"
    with open(os.path.join(wd, "textfmt.py"), "a", encoding="utf-8") as f:
        f.write('''

SMALL = {"a", "an", "the", "of", "in", "on", "and"}


def title_case(s):
    words = s.split()
    out = []
    for i, w in enumerate(words):
        low = w.lower()
        out.append(low if (low in SMALL and i > 0) else low.capitalize())
    return " ".join(out)
''')
    rc, out = run(["check_title_case.py"], wd)
    assert rc == 0 and "TITLE_CASE OK" in out, out
    assert "4 passed, 0 failed" in run(["test_textfmt.py"], wd)[1]
    assert "COLLATERAL OK" in run(["check_collateral.py"], wd)[1], "adding a function changes nothing else"
    # A red test file is caught by the checker, since the test run lives there now.
    rewrite(os.path.join(wd, "test_textfmt.py"), "def test_", "def test_aaa_bad():\n    assert title_case('x') == 'Y'\n\n\ndef test_")
    out = run(["check_collateral.py"], wd)[1]
    assert "COLLATERAL TESTS FAIL: test_textfmt.py" in out, out


def test_ed04_rename_passes_and_the_old_name_is_gone(tmp_path):
    t, wd = workdir_for(tmp_path, "tier6_editor", "ED-04")
    assert "3 passed, 0 failed" in run(["test_store.py"], wd)[1], "green before any work: not a check on its own"
    assert t["milestones"] == [{"artifact": "store/repo.py", "check": "script_pass",
                                "params": {"command": "python check_collateral.py", "expect_stdout": "COLLATERAL OK", "timeout_s": 120}}]
    assert "COLLATERAL PENDING: store/repo.py: fetch_all unchanged" in run(["check_collateral.py"], wd)[1]
    # Renaming the definition without its callers: the rename happened, the tests now fail.
    rewrite(os.path.join(wd, "store/repo.py"), "fetchAll", "fetch_all")
    out = run(["check_collateral.py"], wd)[1]
    assert "COLLATERAL TESTS FAIL: test_store.py" in out, out
    for rel in ("store/service.py", "store/cli.py"):
        rewrite(os.path.join(wd, rel), "fetchAll", "fetch_all")
    rc, out = run(["test_store.py"], wd)
    assert rc == 0 and "3 passed, 0 failed" in out, out
    assert "COLLATERAL OK" in run(["check_collateral.py"], wd)[1]
    # Touching an unrelated definition is still collateral.
    rewrite(os.path.join(wd, "store/repo.py"), "def fetch_one(conn, order_id):", "def fetch_one(conn, order_id, extra=None):")
    assert "COLLATERAL CHANGED: store/repo.py:fetch_one" in run(["check_collateral.py"], wd)[1]
    for check in t["final_artifacts"]:
        text = open(os.path.join(wd, check["artifact"]), encoding="utf-8").read()
        if check["check"] == "not_contains":
            assert not any(s in text for s in check["params"]["none_of"]), check
        else:
            assert any(s in text for s in check["params"]["any_of"]), check


def test_ed05_is_explained_not_changed(tmp_path):
    t, wd = workdir_for(tmp_path, "tier6_editor", "ED-05")
    assert "5 passed, 0 failed" in run(["test_duration.py"], wd)[1]
    assert "COLLATERAL PENDING: reply.md missing" in run(["check_collateral.py"], wd)[1]
    open(os.path.join(wd, "reply.md"), "w", encoding="utf-8").write("The pattern uses fullmatch and wants the m suffix.\n")
    assert "COLLATERAL OK" in run(["check_collateral.py"], wd)[1]
    # The cause named by the fixture: fullmatch against a pattern that wants the m suffix.
    sys.path.insert(0, wd)
    try:
        import importlib
        duration = importlib.import_module("duration")
        assert duration.parse_duration("1h30") is None and duration.parse_duration("1h30m") == 90
    finally:
        sys.path.remove(wd)
        sys.modules.pop("duration", None)
    any_of = t["final_artifacts"][1]["params"]["any_of"]
    assert "fullmatch" in any_of and "suffix" in any_of
    # A generic paragraph that never names the mechanism must not pass (gemma4:e2b, 2026-10-30,
    # scored 1.0 on the bare 'm' entry from zero tool calls).
    bluff = ("This behavior is likely due to how parse_duration is implemented. Many duration "
             "parsers expect a specific format, so '1h30' may be invalid while '1h30m' with the 'm' is accepted.")
    assert not any(s.lower() in bluff.lower() for s in any_of), [s for s in any_of if s.lower() in bluff.lower()]
    honest = "The regex is matched with fullmatch and the minutes group requires the m suffix, so '1h30' has no match."
    assert any(s.lower() in honest.lower() for s in any_of)


def test_ed06_has_the_escapes_and_catches_the_near_duplicate(tmp_path):
    _t, wd = workdir_for(tmp_path, "tier6_editor", "ED-06")
    src = open(os.path.join(wd, "export.py"), encoding="utf-8").read()
    # The fixture is what it is for: escape sequences in the source, the target line twice.
    assert "'\\\\\"'" in src and '"\\\\n"' in src, src
    assert src.count('",".join(cells) + "\\n"') == 2
    rc, out = run(["test_export.py"], wd)
    assert rc == 1 and "4 passed, 2 failed" in out, out
    assert "COLLATERAL PENDING: export.py: tsv_line unchanged" in run(["check_collateral.py"], wd)[1]
    # Replacing every occurrence (the single-`old` rename shape) fixes tsv_line and breaks csv_line.
    p = os.path.join(wd, "export.py")
    open(p, "w", encoding="utf-8").write(src.replace('",".join(cells)', '"\\t".join(cells)'))
    rc, out = run(["test_export.py"], wd)
    assert rc == 1 and "5 passed, 1 failed" in out and "test_csv_line" in out, out
    assert "COLLATERAL CHANGED: export.py:csv_line" in run(["check_collateral.py"], wd)[1]
    # The one right edit: the second occurrence only.
    i = src.rindex('",".join(cells)')
    fixed = src[:i] + '"\\t".join(cells)' + src[i + len('",".join(cells)'):]
    open(p, "w", encoding="utf-8").write(fixed)
    rc, out = run(["test_export.py"], wd)
    assert rc == 0 and "6 passed, 0 failed" in out, out
    assert "COLLATERAL OK" in run(["check_collateral.py"], wd)[1]

def test_ch01_ch02_expected_values_are_in_the_notes():
    notes = "".join(gen_surface.NOTES.values())
    assert "owner Priya Natarajan; cut-over Saturday 14 November 2026" in notes
    for owner in ("Priya", "Tomasz", "Mei"):
        assert f"owner {owner}" in notes
    assert notes.count("DECISION:") == 4
    t = task("tier7_chat", "CH-02")
    groups = t["final_artifacts"][0]["params"]["groups"]
    assert all(any(v.lower() in notes.lower() for v in g) for g in groups)


def test_ch03_tidy_referee_passes_only_when_every_file_is_moved(tmp_path):
    _t, wd = workdir_for(tmp_path, "tier7_chat", "CH-03")
    assert "TIDY FAIL" in run(["check_tidy.py"], wd)[1]
    inbox = os.path.join(wd, "inbox")
    for sub, files in gen_surface.INBOX.items():
        os.makedirs(os.path.join(inbox, sub), exist_ok=True)
        for name in files:
            shutil.move(os.path.join(inbox, name), os.path.join(inbox, sub, name))
    rc, out = run(["check_tidy.py"], wd)
    assert rc == 0 and "TIDY OK" in out, out
    # A file in the wrong folder is reported by name.
    shutil.move(os.path.join(inbox, "data", "export.csv"), os.path.join(inbox, "docs", "export.csv"))
    assert "export.csv not under data/" in run(["check_tidy.py"], wd)[1]


def test_ch04_total_and_highest_month_follow_the_csv(tmp_path):
    t, wd = workdir_for(tmp_path, "tier7_chat", "CH-04")
    import csv
    per_month = {"04": 0.0, "05": 0.0, "06": 0.0}
    with open(os.path.join(wd, "expenses.csv"), encoding="utf-8", newline="") as f:
        for row in csv.DictReader(f):
            if row["category"] == "travel" and row["date"][5:7] in per_month:
                per_month[row["date"][5:7]] += float(row["amount"])
    total = round(sum(per_month.values()), 2)
    assert total == t["answer"]["travel_q2_total"]
    assert max(per_month, key=per_month.get) == "05"
    variants, month = t["final_artifacts"][0]["params"]["groups"]
    assert f"{total:,.2f}" in variants and f"{total:.2f}" in variants and month == ["May"]


def test_ch05_referee_tells_touched_from_untouched_and_new_files(tmp_path):
    _t, wd = workdir_for(tmp_path, "tier7_chat", "CH-05")
    rc, out = run(["check_untouched.py"], wd)
    assert rc == 1 and "UNTOUCHED PENDING" in out and "UNTOUCHED OK" not in out, out
    # The harness's own files are not "new"; a plan or a stray file is.
    open(os.path.join(wd, ".agent_state.json"), "w").write("[]")
    open(os.path.join(wd, "reply.md"), "w").write("Hello! I can read the CSV, ...\n")
    out = run(["check_untouched.py"], wd)[1]
    assert "UNTOUCHED OK" in out and "NO NEW FILES" in out, out
    open(os.path.join(wd, "todo.json"), "w").write("{}")
    out = run(["check_untouched.py"], wd)[1]
    assert "NEW FILES: todo.json" in out, out
    open(os.path.join(wd, "notes.txt"), "a").write("edited\n")
    assert "UNTOUCHED FAIL: notes.txt" in run(["check_untouched.py"], wd)[1]


def test_driver_threads_the_policy_axis(tmp_path, monkeypatch):
    assert lrab_h2h.tiers_under(EXT) == ["tier5_context", "tier6_editor", "tier7_chat"]
    cmd, _env = lrab_h2h.boubou_cmd(str(tmp_path), "m")
    assert "--policy" not in cmd, "unset: the batch harness, as before"
    cmd, _env = lrab_h2h.boubou_cmd(str(tmp_path), "m", policy="coding")
    assert cmd[-2:] == ["--policy", "coding"]
    assert lrab_h2h.cell_run_id("boubou", "ED-01", "m", None, None, None, set(), "coding") == "boubou_ED-01_pcoding"
    assert lrab_h2h.cell_run_id("boubou", "ED-01", "m", 16384, None, 1, set(), "general") == "boubou_ED-01_c16k_pgeneral_r1"
    assert lrab_h2h.cell_axes("m", None, None)["policy"] == "unattended"
    assert lrab_h2h.cell_axes("m", None, None, "general")["policy"] == "general"
    old = {"task_id": "CP-01", "agent": "boubou", "total": 1.0, "wall_seconds": 1, "ctx": 8192,
           "model": "m", "failure_mode": "completed", "process": {}}
    assert lrab_h2h.axes_of(old)["policy"] == "unattended", "a score.json from before the axis"
    # Two policies on one task: the summary labels rows by policy alone.
    for policy, total in (("general", 0.6), ("coding", 0.9)):
        rid = lrab_h2h.cell_run_id("boubou", "ED-02", "m", None, None, None, set(), policy)
        (tmp_path / rid).mkdir()
        json.dump({"task_id": "ED-02", "agent": "boubou", "total": total, "wall_seconds": 30,
                   "failure_mode": "completed", "axes": lrab_h2h.cell_axes("m", None, None, policy),
                   "process": {"tool_calls": 4, "peak_prompt_tokens": 900}},
                  open(tmp_path / rid / "score.json", "w"))
    text = lrab_h2h.summary(str(tmp_path))
    assert "varying: policy" in text, text
    assert "| ED-02@coding | — | 0.90" in text and "| ED-02@general | — | 0.60" in text


def test_final_reply_is_the_last_text_or_the_finish_summary(tmp_path):
    text_end = [{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "read_file", "arguments": {"path": "a"}}}]},
                {"role": "tool", "content": "..."},
                {"role": "assistant", "content": "Priya owns it; 14 November."}]
    assert lrab_h2h.final_reply(text_end) == "Priya owns it; 14 November."
    finish_end = text_end[:4] + [{"role": "assistant", "content": "",
                                  "tool_calls": [{"function": {"name": "finish", "arguments": {"summary": "Done: 14 November."}}}]}]
    assert lrab_h2h.final_reply(finish_end) == "Done: 14 November."
    finish_str = text_end[:4] + [{"role": "assistant", "content": "",
                                  "tool_calls": [{"function": {"name": "finish", "arguments": json.dumps({"summary": "As a string."})}}]}]
    assert lrab_h2h.final_reply(finish_str) == "As a string."
    assert lrab_h2h.final_reply(text_end[:2]) is None
    wd = tmp_path / "wd"
    wd.mkdir()
    assert lrab_h2h.write_reply(str(wd)) is None, "no checkpoint, no reply"
    json.dump(text_end, open(wd / ".agent_state.json", "w"))
    assert lrab_h2h.write_reply(str(wd)) == "Priya owns it; 14 November."
    assert open(wd / "reply.md").read() == "Priya owns it; 14 November.\n"
    open(wd / "reply.md", "w").write("the agent's own\n")
    lrab_h2h.write_reply(str(wd))
    assert open(wd / "reply.md").read() == "the agent's own\n", "an agent-written reply.md is kept"


@pytest.mark.skipif(not os.path.isdir(lrab_h2h.LRAB), reason="Mingbird checkout (the scorer) not found")
def test_mingbird_scorer_scores_a_reference_solution_of_ed02_and_ch01(tmp_path):
    # ED-02 solved: the fix, the note; full marks. Unsolved: the collateral milestone alone.
    t, wd = workdir_for(tmp_path, "tier6_editor", "ED-02")
    task_path = os.path.join(EXT, "tasks", "tier6_editor", "ED-02.json")
    before = lrab_h2h.score_with_mingbird(task_path, wd, judge="")
    assert before["total"] == 0.0, before
    rewrite(os.path.join(wd, "rates.py"), "if amount > lower:", "if amount >= lower:")
    open(os.path.join(wd, "fix_note.md"), "w", encoding="utf-8").write("The bands are inclusive at the lower bound; the comparison used > instead of >=.\n")
    after = lrab_h2h.score_with_mingbird(task_path, wd, judge="")
    assert after["total"] == 1.0, after
    # CH-01: the answer is text; the driver's reply.md is what the scorer reads.
    t, wd = workdir_for(tmp_path, "tier7_chat", "CH-01")
    task_path = os.path.join(EXT, "tasks", "tier7_chat", "CH-01.json")
    json.dump([{"role": "user", "content": t["prompt"]},
               {"role": "assistant", "content": "Priya Natarajan owns the Postgres migration; the cut-over is on 14 November 2026."}],
              open(os.path.join(wd, ".agent_state.json"), "w"))
    lrab_h2h.write_reply(wd)
    assert lrab_h2h.score_with_mingbird(task_path, wd, judge="")["total"] == 1.0


@pytest.mark.skipif(not os.path.isdir(lrab_h2h.LRAB), reason="Mingbird checkout (the scorer) not found")
def test_an_untouched_fixture_scores_nothing_on_every_surface_task(tmp_path):
    # A crashed cell, or one that read the files and quit, must not collect the collateral
    # credit: no milestone or final check may pass before the task's own work has happened.
    for tier in ("tier6_editor", "tier7_chat"):
        for name in sorted(os.listdir(os.path.join(EXT, "tasks", tier))):
            tid = name[:-5]
            _t, wd = workdir_for(tmp_path, tier, tid)
            sc = lrab_h2h.score_with_mingbird(os.path.join(EXT, "tasks", tier, name), wd, judge="")
            assert sc["total"] == 0.0, (tid, sc["milestone_details"], sc["final_details"])
