#!/usr/bin/env python3
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
