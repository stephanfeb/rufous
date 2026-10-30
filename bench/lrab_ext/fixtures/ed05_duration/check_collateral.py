#!/usr/bin/env python3
"""Collateral-damage check for this task: every top-level function and assignment in the
listed modules must be byte-identical (after ast.unparse) to the shipped version, except the
ones the task asks to fix. Prints COLLATERAL OK, or the names that changed. When REQUIRE is
set, OK also needs the task's own work to have happened (a listed definition differs from the
shipped one, a listed file exists, a listed command prints what it should), so an untouched
tree is COLLATERAL PENDING rather than OK: no credit for doing nothing."""
import ast, hashlib, json, os, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
EXPECT = json.loads("{\"duration.py\": {\"_PATTERN\": \"d7c6bf539655948697760e6e9c9f4d05a4a97a0838cd2016e6b223638dd7bfa2\", \"parse_duration\": \"ed4faa281018e39da4c904551f10e1da9014714259f6daa0bf1a14bbfb0d092b\", \"format_duration\": \"31ffae6a85d6c39ca6111bc31e55fa1a17803d57c76327da5d85b7823a142339\"}, \"test_duration.py\": {\"test_hours\": \"9139a778103e0389c49e4ff1c1344a0049b5845631c9cd0b1b9e0e2e41279473\", \"test_minutes\": \"8baa8a02335520d132ea6b7e6a52477d707b84cddc3ddcaf907c4068c95ae269\", \"test_both\": \"3c77198260ee3a2c5f07062c57d6ccaf804d490bc321d6bb7485dca12bff1c80\", \"test_not_a_duration\": \"163049a32057db0f598ef25965c82d3291138bdb52f2c6198779861ee89776ed\", \"test_round_trip\": \"98aa603ad9e57dcd98be0dcecf047e385b5b6bd73d4c9236decd667037e944dd\", \"_run\": \"0202a11b480440f8791c6e7f324a51edfab223919d0c784cdfca9d4f71b04e04\"}}")
ALLOWED = set([])
REQUIRE = json.loads("{\"exists\": [\"reply.md\"]}")

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
