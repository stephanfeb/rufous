"""Hidden scorer overlay (bench/lrab_ext/hidden, bead boubou-65d.10): the generated files match
the generator, a reference fix passes the hidden tests while an overfit to the visible suite does
not, the spec check catches an edited spec, and the driver folds the overlay into a score.
Needs Mingbird's fixtures (read-only); skipped when the checkout is absent.

Run: uv run --with pytest --python 3.13 python -m pytest bench/ -q
"""
import filecmp
import json
import os
import shutil
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lrab_ext"))
import gen_hidden  # noqa: E402
import lrab_h2h  # noqa: E402

HIDDEN = os.path.join(os.path.dirname(os.path.abspath(__file__)), "lrab_ext", "hidden")
FIX = gen_hidden.LRAB_FIXTURES
needs_mingbird = pytest.mark.skipif(not os.path.isdir(FIX), reason="Mingbird checkout not found")


def run(args, cwd):
    # No -I: the visible test_suite.py imports m01..m10 from the script directory, as the scorer
    # runs it (plain `python test_suite.py`).
    r = subprocess.run([sys.executable] + args, cwd=cwd, capture_output=True, text=True, timeout=120)
    return r.returncode, r.stdout


def last_line(out):
    return out.strip().splitlines()[-1]


def workdir_for(tmp_path, task_id):
    """A finished-looking workdir: the task's fixture files plus the hidden overlay."""
    src = {"WF-04": "buggy_project", "LH-03": "buggy_pipeline_pkg"}[task_id]
    d = tmp_path / task_id
    shutil.copytree(os.path.join(FIX, src), d)
    for f in os.listdir(os.path.join(HIDDEN, task_id)):
        if os.path.isfile(os.path.join(HIDDEN, task_id, f)):
            shutil.copy2(os.path.join(HIDDEN, task_id, f), d)
    return str(d)


def fix_wf04(d):
    p = os.path.join(d, "inventory.py")
    s = open(p).read()
    s = s.replace('    db[sku] = {"name": name, "qty": qty, "price": price}\n    return db',
                  '    if sku in db:\n        raise ValueError(sku)\n'
                  '    db[sku] = {"name": name, "qty": qty, "price": price}\n    return db')
    s = s.replace('    db[sku]["qty"] -= quantity\n    return db',
                  '    if quantity > db[sku]["qty"]:\n        raise ValueError("insufficient stock")\n'
                  '    db[sku]["qty"] -= quantity\n    if db[sku]["qty"] == 0:\n        del db[sku]\n    return db')
    s = s.replace('return sum(item["price"] for item in db.values())',
                  'return sum(float(item["price"]) * item["qty"] for item in db.values())')
    s = s.replace('if term in it["name"]}', 'if term.lower() in it["name"].lower()}')
    open(p, "w").write(s)


REFERENCE_LH03 = {
    "m01": "def calc_mean(values, factor=1.0):\n    return sum(values) / len(values)\n",
    "m02": "def calc_median(values, factor=1.0):\n    vs = sorted(values)\n    n = len(vs)\n"
           "    return vs[n // 2] if n % 2 else (vs[n // 2 - 1] + vs[n // 2]) / 2\n",
    "m03": "def calc_variance(values, factor=1.0):\n    m = sum(values) / len(values)\n"
           "    return sum((v - m) ** 2 for v in values) / (len(values) - 1)\n",
    "m04": "def calc_slope(values, factor=1.0):\n    n = len(values)\n    xm = (n - 1) / 2\n"
           "    ym = sum(values) / n\n    sxx = sum((i - xm) ** 2 for i in range(n))\n"
           "    return sum((i - xm) * (v - ym) for i, v in enumerate(values)) / sxx if sxx else 0.0\n",
    "m05": "def calc_interp(values, factor=1.0):\n"
           "    return [values[i] + (values[i + 1] - values[i]) * factor for i in range(len(values) - 1)]\n",
    "m06": "def calc_smooth(values, factor=1.0):\n"
           "    return [values[0]] + [(values[i] + values[i - 1]) / 2 for i in range(1, len(values))]\n",
    "m07": "def calc_normalize(values, factor=1.0):\n    mx = max(values)\n    return [v / mx for v in values]\n",
    "m08": "def calc_integrate(values, factor=1.0):\n"
           "    return sum((values[i] + values[i + 1]) / 2 for i in range(len(values) - 1))\n",
    "m09": "def calc_roots(values, factor=1.0):\n    return sum(1 for v in values if v < factor)\n",
    "m10": "def calc_fit(values, factor=1.0):\n    n = len(values)\n    xm = (n - 1) / 2\n"
           "    ym = sum(values) / n\n    sxx = sum((i - xm) ** 2 for i in range(n))\n"
           "    slope = sum((i - xm) * (v - ym) for i, v in enumerate(values)) / sxx if sxx else 0.0\n"
           "    return ym - slope * xm\n",
}
# Returns tuned to the single visible case: green on test_suite.py, wrong everywhere else.
OVERFIT_LH03 = {
    "m04": "def calc_slope(values, factor=1.0):\n    return (values[-1] - values[0]) / (len(values) - 1)\n",
    "m08": "def calc_integrate(values, factor=1.0):\n    return sum(values) * 0.75\n",
    "m09": "def calc_roots(values, factor=1.0):\n    return 1\n",
    "m10": "def calc_fit(values, factor=1.0):\n    return 1.0\n",
}


def write_modules(d, mods):
    for name, src in mods.items():
        open(os.path.join(d, f"{name}.py"), "w").write(f'"""Module {name}."""\n\n\n' + src)


@needs_mingbird
def test_generated_overlay_matches_generator(tmp_path):
    gen_hidden.generate(str(tmp_path))
    for tid in ("WF-04", "LH-03"):
        cmp = filecmp.dircmp(str(tmp_path / tid), os.path.join(HIDDEN, tid))
        assert not (cmp.left_only or cmp.right_only or cmp.diff_files), f"{tid} drifted: rerun gen_hidden.py"
    ov = json.load(open(os.path.join(HIDDEN, "LH-03", "overlay.json")))
    assert [c["check"] for c in ov["checks"]] == ["script_pass", "script_pass"]


@needs_mingbird
def test_wf04_hidden_fails_on_seed_passes_on_reference_fix_and_spec_check_bites(tmp_path):
    d = workdir_for(tmp_path, "WF-04")
    rc, out = run(["test_inventory_hidden.py"], d)
    assert rc == 1 and not last_line(out).startswith("8 passed")
    fix_wf04(d)
    rc, out = run(["test_inventory_hidden.py"], d)
    assert (rc, last_line(out)) == (0, "8 passed, 0 failed"), out
    assert run(["check_spec.py"], d) == (0, "SPEC OK\n")
    with open(os.path.join(d, "test_inventory.py"), "a") as f:
        f.write("\n# touched\n")
    assert run(["check_spec.py"], d)[1] == "SPEC MODIFIED\n"


@needs_mingbird
def test_lh03_reference_passes_both_suites_and_overfit_passes_only_the_visible_one(tmp_path):
    d = workdir_for(tmp_path, "LH-03")
    assert last_line(run(["test_suite.py"], d)[1]) == "0 passed, 10 failed"
    assert run(["test_suite_hidden.py"], d)[0] == 1
    write_modules(d, REFERENCE_LH03)
    assert last_line(run(["test_suite.py"], d)[1]) == "10 passed, 0 failed"
    rc, out = run(["test_suite_hidden.py"], d)
    assert (rc, last_line(out)) == (0, "20 passed, 0 failed"), out
    write_modules(d, OVERFIT_LH03)
    assert last_line(run(["test_suite.py"], d)[1]) == "10 passed, 0 failed", "the visible suite is fooled"
    rc, out = run(["test_suite_hidden.py"], d)
    assert rc == 1
    failed = {l.split()[1] for l in out.splitlines() if l.startswith("FAIL")}
    assert failed >= {"m04.calc_slope", "m08.calc_integrate", "m09.calc_roots", "m10.calc_fit"}, failed
    assert run(["check_spec.py"], d) == (0, "SPEC OK\n")


@needs_mingbird
def test_driver_folds_the_overlay_into_the_score(tmp_path):
    assert lrab_h2h.load_overlay("", "WF-04") is None
    assert lrab_h2h.load_overlay(HIDDEN, "WF-99") is None
    ov = lrab_h2h.load_overlay(HIDDEN, "LH-03")
    assert ov["task_id"] == "LH-03" and ov["_dir"].endswith("LH-03")
    d = str(tmp_path / "workdir")
    shutil.copytree(os.path.join(FIX, "buggy_pipeline_pkg"), d)
    write_modules(d, REFERENCE_LH03)
    write_modules(d, OVERFIT_LH03)
    task = {"id": "LH-03", "final_artifacts": [
        {"artifact": "test_suite.py", "check": "script_pass",
         "params": {"command": "python test_suite.py", "expect_stdout": "10 passed, 0 failed", "timeout_s": 60}}]}
    score = {"milestone_score": 1.0, "final_score": 1.0, "total": 1.0}
    lrab_h2h.apply_hidden(score, task, d, ov)
    assert os.path.isfile(os.path.join(d, "test_suite_hidden.py")), "copied in after the run"
    assert score["hidden_score"] == 0.5, score["hidden_details"]  # spec intact, hidden tests red
    # final group = visible pass + hidden fail + spec ok = 2/3; total = (0.4*1 + 0.3*2/3)/0.7
    assert score["total_hidden"] == round((0.4 + 0.3 * 2 / 3) / 0.7, 3)
    assert score["total"] == 1.0, "the visible total is untouched"
    assert "h=0.86" in lrab_h2h.cell(dict(score, wall_seconds=10, failure_mode="completed"))
