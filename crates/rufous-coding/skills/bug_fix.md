---
name: bug_fix
description: Systematic debugging: reproduce -> locate -> minimal fix -> verify
---
# Bug-fixing process
1. Reproduce: produce a minimal reproducing input and confirm the error really occurs
2. Locate: use logging / bisection / isolation to find the exact failing line and the root cause
3. Fix: make the smallest change; do not refactor unrelated code while you are there
4. Verify: rerun the original reproducing input plus the related tests; confirm the fix and that nothing else broke
5. If the fix does not work, go back to step 2 and try a different idea; do not repeat the same kind of fix
