---
name: architecture
description: System design: requirements -> constraints -> module split -> interfaces -> implementation
---
# Architecture design
1. Pin down requirements and constraints (performance / concurrency / maintainability / scale)
2. Split into modules: single responsibility, clear interfaces
3. Draw the data flow: where data comes from -> how it is processed -> where it goes
4. Choose data structures and key algorithms, and give the reasons
5. Write the design document first (design.md), then implement; the implementation follows the design
