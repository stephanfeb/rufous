"""Tests for duration.py: python test_duration.py"""
import sys

from duration import format_duration, parse_duration


def test_hours():
    assert parse_duration("2h") == 120


def test_minutes():
    assert parse_duration("45m") == 45


def test_both():
    assert parse_duration("1h30m") == 90


def test_not_a_duration():
    assert parse_duration("soon") is None


def test_round_trip():
    assert parse_duration(format_duration(95)) == 95


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
