#!/usr/bin/env python3
"""Collateral-damage check for this task: every top-level function and assignment in the
listed modules must be byte-identical (after ast.unparse) to the shipped version, except the
ones the task asks to fix. Prints COLLATERAL OK, or the names that changed. When REQUIRE is
set, OK also needs the task's own work to have happened (a listed definition differs from the
shipped one, a listed file exists, a listed command prints what it should), so an untouched
tree is COLLATERAL PENDING rather than OK: no credit for doing nothing."""
import ast, hashlib, json, os, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
EXPECT = json.loads("{\"rates.py\": {\"BANDS\": \"4b5c4d737f92ce3e3c66b696061f9684ef6a396413b15cb250f998fc777ba05c\", \"SURCHARGE\": \"aa2022a3c07b1f17273c8a71a061715dd2a2484df273ec3a3ff60cc25ac58475\", \"band_for\": \"c82d6a47b71935570474d2c0384fe611fbb668c5a17ef2597b4c99b23265e962\", \"shipping_cost\": \"3c2e36a663380b9d8c1a44712551104ab66fbe3318ca85be8c74598651a59004\"}}")
ALLOWED = set(["band_for"])
REQUIRE = json.loads("{\"changed\": {\"rates.py\": [\"band_for\"]}}")

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
