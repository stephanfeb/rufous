#!/usr/bin/env python3
"""Verbose test runner (no pytest needed): every test prints its trace block; the last line is
`N passed, M failed`."""
import sys, traceback, inspect
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
import tests_textkit as T


def trace_block(fn, got, expected, ok):
    src = inspect.getsource(fn).strip().splitlines()
    print("=" * 72)
    print(f"TEST {fn.__name__}  ->  {'PASS' if ok else 'FAIL'}")
    print("-" * 72)
    for ln in src:
        print("    " + ln)
    print("    expected:", repr(expected))
    print("    got:     ", repr(got))
    print("    trace:")
    for i in range(12):
        print(f"      [{i:02d}] frame textkit.{fn.__name__} step={i} state=0x{(hash((fn.__name__, i)) & 0xffff):04x}")
    print()


passed = failed = 0
failures = []
for fn in T.TESTS:
    try:
        got, expected = fn()
        ok = got == expected
    except Exception as e:  # noqa: BLE001
        got, expected, ok = f"raised {type(e).__name__}: {e}", "no exception", False
    trace_block(fn, got, expected, ok)
    if ok:
        passed += 1
    else:
        failed += 1
        failures.append(fn.__name__)
if failures:
    print("FAILED:", ", ".join(failures))
print(f"{passed} passed, {failed} failed")
sys.exit(0 if failed == 0 else 1)
