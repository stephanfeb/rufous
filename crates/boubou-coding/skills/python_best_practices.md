---
name: python_best_practices
description: Python conventions: type hints / pathlib / exceptions / style
---
# Python coding conventions
1. Type hints: def f(x: int) -> str
2. Use pathlib.Path for paths, not string concatenation
3. Catch specific exceptions; avoid bare except
4. snake_case naming that states the intent clearly
5. Use with to manage files / resources
6. When done, verify the syntax with python -m py_compile
