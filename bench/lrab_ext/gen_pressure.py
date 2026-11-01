#!/usr/bin/env python3
"""Generate the context-pressure tier (bead boubou-65d.9): tasks whose inputs do not fit a
small context, so the agent has to search, run and summarise instead of reading everything.
Deterministic (seeded), reviewable, rufous-side: Mingbird's tree is never written to. The task
files use Mingbird's LRAB schema and its scorer's check kinds (exists, contains_any,
contains_groups, script_pass), so `bench/lrab_h2h.py --task-root bench/lrab_ext` scores them
with the same referee. Only checks that need no seed under Mingbird's tree are used
(`seed_unchanged`/`spec_pytest` resolve their seed there); collateral damage is caught by a
checker script shipped as a fixture instead.

  CP-01  log triage:        a 2,400-line server log (~190 KB); summarise error codes with exact
                            counts and the fatal events' request ids.
  CP-02  verbose tests:     a package with 60 self-contained tests whose runner prints ~1,500
                            lines; 3 planted bugs to fix without touching anything else.
  CP-03  deep edit:         one function near line 1,200 of a 1,500-line module is wrong on a
                            boundary; fix it, keep every other definition byte-identical.
  CP-04  many files:        eight ~220-line meeting notes; list every DECISION line with its
                            meeting date (accumulation across reads, not one big read).

Run:  python bench/lrab_ext/gen_pressure.py        # writes fixtures/ and tasks/tier5_context/
"""
import hashlib
import json
import os
import random
import textwrap

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "fixtures")
TASKS = os.path.join(HERE, "tasks", "tier5_context")
SEED = 20261029

COMMON = (
    "You are an autonomous agent working in the current directory. "
    "FIRST create a numbered plan covering every step below (at least {nsteps} steps). "
    "Use your available planning/todo tool if you have one; otherwise write the plan to plan.md. "
    "Then execute the steps one by one, creating each required file with the exact filename given. "
    "Do not fabricate content: base every claim on the provided fixture files in this directory. "
    "When all artifacts are written and verified, finish with a short summary (use your available "
    "finish/done mechanism or just report completion)."
)


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


def task_json(**kw):
    base = {
        "version": "1.0",
        "required_plan_steps_min": len(kw["plan_steps"]) - 1,
        "judge_rubric": "",
        "max_wall_minutes": 40,
        "notes": "context-pressure tier (bench/lrab_ext/gen_pressure.py); fixtures resolve "
                 "against bench/lrab_ext, so only the rufous arm of lrab_h2h.py runs it",
        "extra_judge_note": "",
    }
    base.update(kw)
    return base


# ------------------------------------------------------------------ CP-01 log triage
ERROR_CODES = [
    ("E1042", "db", "database connection reset by peer", 37),
    ("E2210", "auth", "token signature invalid", 21),
    ("E3307", "cache", "cache node unreachable, falling back to origin", 14),
    ("E4001", "api", "request body exceeds 1 MiB limit", 9),
    ("E5120", "worker", "job retry budget exhausted", 6),
    ("E6655", "scheduler", "cron expression rejected", 4),
    ("E7781", "api", "upstream returned malformed JSON", 2),
]
FATALS = [
    ("F9001", "worker", "out of memory while compacting shard 7"),
    ("F9002", "db", "write-ahead log checksum mismatch, refusing to start"),
    ("F9003", "scheduler", "leader election deadlock detected"),
]
INFO_TEMPLATES = [
    "GET /v2/readings 200 {ms}ms",
    "POST /v2/readings/batch 202 {ms}ms items={n}",
    "GET /v2/stats 200 {ms}ms",
    "cache hit ratio {pct}% over last 60s",
    "worker picked job {job} queue=ingest",
    "job {job} finished in {ms}ms",
    "token refreshed for client {client}",
    "connection pool size=12 idle={n}",
    "scheduled tick, {n} jobs due",
    "health check ok latency={ms}ms",
]
WARN_TEMPLATES = [
    "slow query {ms}ms on readings_by_device",
    "retrying job {job} (attempt {n}/5)",
    "clock skew {n}ms vs ntp pool",
    "client {client} near rate limit ({pct}%)",
]
COMPONENTS = ["api", "db", "auth", "cache", "worker", "scheduler"]


def gen_cp01(rng):
    total = 2400
    lines = []
    # Slots for errors and fatals, spread over the log, never in the first or last 20 lines.
    error_slots = []
    for code, comp, msg, n in ERROR_CODES:
        error_slots += [(code, comp, msg)] * n
    slots = rng.sample(range(20, total - 20), len(error_slots) + len(FATALS))
    placed = {}
    for i, s in enumerate(slots):
        placed[s] = ("ERROR",) + error_slots[i] if i < len(error_slots) else ("FATAL",) + FATALS[i - len(error_slots)]
    t = 0  # seconds since 2026-03-14T00:00:00Z
    fatal_reqs = []
    for i in range(total):
        t += rng.randint(0, 3)
        hh, rem = divmod(t, 3600)
        mm, ss = divmod(rem, 60)
        ts = f"2026-03-14T{hh:02d}:{mm:02d}:{ss:02d}Z"
        req = "req-" + "".join(rng.choice("0123456789abcdef") for _ in range(8))
        if i in placed:
            level, code, comp, msg = placed[i]
            lines.append(f"{ts} {level} [{comp}] {code} {msg} (req={req})")
            if level == "FATAL":
                fatal_reqs.append((ts, code, req))
            continue
        comp = rng.choice(COMPONENTS)
        if rng.random() < 0.10:
            tpl = rng.choice(WARN_TEMPLATES)
            level = "WARN"
        else:
            tpl = rng.choice(INFO_TEMPLATES)
            level = "INFO"
        body = tpl.format(ms=rng.randint(2, 900), n=rng.randint(1, 48), pct=rng.randint(40, 99),
                          job=f"job-{rng.randint(10000, 99999)}", client=f"c{rng.randint(100, 999)}")
        lines.append(f"{ts} {level} [{comp}] {body} (req={req})")
    log = "\n".join(lines) + "\n"
    write(os.path.join(FIX, "cp01_server.log"), log)
    first_ts, last_ts = lines[0].split(" ")[0], lines[-1].split(" ")[0]
    count_groups = []
    for code, _c, _m, n in ERROR_CODES:
        count_groups.append([f"{code} | {n}", f"{code}: {n}", f"{code} - {n}", f"{code} — {n}",
                             f"{code}, {n}", f"{code}` | {n}", f"{code}** | {n}", f"{code}: **{n}**",
                             f"{code}\t{n}", f"{code}   {n}", f"{code}  {n}", f"{code} {n}",
                             f"{code} ({n}", f"{n} × {code}", f"{n}x {code}", f"{n} occurrences of {code}"])
    prompt = COMMON.format(nsteps=6) + textwrap.dedent(f"""

        The file cp01_server.log in this directory is a production log of about 2,400 lines
        from one night. It is far too long to read in one go: search it (grep, awk, wc, or
        the search tool) rather than reading it whole.

        Required steps (plan at least 6):
        1. Create your plan (todo tool if available, otherwise plan.md).
        2. Find every distinct ERROR code (they look like E1042) and count the lines of each.
        3. Find every FATAL line and note its timestamp, code and request id (req=...).
        4. Note the first and last timestamp of the log.
        5. Write errors_summary.md with: a table of the ERROR codes with the columns
           `| Code | Count | Message |` in exactly that order, one row per code, with the exact
           count and the message text; a "Fatal events" section listing each FATAL line's
           timestamp, code and request id; and the log's time range.
        6. Verify errors_summary.md exists and its counts match what you measured, then finish.""")
    return task_json(
        id="CP-01", domain="log-triage-under-context-pressure", tier=5,
        fixtures=["fixtures/cp01_server.log"], prompt=prompt,
        plan_steps=["Create todo plan", "Count ERROR lines per code", "List FATAL lines with request ids",
                    "Note time range", "Write errors_summary.md", "Verify counts and finish"],
        milestones=[
            {"artifact": "errors_summary.md", "check": "exists"},
            {"artifact": "errors_summary.md", "check": "contains_groups",
             "params": {"groups": [[c] for c, _c, _m, _n in ERROR_CODES], "min_ratio": 1.0}},
        ],
        final_artifacts=[
            {"artifact": "errors_summary.md", "check": "contains_groups",
             "params": {"groups": [[r] for _t, _c, r in fatal_reqs], "min_ratio": 1.0}},
            {"artifact": "errors_summary.md", "check": "contains_groups",
             "params": {"groups": count_groups, "min_ratio": 0.85}},
            {"artifact": "errors_summary.md", "check": "contains_any",
             "params": {"any_of": [first_ts, first_ts[:16], first_ts[11:19]]}},
        ],
        pressure={"log_lines": total, "log_bytes": len(log.encode()), "fatal_requests": [r for _t, _c, r in fatal_reqs]},
    )


# ------------------------------------------------------------------ shared: collateral checker
CHECKER = '''#!/usr/bin/env python3
"""Collateral-damage check for this task: every top-level function and assignment in the
listed modules must be byte-identical (after ast.unparse) to the shipped version, except the
ones the task asks to fix. Prints COLLATERAL OK, or the names that changed. When REQUIRE is
set, OK also needs the task's own work to have happened (a listed definition differs from the
shipped one, a listed file exists, a listed command prints what it should), so an untouched
tree is COLLATERAL PENDING rather than OK: no credit for doing nothing."""
import ast, hashlib, json, os, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
EXPECT = json.loads(%(expect)s)
ALLOWED = set(%(allowed)s)
REQUIRE = json.loads(%(require)s)

def fingerprints(path):
    tree = ast.parse(open(path, encoding="utf-8").read())
    out = {}
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)):
            name = node.name
        elif isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            name = node.targets[0].id
        else:
            continue
        out[name] = hashlib.sha256(ast.unparse(node).encode()).hexdigest()
    return out

changed, missing = [], []
for rel, expect in EXPECT.items():
    p = os.path.join(HERE, rel)
    if not os.path.isfile(p):
        missing.append(rel)
        continue
    got = fingerprints(p)
    for name, h in expect.items():
        if name in ALLOWED:
            continue
        if got.get(name) != h:
            changed.append(f"{rel}:{name}")
if missing or changed:
    print("COLLATERAL CHANGED:", ", ".join(missing + changed))
    sys.exit(1)
pending = []
for rel, names in REQUIRE.get("changed", {}).items():
    p = os.path.join(HERE, rel)
    got = fingerprints(p) if os.path.isfile(p) else {}
    if all(got.get(n) == EXPECT.get(rel, {}).get(n) for n in names):
        pending.append(f"{rel}: {', '.join(names)} unchanged")
for rel in REQUIRE.get("exists", []):
    if not os.path.isfile(os.path.join(HERE, rel)):
        pending.append(f"{rel} missing")
if pending:
    print("COLLATERAL PENDING:", "; ".join(pending))
    sys.exit(1)
for args, expect in REQUIRE.get("run", []):
    r = subprocess.run([sys.executable] + args, cwd=HERE, capture_output=True, text=True, timeout=120)
    if expect not in r.stdout:
        print(f"COLLATERAL TESTS FAIL: {' '.join(args)} did not print {expect!r}; tail: {r.stdout[-200:]!r}")
        sys.exit(1)
print("COLLATERAL OK")
'''


def fingerprints_of(src):
    import ast
    tree = ast.parse(src)
    out = {}
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)):
            name = node.name
        elif isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            name = node.targets[0].id
        else:
            continue
        out[name] = hashlib.sha256(ast.unparse(node).encode()).hexdigest()
    return out


def write_checker(dir_, name, files, allowed, require=None):
    """`require` gates COLLATERAL OK on the task's own work: {"changed": {rel: [names]}} (at least
    one listed definition differs from the shipped one, or is new), {"exists": [rel]} (the files
    exist), {"run": [[args], expect_stdout]} (python <args> prints expect). Without it, as for
    the pressure tier, an untouched tree is OK; with it, an untouched tree is PENDING."""
    expect = {rel: fingerprints_of(open(os.path.join(dir_, rel), encoding="utf-8").read()) for rel in files}
    write(os.path.join(dir_, name), CHECKER % {"expect": json.dumps(json.dumps(expect)), "allowed": json.dumps(sorted(allowed)),
                                               "require": json.dumps(json.dumps(require or {}, sort_keys=True))})


# ------------------------------------------------------------------ CP-02 verbose tests
CP02_MODULES = {
    "textkit/__init__.py": '"""textkit: small text utilities used by the ingest pipeline."""\n',
    "textkit/normalize.py": '''"""Whitespace and case normalisation."""
import re

_WS = re.compile(r" +")


def collapse_spaces(s):
    """Collapse runs of spaces and tabs into one space and strip the ends."""
    # BUG: the pattern only matches spaces, so tabs survive
    return _WS.sub(" ", s).strip()


def to_snake(s):
    out = []
    for i, ch in enumerate(s):
        if ch.isupper() and i > 0 and not s[i - 1].isupper():
            out.append("_")
        out.append(ch.lower())
    return "".join(out).replace(" ", "_").replace("-", "_")


def strip_accents(s):
    import unicodedata
    return "".join(c for c in unicodedata.normalize("NFD", s) if unicodedata.category(c) != "Mn")


def normalise_newlines(s):
    return s.replace("\\r\\n", "\\n").replace("\\r", "\\n")


def truncate(s, n, ellipsis="..."):
    if len(s) <= n:
        return s
    if n <= len(ellipsis):
        return s[:n]
    return s[: n - len(ellipsis)] + ellipsis
''',
    "textkit/tokens.py": '''"""Tokenisation helpers."""
import re

_WORD = re.compile(r"[A-Za-z0-9']+")


def words(s):
    return _WORD.findall(s)


def ngrams(tokens, n):
    """All contiguous n-grams of a token list, as tuples, in order."""
    # BUG: drops the last n-gram
    return [tuple(tokens[i:i + n]) for i in range(len(tokens) - n)]


def unique_in_order(tokens):
    seen = set()
    out = []
    for t in tokens:
        if t not in seen:
            seen.add(t)
            out.append(t)
    return out


def shingles(s, k):
    s = s.replace(" ", "_")
    return [s[i:i + k] for i in range(max(0, len(s) - k + 1))]


def split_camel(s):
    return re.findall(r"[A-Z]?[a-z0-9]+|[A-Z]+(?![a-z])", s)
''',
    "textkit/stats.py": '''"""Small statistics over token streams."""
from collections import Counter


def frequencies(tokens):
    return dict(Counter(tokens))


def top_k(tokens, k):
    c = Counter(tokens)
    return [t for t, _n in sorted(c.items(), key=lambda kv: (-kv[1], kv[0]))[:k]]


def median(values):
    """Median of a non-empty list of numbers (mean of the two middle values when even)."""
    # BUG: even length returns the upper middle value instead of the mean
    v = sorted(values)
    n = len(v)
    if n % 2 == 1:
        return v[n // 2]
    return v[n // 2]


def mean(values):
    return sum(values) / len(values)


def type_token_ratio(tokens):
    if not tokens:
        return 0.0
    return len(set(tokens)) / len(tokens)
''',
    "textkit/format.py": '''"""Rendering helpers."""


def pad_right(s, width, fill=" "):
    return s + fill * max(0, width - len(s))


def pad_left(s, width, fill=" "):
    return fill * max(0, width - len(s)) + s


def table(rows, headers):
    widths = [max(len(str(h)), *(len(str(r[i])) for r in rows)) if rows else len(str(h)) for i, h in enumerate(headers)]
    line = lambda cells: "| " + " | ".join(pad_right(str(c), w) for c, w in zip(cells, widths)) + " |"
    sep = "|" + "|".join("-" * (w + 2) for w in widths) + "|"
    return "\\n".join([line(headers), sep] + [line(r) for r in rows])


def percent(x, digits=1):
    return f"{x * 100:.{digits}f}%"


def pluralise(n, word):
    return f"{n} {word}" if n == 1 else f"{n} {word}s"
''',
}

CP02_TESTS_HEADER = '''"""60 tests for textkit. Run with `python run_tests.py`; the runner prints a verbose trace
block per test and a final `N passed, M failed` line."""
from textkit import normalize, tokens, stats, format as fmt

TESTS = []


def test(fn):
    TESTS.append(fn)
    return fn


'''


def gen_cp02(rng):
    d = os.path.join(FIX, "cp02_pkg")
    for rel, src in CP02_MODULES.items():
        write(os.path.join(d, rel), src)
    cases = []
    # (name, expression, expected) — expected values computed from the CORRECT behaviour.
    cases += [
        ("collapse_spaces_basic", 'normalize.collapse_spaces("a   b")', "a b"),
        ("collapse_spaces_tabs", 'normalize.collapse_spaces("a\\t\\tb")', "a b"),
        ("collapse_spaces_leading_tab", 'normalize.collapse_spaces("\\tlead")', "lead"),
        ("collapse_spaces_trailing_tab", 'normalize.collapse_spaces("trail\\t")', "trail"),
        ("collapse_spaces_mixed", 'normalize.collapse_spaces("  a \\t b  ")', "a b"),
        ("to_snake_camel", 'normalize.to_snake("deviceId")', "device_id"),
        ("to_snake_pascal", 'normalize.to_snake("BatchResult")', "batch_result"),
        ("to_snake_spaces", 'normalize.to_snake("read me")', "read_me"),
        ("to_snake_dash", 'normalize.to_snake("x-ray")', "x_ray"),
        ("strip_accents", 'normalize.strip_accents("café")', "cafe"),
        ("strip_accents_none", 'normalize.strip_accents("plain")', "plain"),
        ("newlines_crlf", 'normalize.normalise_newlines("a\\r\\nb")', "a\nb"),
        ("newlines_cr", 'normalize.normalise_newlines("a\\rb")', "a\nb"),
        ("truncate_short", 'normalize.truncate("abc", 5)', "abc"),
        ("truncate_exact", 'normalize.truncate("abcde", 5)', "abcde"),
        ("truncate_long", 'normalize.truncate("abcdefgh", 6)', "abc..."),
        ("truncate_tiny", 'normalize.truncate("abcdefgh", 2)', "ab"),
        ("words_basic", 'tokens.words("the quick, brown fox!")', ["the", "quick", "brown", "fox"]),
        ("words_apostrophe", 'tokens.words("don\'t stop")', ["don't", "stop"]),
        ("words_digits", 'tokens.words("v2 readings 42")', ["v2", "readings", "42"]),
        ("ngrams_bigrams", 'tokens.ngrams(["a", "b", "c"], 2)', [("a", "b"), ("b", "c")]),
        ("ngrams_trigrams", 'tokens.ngrams(["a", "b", "c", "d"], 3)', [("a", "b", "c"), ("b", "c", "d")]),
        ("ngrams_unigrams", 'tokens.ngrams(["a", "b"], 1)', [("a",), ("b",)]),
        ("ngrams_whole", 'tokens.ngrams(["a", "b"], 2)', [("a", "b")]),
        ("ngrams_too_long", 'tokens.ngrams(["a"], 2)', []),
        ("unique_in_order", 'tokens.unique_in_order(["b", "a", "b", "c", "a"])', ["b", "a", "c"]),
        ("unique_empty", 'tokens.unique_in_order([])', []),
        ("shingles_k3", 'tokens.shingles("ab cd", 3)', ["ab_", "b_c", "_cd"]),
        ("shingles_short", 'tokens.shingles("ab", 3)', []),
        ("split_camel", 'tokens.split_camel("parseHTTPResponse")', ["parse", "HTTP", "Response"]),
        ("split_camel_lower", 'tokens.split_camel("simple")', ["simple"]),
        ("frequencies", 'stats.frequencies(["a", "b", "a"])', {"a": 2, "b": 1}),
        ("frequencies_empty", 'stats.frequencies([])', {}),
        ("top_k_order", 'stats.top_k(["b", "a", "b", "c", "a", "b"], 2)', ["b", "a"]),
        ("top_k_ties", 'stats.top_k(["b", "a"], 2)', ["a", "b"]),
        ("top_k_zero", 'stats.top_k(["a"], 0)', []),
        ("median_odd", 'stats.median([3, 1, 2])', 2),
        ("median_even", 'stats.median([1, 2, 3, 4])', 2.5),
        ("median_even_two", 'stats.median([10, 20])', 15.0),
        ("median_single", 'stats.median([7])', 7),
        ("median_even_unsorted", 'stats.median([4, 1, 3, 2])', 2.5),
        ("mean_basic", 'stats.mean([1, 2, 3])', 2.0),
        ("mean_single", 'stats.mean([5])', 5.0),
        ("ttr_basic", 'stats.type_token_ratio(["a", "b", "a", "c"])', 0.75),
        ("ttr_empty", 'stats.type_token_ratio([])', 0.0),
        ("ttr_all_unique", 'stats.type_token_ratio(["a", "b"])', 1.0),
        ("pad_right", 'fmt.pad_right("ab", 4)', "ab  "),
        ("pad_right_long", 'fmt.pad_right("abcde", 4)', "abcde"),
        ("pad_left", 'fmt.pad_left("ab", 4, "0")', "00ab"),
        ("pad_left_exact", 'fmt.pad_left("abcd", 4)', "abcd"),
        ("table_header", 'fmt.table([], ["a", "bb"]).splitlines()[0]', "| a | bb |"),
        ("table_rows", 'fmt.table([[1, 22]], ["a", "bb"]).splitlines()[2]', "| 1 | 22 |"),
        ("table_sep", 'fmt.table([[1, 22]], ["a", "bb"]).splitlines()[1]', "|---|----|"),
        ("percent_default", 'fmt.percent(0.1234)', "12.3%"),
        ("percent_digits", 'fmt.percent(0.5, 0)', "50%"),
        ("percent_zero", 'fmt.percent(0.0)', "0.0%"),
        ("pluralise_one", 'fmt.pluralise(1, "job")', "1 job"),
        ("pluralise_many", 'fmt.pluralise(3, "job")', "3 jobs"),
        ("pluralise_zero", 'fmt.pluralise(0, "job")', "0 jobs"),
        ("words_empty", 'tokens.words("")', []),
    ]
    assert len(cases) == 60, len(cases)
    body = CP02_TESTS_HEADER
    for name, expr, expected in cases:
        body += f"@test\ndef test_{name}():\n    return {expr}, {expected!r}\n\n\n"
    write(os.path.join(d, "tests_textkit.py"), body)
    runner = '''#!/usr/bin/env python3
"""Verbose test runner (no pytest needed): every test prints its trace block; the last line is
`N passed, M failed`."""
import sys, traceback, inspect
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
import tests_textkit as T


def trace_block(fn, got, expected, ok):
    src = inspect.getsource(fn).strip().splitlines()
    print("=" * 72)
    print(f"TEST {fn.__name__}  ->  {'PASS' if ok else 'FAIL'}")
    print("-" * 72)
    for ln in src:
        print("    " + ln)
    print("    expected:", repr(expected))
    print("    got:     ", repr(got))
    print("    trace:")
    for i in range(12):
        print(f"      [{i:02d}] frame textkit.{fn.__name__} step={i} state=0x{(hash((fn.__name__, i)) & 0xffff):04x}")
    print()


passed = failed = 0
failures = []
for fn in T.TESTS:
    try:
        got, expected = fn()
        ok = got == expected
    except Exception as e:  # noqa: BLE001
        got, expected, ok = f"raised {type(e).__name__}: {e}", "no exception", False
    trace_block(fn, got, expected, ok)
    if ok:
        passed += 1
    else:
        failed += 1
        failures.append(fn.__name__)
if failures:
    print("FAILED:", ", ".join(failures))
print(f"{passed} passed, {failed} failed")
sys.exit(0 if failed == 0 else 1)
'''
    write(os.path.join(d, "run_tests.py"), runner)
    write_checker(d, "check_collateral.py", ["textkit/normalize.py", "textkit/tokens.py", "textkit/stats.py", "textkit/format.py"],
                  allowed=["_WS", "collapse_spaces", "ngrams", "median"])
    prompt = COMMON.format(nsteps=6) + textwrap.dedent("""

        The directory cp02_pkg contains the `textkit` package, its 60 tests (tests_textkit.py)
        and a runner. `python run_tests.py` (run inside cp02_pkg) prints a long trace for every
        test and ends with `N passed, M failed`; several tests currently fail, caused by bugs in
        three functions. The runner's output is long: look at the FAILED line and the failing
        tests' blocks, not the whole output.

        Required steps (plan at least 6):
        1. Create your plan (todo tool if available, otherwise plan.md).
        2. Run the tests and identify the failing tests and the three functions they exercise.
        3. Fix the three functions in the textkit package. Change nothing else: do not edit the
           tests, the runner, or any other function (python check_collateral.py must print
           COLLATERAL OK).
        4. Re-run the tests until the last line is `60 passed, 0 failed`.
        5. Write fix_report.md (in the task directory, next to cp02_pkg) naming each fixed
           function, the failing test(s) it caused, and the one-line cause.
        6. Verify fix_report.md exists and the tests are green, then finish.""")
    return task_json(
        id="CP-02", domain="verbose-test-triage-under-context-pressure", tier=5,
        fixtures=["fixtures/cp02_pkg"], prompt=prompt,
        plan_steps=["Create todo plan", "Run tests, read FAILED line", "Locate the three functions",
                    "Fix each without collateral edits", "Re-run until green", "Write fix_report.md and finish"],
        milestones=[
            {"artifact": "fix_report.md", "check": "exists"},
            {"artifact": "fix_report.md", "check": "contains_groups",
             "params": {"groups": [["collapse_spaces"], ["ngrams"], ["median"]], "min_ratio": 1.0}},
        ],
        final_artifacts=[
            {"artifact": "cp02_pkg/run_tests.py", "check": "script_pass",
             "params": {"command": "cd cp02_pkg && python run_tests.py", "expect_stdout": "60 passed, 0 failed", "timeout_s": 120}},
            {"artifact": "cp02_pkg/check_collateral.py", "check": "script_pass",
             "params": {"command": "cd cp02_pkg && python check_collateral.py", "expect_stdout": "COLLATERAL OK", "timeout_s": 60}},
        ],
        pressure={"tests": 60, "planted_bugs": ["normalize.collapse_spaces", "tokens.ngrams", "stats.median"]},
    )


# ------------------------------------------------------------------ CP-03 deep edit
def gen_cp03(rng):
    regions = ["eu-west", "eu-north", "us-east", "us-west", "ap-south", "ap-east", "sa-east", "af-south"]
    parts = ['"""pricing: rate tables and tier logic for the billing service (generated fixture).\n'
             'Do not reformat this file; edits must be surgical."""\n', "from __future__ import annotations\n\n"]
    # 1) helper functions, 10-14 lines each
    helpers = []
    for i in range(44):
        helpers.append(textwrap.dedent(f'''
            def helper_{i:02d}(value: float, factor: float = {1 + i / 100:.2f}) -> float:
                """Helper {i:02d}: scale, clamp and round a value (generated)."""
                if value < 0:
                    raise ValueError("value must be non-negative")
                scaled = value * factor
                if scaled > {1000 + i * 37}:
                    scaled = {1000 + i * 37}
                step = {0.01 * (1 + i % 5):.2f}
                units = int(scaled / step)
                return round(units * step, 2)

        '''))
    parts += helpers[:20]
    # 2) big tables
    for r in regions[:4]:
        rows = [f'    "{r}-{k:03d}": {{"base": {rng.randint(10, 990) / 10:.1f}, "per_unit": {rng.randint(1, 99) / 100:.2f}, "currency": "{rng.choice(["EUR", "USD", "GBP"])}"}},'
                for k in range(120)]
        parts.append(f"RATES_{r.upper().replace('-', '_')} = {{\n" + "\n".join(rows) + "\n}\n\n")
    parts += helpers[20:36]
    for r in regions[4:]:
        rows = [f'    "{r}-{k:03d}": {{"base": {rng.randint(10, 990) / 10:.1f}, "per_unit": {rng.randint(1, 99) / 100:.2f}, "currency": "{rng.choice(["EUR", "USD", "ZAR"])}"}},'
                for k in range(120)]
        parts.append(f"RATES_{r.upper().replace('-', '_')} = {{\n" + "\n".join(rows) + "\n}\n\n")
    # 3) the target, deep in the file
    parts.append(textwrap.dedent('''
        TIER_THRESHOLDS = {"silver": 100, "gold": 1000, "platinum": 10000}


        def tier_for_volume(units: int) -> str:
            """Pricing tier for a monthly unit volume.

            0-99 units -> "bronze"; 100-999 -> "silver"; 1000-9999 -> "gold"; 10000 and
            above -> "platinum". The lower bound of each tier is inclusive.
            """
            if units < 0:
                raise ValueError("units must be non-negative")
            if units > TIER_THRESHOLDS["platinum"]:
                return "platinum"
            if units > TIER_THRESHOLDS["gold"]:
                return "gold"
            if units > TIER_THRESHOLDS["silver"]:
                return "silver"
            return "bronze"


        def discount_for_tier(tier: str) -> float:
            return {"bronze": 0.0, "silver": 0.05, "gold": 0.12, "platinum": 0.2}[tier]


        def monthly_cost(region_key: str, units: int) -> float:
            table = None
            for name, value in globals().items():
                if name.startswith("RATES_") and region_key in value:
                    table = value[region_key]
                    break
            if table is None:
                raise KeyError(region_key)
            gross = table["base"] + table["per_unit"] * units
            return round(gross * (1 - discount_for_tier(tier_for_volume(units))), 2)

    '''))
    parts += helpers[36:]
    src = "".join(parts)
    d = os.path.join(FIX, "cp03_billing")
    write(os.path.join(d, "pricing.py"), src)
    tests = '''#!/usr/bin/env python3
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
'''
    write(os.path.join(d, "test_pricing.py"), tests)
    write_checker(d, "check_collateral.py", ["pricing.py"], allowed=["tier_for_volume"])
    line_of_target = next(i for i, l in enumerate(src.splitlines(), 1) if l.startswith("def tier_for_volume"))
    prompt = COMMON.format(nsteps=5) + textwrap.dedent("""

        The directory cp03_billing holds pricing.py, a generated 1,500-line module of rate
        tables and helpers, and test_pricing.py. `python test_pricing.py` (inside cp03_billing)
        fails on tier boundaries. The module is too long to read whole: search it for the
        function the tests exercise and read only that region.

        Required steps (plan at least 5):
        1. Create your plan (todo tool if available, otherwise plan.md).
        2. Run the tests and identify which function and which boundaries fail.
        3. Fix that one function in pricing.py. Change nothing else in the file, do not
           reformat it, do not touch the tests (python check_collateral.py must print
           COLLATERAL OK).
        4. Re-run the tests until the last line is `13 passed, 0 failed`.
        5. Write fix_note.md (in the task directory) with the function name, the line you
           changed and why, then finish.""")
    return task_json(
        id="CP-03", domain="deep-edit-under-context-pressure", tier=5,
        fixtures=["fixtures/cp03_billing"], prompt=prompt,
        plan_steps=["Create todo plan", "Run tests, read failures", "Find tier_for_volume",
                    "Fix the boundary comparisons only", "Re-run tests, write fix_note.md, finish"],
        milestones=[
            {"artifact": "fix_note.md", "check": "exists"},
            {"artifact": "fix_note.md", "check": "contains_any", "params": {"any_of": ["tier_for_volume"]}},
        ],
        final_artifacts=[
            {"artifact": "cp03_billing/test_pricing.py", "check": "script_pass",
             "params": {"command": "cd cp03_billing && python test_pricing.py", "expect_stdout": "13 passed, 0 failed", "timeout_s": 60}},
            {"artifact": "cp03_billing/check_collateral.py", "check": "script_pass",
             "params": {"command": "cd cp03_billing && python check_collateral.py", "expect_stdout": "COLLATERAL OK", "timeout_s": 60}},
        ],
        pressure={"module_lines": src.count("\n"), "target_line": line_of_target},
    )


# ------------------------------------------------------------------ CP-04 many files
TEAMS = ["platform", "mobile", "data", "security", "design", "infra", "qa", "product"]
DECISIONS = [
    ("platform", "adopt UDX as the default transport for mobile builds", "UDX as the default transport"),
    ("platform", "freeze the v2 ingestion API schema until the March release", "freeze the v2 ingestion API"),
    ("mobile", "ship the Android build with a foreground service for transfers", "foreground service for transfers"),
    ("mobile", "drop iOS 15 support from the next minor", "drop iOS 15"),
    ("data", "store blocks as flat files named by CID, no database index", "named by CID"),
    ("data", "cap the local block cache at 2 GiB with LRU eviction", "2 GiB"),
    ("security", "keep the Ed25519 identity key in the platform keystore only", "platform keystore only"),
    ("security", "rotate relay credentials every 30 days", "every 30 days"),
    ("design", "use a single peer list screen for discovery and sharing", "single peer list screen"),
    ("design", "move the share button into the file row", "share button into the file row"),
    ("infra", "pin the dart_libp2p dependency to 4.12", "pin the dart_libp2p"),
    ("infra", "run the device matrix nightly, not on every push", "nightly, not on every push"),
    ("qa", "require two physical devices for every discovery test", "two physical devices"),
    ("qa", "treat a Mac-only mDNS failure as an environment limit, not a bug", "environment limit"),
    ("product", "name the first public build 0.1 and label it preview", "label it preview"),
    ("product", "defer wide-area discovery to a later release", "defer wide-area discovery"),
]
CHATTER = [
    "{a} walked through the {topic} numbers from last week; nothing surprising.",
    "{a} asked whether {topic} is still on the critical path. {b} thinks it is.",
    "Side discussion on {topic}: {b} will write it up separately.",
    "{a}: the {topic} spike is done, results in the shared doc.",
    "Open question from {b}: do we need {topic} before the demo? Parked.",
    "{a} flagged flaky behaviour in {topic} on the office Wi-Fi; {b} could not reproduce.",
    "Reviewed the {topic} checklist line by line. Two items reworded.",
    "{b} noted the {topic} estimate slipped by two days; no change to the milestone.",
    "Reminder from {a}: update the {topic} runbook when the change lands.",
    "{b} demoed {topic} on a test device. Looked fine; one visual glitch noted.",
]
TOPICS = ["mDNS discovery", "the block cache", "relay setup", "the onboarding flow", "crash reporting",
          "battery usage", "the CI matrix", "localisation", "the permission prompts", "logging"]
PEOPLE = ["Amina", "Bram", "Chen", "Dalia", "Eli", "Femi", "Greta", "Hugo", "Ines", "Jonas"]


def gen_cp04(rng):
    d = os.path.join(FIX, "cp04_notes")
    date_groups, decision_groups = [], []
    for i, team in enumerate(TEAMS):
        date = f"2026-02-{3 + i * 3:02d}"
        fname = f"{date}_{team}_sync.md"
        decs = [dec for t, dec, _k in DECISIONS if t == team]
        keys = [k for t, _d, k in DECISIONS if t == team]
        lines = [f"# {team.title()} sync, {date}", "", f"Attendees: {', '.join(rng.sample(PEOPLE, 5))}", "",
                 "## Agenda", ""]
        agenda = rng.sample(TOPICS, 5)
        lines += [f"- {t}" for t in agenda] + ["", "## Notes", ""]
        body = []
        for _ in range(190):
            a, b = rng.sample(PEOPLE, 2)
            body.append("- " + rng.choice(CHATTER).format(a=a, b=b, topic=rng.choice(TOPICS)))
        # the two decisions and three actions, at random positions in the body
        slots = sorted(rng.sample(range(10, 180), 5))
        inserts = [f"- DECISION: {decs[0]}.", f"- DECISION: {decs[1]}."] + \
                  [f"- ACTION: {rng.choice(PEOPLE)} to {rng.choice(['write up', 'prototype', 'measure', 'review'])} {rng.choice(TOPICS)} by Friday." for _ in range(3)]
        rng.shuffle(inserts)
        for s, ins in zip(slots, inserts):
            body[s] = ins
        lines += body + ["", "## Next meeting", "", f"Same slot next week; {rng.choice(PEOPLE)} to send the agenda."]
        write(os.path.join(d, fname), "\n".join(lines) + "\n")
        date_groups.append([date, date.replace("2026-02-", "Feb ").replace("Feb 0", "Feb ")])
        for k in keys:  # exact substring of the DECISION line, plus case variants (scorer is case-sensitive)
            variants = {k, k.lower(), k[0].upper() + k[1:]}
            if "dart_libp2p" in k:
                variants.add(k.replace("dart_libp2p", "dart-libp2p"))
            decision_groups.append(sorted(variants))
    prompt = COMMON.format(nsteps=5) + textwrap.dedent("""

        The directory cp04_notes holds eight team meeting notes (one Markdown file each, about
        220 lines). Each contains exactly two lines that start with `DECISION:` among the
        discussion bullets. Do not read the files whole one after another and hope to remember
        them: search for the DECISION lines (grep or the search tool), file by file.

        Required steps (plan at least 5):
        1. Create your plan (todo tool if available, otherwise plan.md).
        2. For each of the eight files, find the two DECISION lines and the meeting date (in the
           file name and the title).
        3. Write decisions.md: one section per meeting (team and date), listing its two
           decisions verbatim; sixteen decisions in total.
        4. Verify decisions.md has sixteen decisions and all eight dates.
        5. Finish.""")
    return task_json(
        id="CP-04", domain="many-file-digest-under-context-pressure", tier=5,
        fixtures=["fixtures/cp04_notes"], prompt=prompt,
        plan_steps=["Create todo plan", "Grep DECISION lines per file", "Collect dates",
                    "Write decisions.md", "Verify sixteen decisions, finish"],
        milestones=[
            {"artifact": "decisions.md", "check": "exists"},
            {"artifact": "decisions.md", "check": "contains_groups", "params": {"groups": date_groups, "min_ratio": 0.75}},
        ],
        final_artifacts=[
            {"artifact": "decisions.md", "check": "contains_groups",
             "params": {"groups": decision_groups, "min_ratio": 0.875}},
        ],
        pressure={"files": len(TEAMS), "decisions": len(DECISIONS)},
    )


def generate(root=HERE):
    """Write fixtures/ and tasks/tier5_context/ under `root`; returns the task dicts."""
    global FIX, TASKS
    FIX = os.path.join(root, "fixtures")
    TASKS = os.path.join(root, "tasks", "tier5_context")
    rng = random.Random(SEED)
    tasks = [gen_cp01(rng), gen_cp02(rng), gen_cp03(rng), gen_cp04(rng)]
    os.makedirs(TASKS, exist_ok=True)
    for t in tasks:
        write(os.path.join(TASKS, f"{t['id']}.json"), json.dumps(t, ensure_ascii=False, indent=2) + "\n")
    return tasks


def main():
    for t in generate():
        print(t["id"], t["domain"], t.get("pressure"))


if __name__ == "__main__":
    main()
