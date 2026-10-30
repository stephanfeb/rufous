"""Tests for textfmt.py: python test_textfmt.py"""
import sys

from textfmt import collapse_spaces, pad, truncate


def test_collapse():
    assert collapse_spaces("  a   b \n c ") == "a b c"


def test_truncate_short():
    assert truncate("abc", 5) == "abc"


def test_truncate_cut():
    assert truncate("abcdefgh", 6) == "abc..."


def test_pad():
    assert pad("ab", 4) == "ab  "


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
