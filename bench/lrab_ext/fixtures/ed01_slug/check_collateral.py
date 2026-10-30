#!/usr/bin/env python3
"""Collateral-damage check for this task: every top-level function and assignment in the
listed modules must be byte-identical (after ast.unparse) to the shipped version, except the
ones the task asks to fix. Prints COLLATERAL OK, or the names that changed."""
import ast, hashlib, json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
EXPECT = json.loads("{\"slug.py\": {\"_NON_WORD\": \"5f0cc39b64b8260d96e81ef6e201228b0d15a07d474c12166f81762ab85ecde7\", \"strip_accents\": \"006ecbee2e21ead944e01a641ad68c5c97d5fde9305a00c3eee7914f4020670a\", \"slugify\": \"7df955ade339de295b88e0fc9e105b1bb3fee65d3f12dc850fa8db8e962c4f9d\", \"slug_path\": \"a48e24f7947af6a86570f3a50b3f23d7d7d405297aefe276a196e01300ca4072\"}}")
ALLOWED = set(["_NON_WORD", "slugify"])

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
print("COLLATERAL OK")
