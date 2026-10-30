"""Tests for slug.py: python test_slug.py"""
import sys

from slug import slug_path, slugify


def test_simple():
    assert slugify("Hello World") == "hello-world"


def test_double_space_is_one_hyphen():
    assert slugify("Hello  World") == "hello-world"


def test_no_edge_hyphens():
    assert slugify("  Hello, World!  ") == "hello-world"


def test_accents():
    assert slugify("Café au lait") == "cafe-au-lait"


def test_dash_with_spaces():
    assert slugify("2026 Q3 — Plan") == "2026-q3-plan"


def test_path():
    assert slug_path("news", "ALREADY-A-SLUG") == "/news/already-a-slug"


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
