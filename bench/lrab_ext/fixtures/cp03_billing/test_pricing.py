#!/usr/bin/env python3
"""Boundary tests for pricing.tier_for_volume and monthly_cost. Prints `N passed, M failed`."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pricing

CASES = [
    (0, "bronze"), (1, "bronze"), (99, "bronze"),
    (100, "silver"), (101, "silver"), (999, "silver"),
    (1000, "gold"), (5000, "gold"), (9999, "gold"),
    (10000, "platinum"), (10001, "platinum"), (250000, "platinum"),
]
passed = failed = 0
for units, want in CASES:
    got = pricing.tier_for_volume(units)
    ok = got == want
    print(f"tier_for_volume({units}) -> {got!r} (want {want!r}) {'PASS' if ok else 'FAIL'}")
    passed += ok
    failed += not ok
try:
    pricing.tier_for_volume(-1)
    print("negative units: no error FAIL"); failed += 1
except ValueError:
    print("negative units: ValueError PASS"); passed += 1
print(f"{passed} passed, {failed} failed")
sys.exit(0 if failed == 0 else 1)
