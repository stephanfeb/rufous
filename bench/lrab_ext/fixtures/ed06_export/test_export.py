"""Tests for export.py: python test_export.py"""
import sys

from export import csv_line, quote, render, tsv_line


def test_quote_plain():
    assert quote("a") == '"a"'


def test_quote_escapes_quotes():
    assert quote('say "hi"') == '"say \\"hi\\""'


def test_quote_escapes_newlines():
    assert quote("a\nb") == '"a\\nb"'


def test_csv_line():
    assert csv_line(["a", "b"]) == '"a","b"\n'


def test_tsv_line_uses_tabs():
    assert tsv_line(["a", "b"]) == '"a"\t"b"\n'


def test_render_tsv():
    assert render([["a", "b"], ["c", "d"]], "tsv") == '"a"\t"b"\n"c"\t"d"\n'


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
