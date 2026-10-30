#!/usr/bin/env python3
"""Collateral-damage check for this task: every top-level function and assignment in the
listed modules must be byte-identical (after ast.unparse) to the shipped version, except the
ones the task asks to fix. Prints COLLATERAL OK, or the names that changed. When REQUIRE is
set, OK also needs the task's own work to have happened (a listed definition differs from the
shipped one, a listed file exists, a listed command prints what it should), so an untouched
tree is COLLATERAL PENDING rather than OK: no credit for doing nothing."""
import ast, hashlib, json, os, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
EXPECT = json.loads("{\"textfmt.py\": {\"_WS\": \"dff5dda80e4d959d01eef2d66c7e6aa9b3db0ca0b0f28619f853aafa5634b318\", \"collapse_spaces\": \"1858db3aec63102e11877cb00456a06affe37596803bf4e029a7bf11646dad5f\", \"truncate\": \"b968fb7ad652f71ba5c794859a7c2a8c9def344f5bba77f57effcdadcdd770c2\", \"pad\": \"9f3c3edbde8b4ebaafd8c68412154c017f2bb28b4dbe75faab0a972537095486\"}}")
ALLOWED = set([])
REQUIRE = json.loads("{\"changed\": {\"textfmt.py\": [\"title_case\"]}, \"run\": [[[\"test_textfmt.py\"], \", 0 failed\"]]}")

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
