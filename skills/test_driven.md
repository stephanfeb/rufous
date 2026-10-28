---
name: test_driven
description: Write a failing test before implementing; the TDD red-green-refactor cycle
---
# TDD cycle
1. First write a test that fails (red), describing the expected behaviour
2. Write the minimal implementation that makes it pass (green)
3. Refactor, keeping all tests green
4. Run with pytest -q; on failure read only the first failing test and fix until it passes
5. Test files are named test_*.py, functions test_*; assertions must be specific
