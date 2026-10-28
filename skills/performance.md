---
name: performance
description: Measure first to find hotspots, then optimise, then compare
---
# Performance optimisation
1. Measure first: time/cProfile to find hotspots; record the baseline numbers
2. Optimise only the hotspots, never by guesswork
3. Common moves: avoid O(n^2) (use set/dict), list comprehensions, cut repeated computation, caching
4. Measure again after optimising; record the speed-up factor
5. Keep behaviour correct; all tests green
