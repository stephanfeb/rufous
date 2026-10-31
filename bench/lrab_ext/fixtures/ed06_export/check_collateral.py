#!/usr/bin/env python3
"""Collateral-damage check for this task: every top-level function and assignment in the
listed modules must be byte-identical (after ast.unparse) to the shipped version, except the
ones the task asks to fix. Prints COLLATERAL OK, or the names that changed. When REQUIRE is
set, OK also needs the task's own work to have happened (a listed definition differs from the
shipped one, a listed file exists, a listed command prints what it should), so an untouched
tree is COLLATERAL PENDING rather than OK: no credit for doing nothing."""
import ast, hashlib, json, os, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
EXPECT = json.loads("{\"export.py\": {\"quote\": \"9b67441c1e8719d296baafe4216e9e43ac903f7dd5289e3264226b3e18560038\", \"csv_line\": \"d597216a8a34ba1dc1fc98e31860a88434c0653ebba73a389ea88fa93a8950ce\", \"tsv_line\": \"a58477be42824b2a21ccfc1e64a2caff90d80cb19c49d6923a3cba8c23974ffd\", \"render\": \"dd5a4af715de8447b12eadce23c9555bf75caccbe703c98811aa37cbb5977e60\"}}")
ALLOWED = set(["tsv_line"])
REQUIRE = json.loads("{\"changed\": {\"export.py\": [\"tsv_line\"]}}")

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
