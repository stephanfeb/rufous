# Host tool mentions: a named automation gets run (boubou-8c4)

Follow-up to [host-tools.md](host-tools.md). Draft, 2026-11-05.

## What happens

In a fresh session, qwen3.5:4b answers *What did Say Hello return for Ada?* without calling
`choreo.say_hello`. It failed at every host-tool count in the 2026-11-05 sweep (0–1 of 3 at N=5,
10, 15 and 25). gemma4:e2b passed 11 of 12. The replies are of two kinds. One is a guess from the
description: *"Say Hello typically returns a greeting like 'Hello Ada!'"* The other is an honest
refusal: *"I don't have access to previous results… would you like me to run it?"*

It is not the cap, and it is not the tool list. The turn was sent all five host tools
(`acp_1105_123214.request.json`), and the system prompt ended with the HOST TOOLS line naming
`choreo.say_hello`.

It is not the route either. The decider labelled the message `greeting \`hello\``: the message is
34 characters long, under `GREETING_MAX_CHARS`, and has no task or question hint (`guards/qa.rs`).
A greeting is `Intent::Question`, the same route as Choreo's original prompt (`question hint
\`tell me\``), which passes 3/3. Under `--policy general` both get the task prompt and the
question tool set plus the host tools.

What differs is the text the model reads:

1. **The prompt's first rule points the other way.** `TASK_BASE` opens with *JUDGE INTENT FIRST*:
   small talk, greetings and general knowledge questions, *e.g. "hello"*, get *"ANSWER IN PLAIN
   TEXT. Never call a tool"*. The user's message contains "Hello" and is phrased as a question
   about what something does. The HOST TOOLS line that says otherwise comes last, after the
   platform block.
2. **The name in the message is not the name in the tool list.** The user says "Say Hello"; the
   list says `choreo.say_hello`. Linking the two is left to the model.
3. **Past tense in a fresh session.** "What did … return" reads as a question about history. With
   no history, "I have no record" is a literal, defensible answer.

This is the pattern in [small-models-follow-text-not-state.md](small-models-follow-text-not-state.md):
the state that would settle it (a host tool by that name is right there) is not in the text the
model weighs most. The fix that write-up recommends is to turn the state back into text at the
moment it matters, deterministically, once.

## Design

**Mention detection (shared).** `HostFace` gets a display name per tool: the MCP `title` when the
server sends one, else the tool name with `_` read as a space (`say_hello` → "say hello"). A
message *mentions* a host tool when that display name, or the qualified name, occurs in it on word
boundaries, case-insensitively. Display names of one word never count as mentions on their own:
"backup", "hello" or "report" are everyday words. Pure lexical matching on the latest real user
instruction (`qa::task_routing_text`), logged as `[host] mention: choreo.say_hello ("say hello")`.

**The HOST TOOLS line names display names.** `choreo.say_hello (Say Hello)`, so the second gap is
closed in text on every turn, at no cost when nothing is mentioned. When the server gives no
`title`, the line shows the derived display name.

**Arm A: mention note before the call.** When the instruction mentions a host tool, a one-line
harness note goes in after the user message: *`[harness] "Say Hello" is the app's automation
choreo.say_hello. If the user wants it run or wants its result, call it; it runs now.`* It fires once
per turn and only on a mention.

**Arm B: mention hold at the end.** In the slot before `text reply ends the turn`
(`run.rs`, beside the claims hold), on any route: the instruction mentions a host tool, this turn
called no host tool, and this is the first such hold in the turn. The reply is then held with
*`[harness] You answered without running choreo.say_hello, which the user named. If they want it
run or its result, call it now; if not, reply again and say why.`* An honest second text reply
always passes. This follows the write-up's rule: one specific hold, then let it go.

A is cheaper on a miss (no wasted reply) but fires on every mention, including "don't run Weekly
Report". B costs one extra round, only on a miss, and lets a refusal for a reason through. Both sit
behind ablation switches (`host_mention_note`, `host_mention_hold`) so the bench can compare them.
The one that passes the check below with fewer false fires is kept, and the other is removed.

Not changed: the decider. Labelling the message a task would not change the prompt or the tools
under `general`, and a host title adding to `TASK_HINTS` would route "hello" questions as tasks.

## Bench

Extend `bench/host_tools.py` with prompts that mention a tool (N=5 and 15, 3 runs, both models).
Run three arms: none, A and B.

| Prompt | Wanted |
|---|---|
| What did Say Hello return for Ada? | `say_hello` runs, the reply quotes it |
| Has Weekly Report run today? | `weekly_report` runs, or the reply says it cannot tell without running it |
| What does Tidy Downloads do? | no call is required (described from the tool); a call is not a failure |
| Don't run Weekly Report, just tell me what it would make. | no `weekly_report` call |
| Hello! | no host call (one-word display name, no mention) |
| the existing 7 prompts | unchanged |

Kept when: qwen passes the first prompt at least 8 of 9 in both N cells; the negation and
"Hello!" prompts make no host call; and the existing prompts hold their 2026-11-05 numbers.

## Reading, 2026-11-05: arm A kept, arm B removed

`bench/results/host_tools/2026-11-05_arm_{none,note,hold}`, N=5 and 15, 3 runs each. The table
counts the runs where the named automation was called, out of 6. For hello_q, the brackets count
replies that also quoted the run id.

| Prompt | Model | none | note (A) | hold (B) |
|---|---|---|---|---|
| What did Say Hello return for Ada? | qwen3.5:4b | 1 (1) | 6 (3) | 6 (5) |
| What did Say Hello return for Ada? | gemma4:e2b | 6 (6) | 6 (6) | 6 (6) |
| Don't run Weekly Report… | qwen / gemma | 0 / 0 | 0 / 0 | 5 / 1 |
| What does Tidy Downloads do? | qwen / gemma | 0 / 0 | 0 / 3 | 6 / 6 |
| Has Weekly Report run today? | qwen / gemma | 1 / 4 | 4 / 6 | 6 / 6 |
| Hello! and the four plain prompts | both | no host call | no host call | no host call |

- **B is removed.** Held once, the model answers with the call whatever the user said. It ran Weekly
  Report after "Don't run Weekly Report" and Tidy Downloads, which moves files, on a question about
  what it does. Every Choreo call is a real run with real effects.
- **A is kept.** qwen makes the call 6/6, against 1/6 before, and A causes no false calls. It misses the
  8-of-9 bar on the strict check only by quoting. Three of the six replies gave "Hello, Ada!" without
  the `(run …)` the result carried. The call happens, so that miss is a quoting bead of its own, not
  this one.

## Open questions

- **Choreo:** will each automation's tool carry an MCP `title` set to the name the user sees
  ("Say Hello")? Without one, the derived name only works when the tool name follows the title.
- **"What does X do?"** Should it run X? The table treats a description as a pass. Choreo may
  want it never to run, since every call is a real run with real effects.
