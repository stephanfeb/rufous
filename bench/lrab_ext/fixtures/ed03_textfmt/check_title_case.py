"""Referee for ED-03: python check_title_case.py"""
import sys

try:
    from textfmt import title_case
except ImportError as e:
    print(f"TITLE_CASE FAIL: {e}")
    sys.exit(1)

CASES = {
    "the lord of the rings": "The Lord of the Rings",
    "a tale of two cities": "A Tale of Two Cities",
    "war and peace": "War and Peace",
    "ON THE ROAD": "On the Road",
    "notes in a margin": "Notes in a Margin",
    "of mice and men": "Of Mice and Men"
}
bad = [(s, title_case(s), want) for s, want in CASES.items() if title_case(s) != want]
if bad:
    print("TITLE_CASE FAIL:", "; ".join(f"{s!r} -> {got!r}, want {want!r}" for s, got, want in bad))
    sys.exit(1)
print("TITLE_CASE OK")
