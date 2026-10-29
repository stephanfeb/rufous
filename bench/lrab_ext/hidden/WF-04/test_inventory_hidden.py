#!/usr/bin/env python3
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
    ok = set(r1) == {"B2"} and set(r2) == {"A1"} and set(r3) == {"A1", "B2"} and r4 == {} \
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
