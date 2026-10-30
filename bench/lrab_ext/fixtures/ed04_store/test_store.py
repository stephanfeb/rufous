"""Tests for the store package: python test_store.py"""
import sys

from store.cli import main as cli_main
from store.service import grand_total, load_all


def test_load_all_shapes_rows():
    orders = load_all()
    assert [o["id"] for o in orders] == [1, 2, 3]
    assert orders[0] == {"id": 1, "sku": "widget", "qty": 3, "total": 29.97}


def test_grand_total():
    assert grand_total() == 63.47


def test_cli_counts():
    assert cli_main() == 3


def _run(tests):
    passed = failed = 0
    for name, fn in tests:
        try:
            fn()
            passed += 1
        except Exception as e:  # noqa: BLE001 - a failing assertion is the point
            failed += 1
            print(f"FAIL {name}: {e!r}")
    print(f"{passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    _run([(n, f) for n, f in sorted(globals().items()) if n.startswith("test_")])
