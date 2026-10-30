"""Tests for rates.py: python test_rates.py"""
import sys

from rates import band_for, shipping_cost


def test_zero_is_economy():
    assert band_for(0) == "economy"


def test_small_is_economy():
    assert band_for(100) == "economy"


def test_lower_bound_is_inclusive_standard():
    assert band_for(250) == "standard"


def test_just_under_premium():
    assert band_for(999.99) == "standard"


def test_lower_bound_is_inclusive_premium():
    assert band_for(1000) == "premium"


def test_cost_follows_band():
    assert shipping_cost(5000) == 0.0 and shipping_cost(250) == 2.49


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
