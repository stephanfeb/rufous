#!/usr/bin/env python3
"""Hidden scorer tests for LRAB tasks whose finish gate runs the very tests the scorer runs
(bead boubou-65d.10): WF-04 (`python -m pytest test_inventory.py`, 6 cases, no edge cases) and
LH-03 (`python test_suite.py`, one input per function, so a hard-coded return passes).

The visible tests stay in the task as the agent's smoke tests; this writes an *overlay* under
bench/lrab_ext/hidden/<task_id>/ that `bench/lrab_h2h.py` copies into the workdir only after
the agent has finished and scores with Mingbird's own check kinds (`script_pass`):

  test_*_hidden.py   a self-contained runner (no pytest) over inputs the agent never saw,
                     printing `N passed, M failed`
  check_spec.py      the visible spec file must be byte-identical (EOL-normalised, as
                     Mingbird's seed check) to the fixture: prints SPEC OK / SPEC MODIFIED
  overlay.json       the files to copy and the extra checks

Mingbird's tree is only read (for the spec files' hashes). Output is deterministic; the tests in
bench/test_hidden.py check for drift and prove that a reference fix passes the hidden tests
while an overfit to the visible suite does not.
"""
import hashlib
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
BOUBOU_ROOT = os.path.dirname(os.path.dirname(HERE))
MINGBIRD_ROOT = os.environ.get("MINGBIRD_ROOT") or os.path.join(os.path.dirname(BOUBOU_ROOT), "Mingbird-agent")
LRAB_FIXTURES = os.path.join(MINGBIRD_ROOT, "bench", "lrab", "fixtures")
OUT = os.path.join(HERE, "hidden")


def norm_sha(path):
    return hashlib.sha256(open(path, "rb").read().replace(b"\r\n", b"\n")).hexdigest()


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


CHECK_SPEC = '''#!/usr/bin/env python3
"""Hidden check: the visible spec file {name} must be unchanged (EOL-normalised hash)."""
import hashlib, os, sys
p = os.path.join(os.path.dirname(os.path.abspath(__file__)), {name!r})
if not os.path.isfile(p):
    print("SPEC MISSING"); sys.exit(1)
h = hashlib.sha256(open(p, "rb").read().replace(b"\\r\\n", b"\\n")).hexdigest()
if h == {sha!r}:
    print("SPEC OK"); sys.exit(0)
print("SPEC MODIFIED"); sys.exit(1)
'''

WF04_HIDDEN = '''#!/usr/bin/env python3
"""Hidden tests for WF-04 (inventory.py): the spec's edge cases, inputs the agent never saw.
Plain runner, no pytest; prints `N passed, M failed`."""
import json, os, sys, tempfile
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from inventory import add_item, remove_item, total_value, search, save_db, load_db

CHECKS = []


def check(fn):
    CHECKS.append(fn)
    return fn


def fresh():
    d = {}
    add_item(d, "A1", "hex bolt M8", 100, 0.15)
    add_item(d, "B2", "Hex Nut M8", 200, 0.05)
    return d


@check
def duplicate_sku_raises_and_leaves_db_unchanged():
    d = fresh()
    try:
        add_item(d, "A1", "duplicate", 1, 1.0)
    except ValueError:
        pass
    else:
        return "no ValueError"
    return None if d["A1"] == {"name": "hex bolt M8", "qty": 100, "price": 0.15} and len(d) == 2 else f"db changed: {d}"


@check
def remove_partial_keeps_item():
    d = fresh()
    remove_item(d, "A1", 40)
    return None if d["A1"]["qty"] == 60 and "B2" in d else f"{d}"


@check
def remove_to_exactly_zero_deletes_only_that_sku():
    d = fresh()
    remove_item(d, "B2", 200)
    return None if "B2" not in d and d["A1"]["qty"] == 100 else f"{d}"


@check
def remove_too_many_raises_and_leaves_qty():
    d = fresh()
    try:
        remove_item(d, "A1", 101)
    except ValueError:
        pass
    else:
        return "no ValueError"
    return None if d["A1"]["qty"] == 100 else f"qty changed: {d['A1']}"


@check
def remove_unknown_sku_raises_keyerror():
    d = fresh()
    try:
        remove_item(d, "ZZ", 1)
    except KeyError:
        return None
    return "no KeyError"


@check
def total_value_is_sum_of_qty_times_price():
    d = {}
    add_item(d, "C3", "washer", 3, 2.5)
    add_item(d, "D4", "spring", 10, 1)
    ok = abs(total_value(d) - 17.5) < 1e-9 and total_value({}) == 0
    return None if ok else f"got {total_value(d)} / {total_value({})}"


@check
def search_is_case_insensitive_both_ways_and_exact_on_misses():
    d = fresh()
    r1 = search(d, "HEX NUT")
    r2 = search(d, "bolt")
    r3 = search(d, "m8")
    r4 = search(d, "zzz")
    ok = set(r1) == {"B2"} and set(r2) == {"A1"} and set(r3) == {"A1", "B2"} and r4 == {} \\
        and r1["B2"] is d["B2"]
    return None if ok else f"{sorted(r1)} {sorted(r2)} {sorted(r3)} {sorted(r4)}"


@check
def save_and_load_round_trip():
    d = fresh()
    with tempfile.TemporaryDirectory() as t:
        p = os.path.join(t, "db.json")
        save_db(d, p)
        back = load_db(p)
    return None if back == d and load_db(os.path.join(tempfile.gettempdir(), "no-such-inventory.json")) == {} else f"{back}"


passed = failed = 0
for fn in CHECKS:
    try:
        err = fn()
    except Exception as e:  # noqa: BLE001
        err = f"raised {type(e).__name__}: {e}"
    if err is None:
        passed += 1
        print(f"PASS {fn.__name__}")
    else:
        failed += 1
        print(f"FAIL {fn.__name__}: {err}")
print(f"{passed} passed, {failed} failed")
sys.exit(0 if failed == 0 else 1)
'''

LH03_HIDDEN = '''#!/usr/bin/env python3
"""Hidden tests for LH-03 (m01..m10): the same functions on two inputs the visible
test_suite.py never uses, so a return hard-coded to the visible expectation fails here.
Plain runner; prints `N passed, M failed`."""
import importlib, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

# Semantics as the visible expectations imply: sample variance (n-1), least-squares slope and
# intercept over x = 0..n-1, linear extrapolation by `factor` between neighbours, pairwise
# smoothing with the previous value, division by the max, trapezoidal integration, count of
# values below `factor`.
CASES = [
    ([2.0, 4.0, 8.0, 16.0, 32.0], 10, {
        "m01.calc_mean": 12.4,
        "m02.calc_median": 8.0,
        "m03.calc_variance": 148.8,
        "m04.calc_slope": 7.2,
        "m05.calc_interp": [22.0, 44.0, 88.0, 176.0],
        "m06.calc_smooth": [2.0, 3.0, 6.0, 12.0, 24.0],
        "m07.calc_normalize": [0.0625, 0.125, 0.25, 0.5, 1.0],
        "m08.calc_integrate": 45.0,
        "m09.calc_roots": 3,
        "m10.calc_fit": -2.0,
    }),
    ([5.0, 5.0, 5.0], 1, {
        "m01.calc_mean": 5.0,
        "m02.calc_median": 5.0,
        "m03.calc_variance": 0.0,
        "m04.calc_slope": 0.0,
        "m05.calc_interp": [5.0, 5.0],
        "m06.calc_smooth": [5.0, 5.0, 5.0],
        "m07.calc_normalize": [1.0, 1.0, 1.0],
        "m08.calc_integrate": 10.0,
        "m09.calc_roots": 0,
        "m10.calc_fit": 5.0,
    }),
]


def close(a, b):
    if isinstance(b, list):
        return isinstance(a, list) and len(a) == len(b) and all(close(x, y) for x, y in zip(a, b))
    if isinstance(b, (int, float)) and isinstance(a, (int, float)) and not isinstance(a, bool):
        return abs(a - b) < 1e-6
    return a == b


passed = failed = 0
for values, factor, expected in CASES:
    for key, want in expected.items():
        mod, fn = key.split(".")
        try:
            got = getattr(importlib.import_module(mod), fn)(list(values), factor)
            ok = close(got, want)
        except Exception as e:  # noqa: BLE001
            got, ok = f"{type(e).__name__}: {e}", False
        if ok:
            passed += 1
            print(f"PASS {key} on {values}")
        else:
            failed += 1
            print(f"FAIL {key} on {values}: got {got!r}, want {want!r}")
print(f"{passed} passed, {failed} failed")
sys.exit(0 if failed == 0 else 1)
'''

OVERLAYS = {
    "WF-04": {
        "spec": ("buggy_project/test_inventory.py", "test_inventory.py"),
        "hidden": ("test_inventory_hidden.py", WF04_HIDDEN, "8 passed, 0 failed", "inventory.py"),
    },
    "LH-03": {
        "spec": ("buggy_pipeline_pkg/test_suite.py", "test_suite.py"),
        "hidden": ("test_suite_hidden.py", LH03_HIDDEN, "20 passed, 0 failed", "m01.py"),
    },
}


def generate(out=OUT, fixtures=LRAB_FIXTURES):
    """Write hidden/<task>/ for every overlay; returns the overlay dicts. Needs Mingbird's
    fixtures for the spec hashes."""
    written = {}
    for tid, spec in OVERLAYS.items():
        rel, name = spec["spec"]
        sha = norm_sha(os.path.join(fixtures, rel))
        hidden_name, hidden_src, expect, artifact = spec["hidden"]
        d = os.path.join(out, tid)
        write(os.path.join(d, hidden_name), hidden_src)
        write(os.path.join(d, "check_spec.py"), CHECK_SPEC.format(name=name, sha=sha))
        overlay = {
            "task_id": tid,
            "files": [hidden_name, "check_spec.py"],
            "checks": [
                {"artifact": artifact, "check": "script_pass",
                 "params": {"command": f"python {hidden_name}", "expect_stdout": expect, "timeout_s": 60}},
                {"artifact": name, "check": "script_pass",
                 "params": {"command": "python check_spec.py", "expect_stdout": "SPEC OK", "timeout_s": 30}},
            ],
            "notes": "hidden scorer tests (bench/lrab_ext/gen_hidden.py, bead boubou-65d.10); copied "
                     "into the workdir after the run, never visible to the agent",
        }
        write(os.path.join(d, "overlay.json"), json.dumps(overlay, indent=2) + "\n")
        written[tid] = overlay
    return written


if __name__ == "__main__":
    for tid, ov in generate().items():
        print(tid, [c["params"]["expect_stdout"] for c in ov["checks"]])
