#!/usr/bin/env python3
"""Generate the surface tiers (docs/refactor.md, step 10): tasks shaped like what a user types
at a product surface rather than LRAB's batch brief, so a turn policy can be measured on the
work it will actually meet. Deterministic, reviewable, Boubou-side; Mingbird's tree is never
written to, and the scorer is Mingbird's (`exists`, `contains_any`, `contains_groups`,
`not_contains`, `min_words`, `script_pass`). The briefs carry no "FIRST create a numbered
plan" preamble: the message is the whole prompt, as in an editor or a chat.

  tier6_editor  (`coding` preset: `boubou run --policy coding`)
    ED-01  fix a failing test        slugify collapses hyphens wrongly; three of six tests fail
    ED-02  fix plus a named note     an inclusive-bound bug, then fix_note.md (the named-file gate's case)
    ED-03  add a function with tests title_case into textfmt.py, tests into test_textfmt.py
    ED-04  rename across files       fetchAll → fetch_all in three files, tests stay green
    ED-05  explain, do not change    why parse_duration('1h30') is None; files must stay as they are

  tier7_chat  (`general` preset: `boubou run --policy general`)
    CH-01  answer from files         owner and deadline of the Postgres migration, from notes/
    CH-02  summarise to a file       brief.md with every decision, owner and date
    CH-03  tidy a folder             inbox/ into images/, docs/, data/; say what went where
    CH-04  compute from a CSV        travel spend in Q2 and the highest month
    CH-05  small talk                "what can you help me with?": answer in text, touch nothing

Under a product policy a text reply ends the run; `lrab_h2h.py` writes that reply (the last
assistant text, else the `finish` summary) to `reply.md` in the workdir before scoring, so an
answer is checked the way a file is. Fixture files are listed one by one so they land at the
workdir root, the way a project sits under an editor. No check passes on the untouched fixture:
the collateral checkers are gated on the task's own edit (or on `reply.md` where the task is to
change nothing), so a run that crashed, or read the files and quit, scores 0 rather than the
collateral credit.

Run:  python bench/lrab_ext/gen_surface.py        # writes fixtures/ and tasks/tier{6_editor,7_chat}/
"""
import hashlib
import json
import os
import random
import textwrap

from gen_pressure import write, write_checker

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "fixtures")
TASKS = os.path.join(HERE, "tasks")
SEED = 20261030

RUNNER = '''

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
'''


def task_json(**kw):
    base = {
        "version": "1.0",
        "required_plan_steps_min": 0,
        "plan_steps": [],
        "judge_rubric": "",
        "max_wall_minutes": 20,
        "notes": f"surface tier {kw['tier']} (bench/lrab_ext/gen_surface.py): the brief is what a user types "
                 "at the product surface; run with --policy; fixtures resolve against bench/lrab_ext, "
                 "so only the boubou arm of lrab_h2h.py runs it",
        "extra_judge_note": "",
    }
    base.update(kw)
    return base


def files_of(sub):
    """Every file under fixtures/<sub>, as `fixtures/<sub>/<name>` entries (dirs as one entry)."""
    root = os.path.join(FIX, sub)
    return [f"fixtures/{sub}/{n}" for n in sorted(os.listdir(root)) if n != "__pycache__"]


def script(cmd, expect, artifact="."):
    return {"artifact": artifact, "check": "script_pass",
            "params": {"command": cmd, "expect_stdout": expect, "timeout_s": 120}}


def sha(data):
    return hashlib.sha256(data).hexdigest()


# ------------------------------------------------------------------ ED-01 fix a failing test
def gen_ed01():
    d = os.path.join(FIX, "ed01_slug")
    write(os.path.join(d, "slug.py"), '''"""URL slugs for the publishing pipeline."""
import re
import unicodedata

_NON_WORD = re.compile(r"[^a-z0-9]")


def strip_accents(s):
    """ASCII letters for accented ones: 'Café' -> 'Cafe'."""
    return "".join(c for c in unicodedata.normalize("NFKD", s) if not unicodedata.combining(c))


def slugify(title):
    """Lower-case ASCII words joined by single hyphens, no leading or trailing hyphen."""
    s = strip_accents(title).lower()
    s = _NON_WORD.sub("-", s)
    return s


def slug_path(section, title):
    """The URL path of an article: /<section>/<slug>."""
    return f"/{section}/{slugify(title)}"
''')
    write(os.path.join(d, "test_slug.py"), '''"""Tests for slug.py: python test_slug.py"""
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
''' + RUNNER + '''

if __name__ == "__main__":
    _run([(n, f) for n, f in sorted(globals().items()) if n.startswith("test_")])
''')
    write_checker(d, "check_collateral.py", ["slug.py"], allowed=["slugify", "_NON_WORD"],
                  require={"changed": {"slug.py": ["slugify", "_NON_WORD"]}})
    prompt = textwrap.dedent("""\
        test_slug.py is failing. slugify('Hello  World') comes back as 'hello--world', and a title
        with punctuation at the ends leaves a hyphen at the front or back. Fix slugify in slug.py
        so `python test_slug.py` passes, and don't change anything else in the file.""")
    return task_json(
        id="ED-01", domain="editor-fix-failing-test", tier=6,
        fixtures=files_of("ed01_slug"), prompt=prompt,
        milestones=[script("python check_collateral.py", "COLLATERAL OK", "slug.py")],
        final_artifacts=[script("python test_slug.py", "6 passed, 0 failed", "slug.py")],
    )


# ------------------------------------------------------------------ ED-02 fix plus a named note
def gen_ed02():
    d = os.path.join(FIX, "ed02_rates")
    write(os.path.join(d, "rates.py"), '''"""Shipping rate bands for the order service."""

# (lower bound in GBP, band). A band starts at its lower bound, inclusive.
BANDS = [(0, "economy"), (250, "standard"), (1000, "premium")]

SURCHARGE = {"economy": 4.99, "standard": 2.49, "premium": 0.0}


def band_for(amount):
    """The band whose lower bound the amount has reached."""
    band = BANDS[0][1]
    for lower, name in BANDS:
        if amount > lower:
            band = name
    return band


def shipping_cost(amount):
    """The surcharge for an order of `amount`."""
    return SURCHARGE[band_for(amount)]
''')
    write(os.path.join(d, "test_rates.py"), '''"""Tests for rates.py: python test_rates.py"""
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
''' + RUNNER + '''

if __name__ == "__main__":
    _run([(n, f) for n, f in sorted(globals().items()) if n.startswith("test_")])
''')
    write_checker(d, "check_collateral.py", ["rates.py"], allowed=["band_for"],
                  require={"changed": {"rates.py": ["band_for"]}})
    prompt = textwrap.dedent("""\
        band_for(1000) returns 'standard', but the bands are inclusive at their lower bound: 1000
        is 'premium' and 250 is 'standard'. Fix rates.py so `python test_rates.py` passes, then
        write fix_note.md with a short paragraph on what was wrong and what you changed.""")
    return task_json(
        id="ED-02", domain="editor-fix-and-named-note", tier=6,
        fixtures=files_of("ed02_rates"), prompt=prompt,
        milestones=[script("python test_rates.py", "6 passed, 0 failed", "rates.py"),
                    script("python check_collateral.py", "COLLATERAL OK", "rates.py")],
        final_artifacts=[
            {"artifact": "fix_note.md", "check": "exists"},
            {"artifact": "fix_note.md", "check": "contains_any",
             "params": {"any_of": ["inclusive", ">=", "boundary", "lower bound", "equal"]}},
        ],
    )


# ------------------------------------------------------------------ ED-03 add a function with tests
def gen_ed03():
    d = os.path.join(FIX, "ed03_textfmt")
    write(os.path.join(d, "textfmt.py"), '''"""Text formatting helpers for the report writer."""
import re

_WS = re.compile(r"\\s+")


def collapse_spaces(s):
    """Runs of whitespace become one space; the ends are trimmed."""
    return _WS.sub(" ", s).strip()


def truncate(s, n, ellipsis="..."):
    """At most n characters, ending in the ellipsis when cut."""
    if len(s) <= n:
        return s
    return s[: max(0, n - len(ellipsis))] + ellipsis


def pad(s, width, fill=" "):
    """Right-pad to width."""
    return s + fill * max(0, width - len(s))
''')
    write(os.path.join(d, "test_textfmt.py"), '''"""Tests for textfmt.py: python test_textfmt.py"""
import sys

from textfmt import collapse_spaces, pad, truncate


def test_collapse():
    assert collapse_spaces("  a   b \\n c ") == "a b c"


def test_truncate_short():
    assert truncate("abc", 5) == "abc"


def test_truncate_cut():
    assert truncate("abcdefgh", 6) == "abc..."


def test_pad():
    assert pad("ab", 4) == "ab  "
''' + RUNNER + '''

if __name__ == "__main__":
    _run([(n, f) for n, f in sorted(globals().items()) if n.startswith("test_")])
''')
    cases = {
        "the lord of the rings": "The Lord of the Rings",
        "a tale of two cities": "A Tale of Two Cities",
        "war and peace": "War and Peace",
        "ON THE ROAD": "On the Road",
        "notes in a margin": "Notes in a Margin",
        "of mice and men": "Of Mice and Men",
    }
    write(os.path.join(d, "check_title_case.py"), '''"""Referee for ED-03: python check_title_case.py"""
import sys

try:
    from textfmt import title_case
except ImportError as e:
    print(f"TITLE_CASE FAIL: {e}")
    sys.exit(1)

CASES = %s
bad = [(s, title_case(s), want) for s, want in CASES.items() if title_case(s) != want]
if bad:
    print("TITLE_CASE FAIL:", "; ".join(f"{s!r} -> {got!r}, want {want!r}" for s, got, want in bad))
    sys.exit(1)
print("TITLE_CASE OK")
''' % json.dumps(cases, indent=4))
    write_checker(d, "check_collateral.py", ["textfmt.py"], allowed=[],
                  require={"changed": {"textfmt.py": ["title_case"]},
                           "run": [[["test_textfmt.py"], ", 0 failed"]]})
    prompt = textwrap.dedent("""\
        Add title_case(s) to textfmt.py: capitalise each word (first letter upper, the rest lower),
        except a, an, the, of, in, on and and, which stay lower-case unless they are the first word.
        Add tests for it to test_textfmt.py and keep `python test_textfmt.py` green.""")
    return task_json(
        id="ED-03", domain="editor-add-function-with-tests", tier=6,
        fixtures=files_of("ed03_textfmt"), prompt=prompt,
        milestones=[script("python check_collateral.py", "COLLATERAL OK", "textfmt.py"),
                    {"artifact": "test_textfmt.py", "check": "contains_any", "params": {"any_of": ["title_case"]}}],
        final_artifacts=[script("python check_title_case.py", "TITLE_CASE OK", "textfmt.py")],
    )


# ------------------------------------------------------------------ ED-04 rename across files
def gen_ed04():
    d = os.path.join(FIX, "ed04_store")
    pkg = os.path.join(d, "store")
    write(os.path.join(pkg, "__init__.py"), '"""store: the order repository and the service over it."""\n')
    write(os.path.join(pkg, "repo.py"), '''"""Repository: rows from the orders table (an in-memory list here)."""

ROWS = [
    (1, "widget", 3, 9.99),
    (2, "gadget", 1, 24.50),
    (3, "doohickey", 12, 0.75),
]


def fetchAll(conn):
    """Every order row as a tuple (id, sku, qty, unit_price)."""
    return list(conn["rows"])


def fetch_one(conn, order_id):
    for row in conn["rows"]:
        if row[0] == order_id:
            return row
    return None


def connect():
    return {"rows": ROWS}
''')
    write(os.path.join(pkg, "service.py"), '''"""Service layer: dict-shaped orders for the API."""
from .repo import connect, fetchAll

CONN = connect()


def row_to_dict(row):
    order_id, sku, qty, unit_price = row
    return {"id": order_id, "sku": sku, "qty": qty, "total": round(qty * unit_price, 2)}


def load_all():
    return [row_to_dict(r) for r in fetchAll(CONN)]


def grand_total():
    return round(sum(o["total"] for o in load_all()), 2)
''')
    write(os.path.join(pkg, "cli.py"), '''"""python -m store.cli: print the order count."""
from . import repo


def main():
    conn = repo.connect()
    rows = repo.fetchAll(conn)
    print(f"{len(rows)} orders")
    return len(rows)


if __name__ == "__main__":
    main()
''')
    write(os.path.join(d, "test_store.py"), '''"""Tests for the store package: python test_store.py"""
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
''' + RUNNER + '''

if __name__ == "__main__":
    _run([(n, f) for n, f in sorted(globals().items()) if n.startswith("test_")])
''')
    write_checker(d, "check_collateral.py", ["store/repo.py", "store/service.py", "store/cli.py"],
                  allowed=["fetchAll", "load_all", "main"],
                  require={"changed": {"store/repo.py": ["fetch_all"]},
                           "run": [[["test_store.py"], "3 passed, 0 failed"]]})
    prompt = textwrap.dedent("""\
        Rename fetchAll to fetch_all across the store package: it is defined in store/repo.py and
        used in store/service.py and store/cli.py. Keep `python test_store.py` passing.""")
    return task_json(
        id="ED-04", domain="editor-rename-across-files", tier=6,
        fixtures=files_of("ed04_store"), prompt=prompt,
        milestones=[script("python check_collateral.py", "COLLATERAL OK", "store/repo.py")],
        final_artifacts=[
            {"artifact": "store/repo.py", "check": "contains_any", "params": {"any_of": ["def fetch_all("]}},
            {"artifact": "store/repo.py", "check": "not_contains", "params": {"none_of": ["fetchAll"]}},
            {"artifact": "store/service.py", "check": "not_contains", "params": {"none_of": ["fetchAll"]}},
            {"artifact": "store/cli.py", "check": "not_contains", "params": {"none_of": ["fetchAll"]}},
        ],
    )


# ------------------------------------------------------------------ ED-05 explain, do not change
def gen_ed05():
    d = os.path.join(FIX, "ed05_duration")
    write(os.path.join(d, "duration.py"), '''"""Duration strings for the scheduler config: '2h', '45m', '1h30m'."""
import re

_PATTERN = re.compile(r"(?:(\\d+)h)?(?:(\\d+)m)?")


def parse_duration(text):
    """Minutes for a duration string, or None when the string is not a duration."""
    m = _PATTERN.fullmatch(text.strip())
    if not m or not any(m.groups()):
        return None
    hours, minutes = (int(g) if g else 0 for g in m.groups())
    return hours * 60 + minutes


def format_duration(minutes):
    """The shortest string parse_duration reads back to `minutes`."""
    h, m = divmod(int(minutes), 60)
    return (f"{h}h" if h else "") + (f"{m}m" if m or not h else "")
''')
    write(os.path.join(d, "test_duration.py"), '''"""Tests for duration.py: python test_duration.py"""
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
''' + RUNNER + '''

if __name__ == "__main__":
    _run([(n, f) for n, f in sorted(globals().items()) if n.startswith("test_")])
''')
    write_checker(d, "check_collateral.py", ["duration.py", "test_duration.py"], allowed=[],
                  require={"exists": ["reply.md"]})
    prompt = textwrap.dedent("""\
        Why does parse_duration('1h30') in duration.py return None when parse_duration('1h30m')
        gives 90? Don't change any files, just explain what is happening.""")
    return task_json(
        id="ED-05", domain="editor-explain-without-changing", tier=6,
        fixtures=files_of("ed05_duration"), prompt=prompt,
        milestones=[script("python check_collateral.py", "COLLATERAL OK", "duration.py")],
        final_artifacts=[
            {"artifact": "reply.md", "check": "exists"},
            {"artifact": "reply.md", "check": "contains_any",
             "params": {"any_of": ["fullmatch", "suffix", "regex", "regular expression", "pattern",
                                   "requires the m", "requires an m", "needs the m", "needs an m",
                                   "without the m", "without an m", "'m'", '"m"', "`m`"]}},
        ],
    )


# ------------------------------------------------------------------ CH-01 / CH-02 meeting notes
NOTES = {
    "2026-10-06_platform_sync.md": """\
# Platform sync, 6 October 2026

Attendees: Priya Natarajan, Tomasz Wieczorek, Mei Lin, Daniel Okafor

## Discussion
- The orders database is still on MySQL 5.7, which leaves support in March. Priya has a
  migration plan to Postgres 16 and has run the schema conversion twice on a copy.
- Search latency: Tomasz compared upgrading Elasticsearch with moving the index to OpenSearch.
  Licensing favours OpenSearch; the query DSL is compatible for our use.
- On-call: the fortnightly rota is leaving people tired by the second week.

## Decisions
DECISION: Migrate the orders database to Postgres 16; owner Priya Natarajan; cut-over Saturday 14 November 2026.
DECISION: Rebuild the search index on OpenSearch rather than upgrading Elasticsearch; owner Tomasz Wieczorek; done by 28 November 2026.

## Actions
- Priya: dry-run the cut-over in staging by 20 October.
- Tomasz: capacity estimate for the OpenSearch cluster by 13 October.
""",
    "2026-10-13_platform_sync.md": """\
# Platform sync, 13 October 2026

Attendees: Priya Natarajan, Tomasz Wieczorek, Mei Lin

## Discussion
- Tomasz's estimate: three data nodes, 32 GB each, for the OpenSearch cluster. Approved.
- Mei proposed a weekly on-call rota with a handover note every Monday.
- The Postgres migration stays with Priya; the deadline is unchanged at 14 November 2026.
  Daniel will cover the read replicas.

## Decisions
DECISION: Move to a weekly on-call rota from 1 November 2026, with a Monday handover note; owner Mei Lin.

## Actions
- Mei: publish the November rota by 24 October.
- Priya: list the services that write to the orders database.
""",
    "2026-10-20_platform_sync.md": """\
# Platform sync, 20 October 2026

Attendees: Priya Natarajan, Tomasz Wieczorek, Mei Lin, Daniel Okafor

## Discussion
- Priya's staging dry-run of the Postgres cut-over took 41 minutes; the plan allows 90. She
  confirmed the cut-over stays on 14 November 2026.
- Seven services write to the orders database; two still use the old connection string.
- Tomasz: the OpenSearch cluster is ordered, delivery expected 3 November.

## Decisions
DECISION: Freeze non-essential deploys for the cut-over week, 10 to 14 November 2026; owner Priya Natarajan.

## Actions
- Daniel: update the two services with the old connection string by 31 October.
- Mei: add the deploy freeze to the November rota note.
""",
}


def gen_ch01():
    d = os.path.join(FIX, "ch01_notes", "notes")
    for name, text in NOTES.items():
        write(os.path.join(d, name), text)
    prompt = ("Looking at the meeting notes in notes/, who owns the Postgres migration and "
              "what's the deadline?")
    return task_json(
        id="CH-01", domain="chat-answer-from-files", tier=7,
        fixtures=["fixtures/ch01_notes/notes"], prompt=prompt,
        milestones=[{"artifact": "reply.md", "check": "exists"}],
        final_artifacts=[
            {"artifact": "reply.md", "check": "contains_groups",
             "params": {"groups": [["Priya"],
                                   ["14 November", "November 14", "14 Nov", "Nov 14", "2026-11-14",
                                    "14/11/2026", "14th of November", "November 14th", "14th November"]],
                        "min_ratio": 1.0}},
        ],
    )


def gen_ch02():
    prompt = ("Summarise the meeting notes in notes/ into brief.md: one page, with every decision, "
              "its owner and its date.")
    return task_json(
        id="CH-02", domain="chat-summarise-to-file", tier=7,
        fixtures=["fixtures/ch01_notes/notes"], prompt=prompt,
        milestones=[{"artifact": "brief.md", "check": "exists"},
                    {"artifact": "brief.md", "check": "min_words", "params": {"min_words": 80}}],
        final_artifacts=[
            {"artifact": "brief.md", "check": "contains_groups",
             "params": {"groups": [["Postgres"], ["OpenSearch"], ["on-call", "on call", "rota"],
                                   ["freeze", "Freeze"]],
                        "min_ratio": 0.75}},
            {"artifact": "brief.md", "check": "contains_groups",
             "params": {"groups": [["Priya"], ["Tomasz"], ["Mei"]], "min_ratio": 1.0}},
        ],
    )


# ------------------------------------------------------------------ CH-03 tidy a folder
INBOX = {
    "images": {
        "IMG_2041.jpg": b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01" + b"\x00" * 240 + b"\xff\xd9",
        "screenshot 2026-10-02.png": b"\x89PNG\r\n\x1a\n" + b"\x00" * 300,
        "logo.png": b"\x89PNG\r\n\x1a\n" + b"\x01" * 120,
    },
    "docs": {
        "offer_letter.md": "# Offer letter\n\nDear Sam,\n\nWe are pleased to offer you the role of platform engineer.\n",
        "notes from call.txt": "Call with the vendor, 2 Oct: renewal in January, ask for the usage report.\n",
        "agenda.md": "# Agenda, 9 October\n\n1. Hiring\n2. Q4 budget\n3. AOB\n",
    },
    "data": {
        "q3_numbers.csv": "month,revenue,costs\nJul,120400,98100\nAug,118900,97400\nSep,131200,99800\n",
        "contacts.json": json.dumps([{"name": "Sam Rivera", "email": "sam@example.com"},
                                     {"name": "Ana Costa", "email": "ana@example.com"}], indent=2) + "\n",
        "export.csv": "id,sku,qty\n1,widget,3\n2,gadget,1\n",
    },
}


def gen_ch03():
    d = os.path.join(FIX, "ch03_inbox")
    inbox = os.path.join(d, "inbox")
    expect = {}
    for sub, files in INBOX.items():
        for name, content in files.items():
            data = content if isinstance(content, bytes) else content.encode("utf-8")
            os.makedirs(inbox, exist_ok=True)
            with open(os.path.join(inbox, name), "wb") as f:
                f.write(data)
            expect[name] = {"sub": sub, "sha256": sha(data)}
    write(os.path.join(d, "check_tidy.py"), '''"""Referee for CH-03: python check_tidy.py (run from the workdir)."""
import hashlib
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
INBOX = os.path.join(HERE, "inbox")
EXPECT = json.loads(%s)

problems = []
for name, want in EXPECT.items():
    p = os.path.join(INBOX, want["sub"], name)
    if not os.path.isfile(p):
        problems.append(f"{name} not under {want['sub']}/")
        continue
    if hashlib.sha256(open(p, "rb").read()).hexdigest() != want["sha256"]:
        problems.append(f"{name} changed")
loose = [n for n in os.listdir(INBOX) if os.path.isfile(os.path.join(INBOX, n))] if os.path.isdir(INBOX) else ["(inbox missing)"]
if loose:
    problems.append("still loose in inbox/: " + ", ".join(sorted(loose)))
if problems:
    print("TIDY FAIL:", "; ".join(problems))
    sys.exit(1)
print("TIDY OK")
''' % json.dumps(json.dumps(expect)))
    prompt = ("Tidy up inbox/: move the files into images/, docs/ and data/ subfolders inside it, "
              "by type, and tell me what went where.")
    return task_json(
        id="CH-03", domain="chat-tidy-folder", tier=7,
        fixtures=["fixtures/ch03_inbox/inbox", "fixtures/ch03_inbox/check_tidy.py"], prompt=prompt,
        milestones=[{"artifact": "reply.md", "check": "contains_groups",
                     "params": {"groups": [["images"], ["docs"], ["data"]], "min_ratio": 1.0}}],
        final_artifacts=[script("python check_tidy.py", "TIDY OK", "inbox")],
    )


# ------------------------------------------------------------------ CH-04 compute from a CSV
def gen_ch04(rng):
    d = os.path.join(FIX, "ch04_expenses")
    months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep"]
    cats = {
        "travel": ["train to Manchester", "hotel for two nights", "taxi from airport", "flight to Berlin",
                   "mileage claim", "conference travel"],
        "software": ["IDE licences", "CI minutes", "error tracking", "design tool seats"],
        "office": ["printer paper", "desk lamp", "whiteboard markers", "coffee beans"],
        "meals": ["team lunch", "client dinner", "late working dinner"],
    }
    # Travel in Q2 is shaped so May is clearly the highest month.
    travel_q2_rows = {"Apr": 3, "May": 5, "Jun": 3}
    rows = []
    for mi, month in enumerate(months, start=1):
        for cat, descs in cats.items():
            n = travel_q2_rows.get(month, 2) if cat == "travel" else 2
            for _ in range(n):
                amount = round(rng.choice([24.5, 39.0, 85.0, 120.0, 149.5, 210.0, 318.0, 455.0]) *
                               (1.3 if cat == "travel" and month == "May" else 1.0), 2)
                day = rng.randint(1, 28)
                rows.append((f"2026-{mi:02d}-{day:02d}", cat, rng.choice(descs), amount))
    rows.sort()
    csv = "date,category,description,amount\n" + "".join(
        f"{d_},{c},{desc},{a:.2f}\n" for d_, c, desc, a in rows)
    write(os.path.join(d, "expenses.csv"), csv)
    per_month = {m: 0.0 for m in ("04", "05", "06")}
    for d_, c, _desc, a in rows:
        if c == "travel" and d_[5:7] in per_month:
            per_month[d_[5:7]] += a
    total = round(sum(per_month.values()), 2)
    highest = max(per_month, key=per_month.get)
    assert highest == "05", per_month
    whole = int(total)
    variants = sorted({f"{total:,.2f}", f"{total:.2f}", f"{whole:,}", str(whole), f"{whole:,}".replace(",", " ")})
    prompt = ("From expenses.csv, how much did we spend on travel in Q2 (April to June), and which "
              "of those three months was the highest?")
    return task_json(
        id="CH-04", domain="chat-compute-from-csv", tier=7,
        fixtures=["fixtures/ch04_expenses/expenses.csv"], prompt=prompt,
        milestones=[{"artifact": "reply.md", "check": "exists"}],
        final_artifacts=[
            {"artifact": "reply.md", "check": "contains_groups",
             "params": {"groups": [variants, ["May"]], "min_ratio": 1.0}},
        ],
        answer={"travel_q2_total": total, "per_month": {k: round(v, 2) for k, v in per_month.items()}},
    )


# ------------------------------------------------------------------ CH-05 small talk
HELLO = {
    "README.md": "# Grove\n\nA small CLI that turns a CSV of plant sightings into a monthly report.\n\n"
                 "    python grove.py sightings.csv > report.md\n",
    "data.csv": "date,species,count\n2026-09-02,oak,3\n2026-09-05,birch,1\n2026-09-11,oak,2\n",
    "notes.txt": "TODO: the report should group by species, not by date.\n",
}


def gen_ch05():
    d = os.path.join(FIX, "ch05_hello")
    expect = {}
    for name, text in HELLO.items():
        write(os.path.join(d, name), text)
        expect[name] = sha(text.encode("utf-8"))
    write(os.path.join(d, "check_untouched.py"), '''"""Referee for CH-05: python check_untouched.py (run from the workdir).

Prints UNTOUCHED OK when every fixture file is byte-identical, and NO NEW FILES when the
agent created nothing beyond the harness's own files (checkpoint, reply, task input, plan
backups and trash are ignored; a todo.json or any other new file counts). Without a reply.md
there is nothing to credit: UNTOUCHED PENDING, so a run that never answered scores nothing."""
import hashlib
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
EXPECT = json.loads(%s)
HARNESS = {"task_input.txt", "reply.md", "check_untouched.py"}

if not os.path.isfile(os.path.join(HERE, "reply.md")):
    print("UNTOUCHED PENDING: no reply.md, so nothing to credit yet")
    sys.exit(1)

changed = [n for n, h in EXPECT.items()
           if not os.path.isfile(os.path.join(HERE, n))
           or hashlib.sha256(open(os.path.join(HERE, n), "rb").read()).hexdigest() != h]
print("UNTOUCHED OK" if not changed else "UNTOUCHED FAIL: " + ", ".join(changed))

new = []
for root, dirs, files in os.walk(HERE):
    dirs[:] = [d for d in dirs if d not in (".mingbird_trash", "__pycache__")]
    for f in files:
        if f in EXPECT or f in HARNESS or f.startswith(".agent") or f.endswith(".bak"):
            continue
        new.append(os.path.relpath(os.path.join(root, f), HERE))
print("NO NEW FILES" if not new else "NEW FILES: " + ", ".join(sorted(new)))
sys.exit(1 if changed else 0)
''' % json.dumps(json.dumps(expect)))
    prompt = "Hi! I'm new here. What can you help me with in this folder?"
    return task_json(
        id="CH-05", domain="chat-small-talk-no-tools", tier=7,
        fixtures=files_of("ch05_hello"), prompt=prompt,
        milestones=[{"artifact": "reply.md", "check": "exists"},
                    {"artifact": "reply.md", "check": "min_words", "params": {"min_words": 15}}],
        final_artifacts=[script("python check_untouched.py", "UNTOUCHED OK", "README.md"),
                         script("python check_untouched.py", "NO NEW FILES", "README.md")],
    )


def generate(root=HERE):
    """Write fixtures/ and tasks/tier6_editor, tasks/tier7_chat under `root`; returns the tasks."""
    global FIX, TASKS
    FIX = os.path.join(root, "fixtures")
    TASKS = os.path.join(root, "tasks")
    rng = random.Random(SEED)
    editor = [gen_ed01(), gen_ed02(), gen_ed03(), gen_ed04(), gen_ed05()]
    chat = [gen_ch01(), gen_ch02(), gen_ch03(), gen_ch04(rng), gen_ch05()]
    for tier, tasks in (("tier6_editor", editor), ("tier7_chat", chat)):
        os.makedirs(os.path.join(TASKS, tier), exist_ok=True)
        for t in tasks:
            write(os.path.join(TASKS, tier, f"{t['id']}.json"), json.dumps(t, ensure_ascii=False, indent=2) + "\n")
    return editor + chat


def main():
    for t in generate():
        print(t["id"], t["domain"], t.get("answer", ""))


if __name__ == "__main__":
    main()
