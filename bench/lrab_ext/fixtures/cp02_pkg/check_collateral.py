#!/usr/bin/env python3
"""Collateral-damage check for this task: every top-level function and assignment in the
listed modules must be byte-identical (after ast.unparse) to the shipped version, except the
ones the task asks to fix. Prints COLLATERAL OK, or the names that changed."""
import ast, hashlib, json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
EXPECT = json.loads("{\"textkit/normalize.py\": {\"_WS\": \"dc323003d3da0fa3c2d4b21af3deaec23515c2a8c1c2ab5f95f1b4b8c3291de6\", \"collapse_spaces\": \"e1f69e8d58f224493eb443581feef6f6f351f0c24fd20b454f687aed1b62b990\", \"to_snake\": \"5f1f24da6dcd398c99e0445756e165c3dc986be6d762ecd145ea1a162d1536ff\", \"strip_accents\": \"0733c537d1f6845dd437d2473ea7f20fd65a4fc7d2416839b3338a8c90b0ce89\", \"normalise_newlines\": \"c5da5652912952e45ef15f6b0f194102c1836e6b78e03a97b2f50399a674f25a\", \"truncate\": \"e1c5ebaeef3f6496cfdcae30b3e9948e2520ce8a38ef4d5dea26b91e7874ffc9\"}, \"textkit/tokens.py\": {\"_WORD\": \"446b1e1fdf1e89a651c09fddca276497050b0b221343b0702d4bd072287bcc44\", \"words\": \"274f513677e6e5f94757e0d091cb0fd96383bfd313e9e32a1f5451ac05f1904a\", \"ngrams\": \"6ac5bd1f6e54e1891000e821c6af3f56e30bef8c6815a13d7cf60a706db58d27\", \"unique_in_order\": \"91e429e85e9ab4b9cd2274f5851446e9f5c67c703700a2d6cb8afe7a60b08849\", \"shingles\": \"f77272909ebb3c2b503dbad01efacb923826d2766ef80d88c939a0bb810f224a\", \"split_camel\": \"0212205fee1565b1e9c94b4145e1de8436f5f3c3b32eb2a7603d3b7b10389075\"}, \"textkit/stats.py\": {\"frequencies\": \"a5aa9830d8ecb5a014c67448b0596cb845eabac66d4aec06877b118cf4a5ec56\", \"top_k\": \"d0019b8466bd68f8a260172bb1477a5680f60c403c64cbe3ddcffe7795d69a57\", \"median\": \"74e2481c0bd0e1e1b01c562c23e6b867115212f5e3ccb663cd7677fa5c92a9b4\", \"mean\": \"f5e659e903a9f7683096a417f827176abbb50548f87d05db92ce67f0718a1f4a\", \"type_token_ratio\": \"32ec8e5fb8ddde566f6b61ef74bbefb0364dab545bb5aa9ab9e4a29f75cd24b3\"}, \"textkit/format.py\": {\"pad_right\": \"d0c6a798fef67caebb714d66b55dd03cc69a78a5c6a4cc6a3ed07fcdea2034db\", \"pad_left\": \"624597f54430d9fea32002ec9460c7e728f2aa039a17ebb5c55ec2de88fa2d88\", \"table\": \"f25e3e466ad186462e02eeb983f86a6dcf58d3ddd8c348bfab7c0e3ce11dfb7b\", \"percent\": \"fb549d7f09c03575512077aaf608d5f153dcc4cac3be1508422ae31f39a333d8\", \"pluralise\": \"c1e4fb9fe2474a342d85543f66c7ce098add19f1dbf3e9cbe2df21cc41eef88e\"}}")
ALLOWED = set(["_WS", "collapse_spaces", "median", "ngrams"])

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
