---
name: refactor
description: Refactoring: behaviour unchanged, structure changed; deliver only when all tests are green
---
# Refactoring principles
1. Behaviour unchanged, structure changed; tests must be all green before and after the refactor
2. Extract duplicated logic into functions; split long functions into small ones
3. One kind of refactoring at a time; run the tests after every step
4. Names are documentation: variable / function names state the intent
5. Do not add features while you are there; that is a new requirement, not a refactor
