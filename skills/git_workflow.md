---
name: git_workflow
description: git discipline: check status first, small commits, branch and merge
---
# git workflow
1. Before acting, run git status / git diff to understand the current state
2. If not initialised, git init first; if git complains about a missing identity, set git config user.name/user.email
3. Small commits: one logical change per commit, with a short, clear message
4. New features on a branch: git checkout -b feature/xxx, merge once done
5. Merge: git checkout main && git merge feature/xxx
6. Use git log --oneline to confirm the commit history looks as expected
