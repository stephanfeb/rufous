# Jev and Open-Weights "System One" Models

*Research notes, 28 October 2026*

## What Jev is

**Jev** is a proprietary decision model from **TypeSafe AI** (typesafe.ai), released in limited early access on 15 September 2026 alongside a $40M seed round led by DCVC. The CEO, Diogo Almeida, previously spent ~4 years at OpenAI working on RLHF, InstructGPT, ChatGPT and GPT-4.

TypeSafe calls Jev the first **"System One" model** — Kahneman's fast, intuitive thinking, as opposed to slow, deliberate "System Two" reasoning. The defining traits:

- **No text generation.** Input is a block of state (string, JSON, or array of text) plus one or more typed questions; output is typed values with probabilities and confidence scores, meant to be consumed by software.
- **Three primitives:** *Choice* (pick one option from a set), *Score* (rate against ordered levels), *Noul* (yes/no probability).
- **Single parallel pass** over all questions, so output cannot fall outside the supplied schema.
- **RLCD training** (Reinforcement Learning for Calibrated Decisions): probabilities optimized against outcomes rather than human preference.
- **Pricing/speed claims:** $0.042 per million input tokens, free output; 70–500 ms end-to-end; 40–200x faster and 40–400x cheaper than frontier LLMs. TypeSafe itself notes the test workflows were built by its own team and the gains likely sit at the high end of real-world results.

Jev is closed: no architecture, weights, or technical paper have been published. Outside observers suspect it is built on an open-weight LLM. Stable release is jev-1.13.0.

## Open-weights alternatives

The community response has been large and fast: multiple "awesome" lists, a `jev-alternative` GitHub topic with 40+ repos, and three arXiv papers within six weeks.

### Backed by established organizations

| Model | Base / size | License | Notes |
|---|---|---|---|
| **Cloudflare Clef** | Fine-tuned Qwen + joint decision head; 27B / 9B / 30B-A3B | Apache 2.0 | Jev-compatible request format; Cloudflare's runs place it #1 on the Jev Decision Index |
| **Liquid AI d1** | d1-3B, d1-omni-600M (plus hosted d1) | LFM Open License | Works with TypeSafe's SDKs |
| **Together AI Tev1** | Qwen3.5 fine-tunes, 4B | Weights license pending; code MIT | Returns one option letter; next-token, not a parallel decision head |
| **H2O-Lightning-4B** | Qwen3.5-4B | Open | Multimodal (text + images), one forward pass per decision |
| **AutoTrust JEV-27B** | Qwen3.8-27B, distilled from Jev 1.13 | Apache 2.0 | Authors report 84.07 vs 83.85 for Jev across six public benchmarks |

### Strongest independent projects

- **JevK5** — Best third-party validation so far. JevBench v1.4 ranks it 2nd of 76 systems and first among open entrants (62.04 vs Jev 1.13's 63.29). Qwen3.5-4B/9B + merged LoRA, Apache-2.0, ~13 ms on an H100, GGUF builds for llama.cpp. A community LoRA on top of it (Plumb-4B) later scored 65.84, above Jev.
- **Laya** (Convai Innovations) — The first replica, out three days after Jev. 421M ModernBERT, Apache 2.0, very fast and tiny, but weak out of the box: at chance on an unseen phishing task without fine-tuning; JevBench v1.3 composite 54.4 vs Jev's 74.4, hard-case accuracy 34.1% vs 74.1%. Large ecosystem (MLX, browser-agent, Windows/ONNX ports).
- **Kev** — Qwen3.5 with learned decision heads; Kev-27B within a point of Jev on unseen sources (0.851 vs 0.857 Brier). Only open-weights route on OpenRouter's Decisions API. CC BY-NC.
- **Open-Jev** (Zefan Cai) — LoRA + scalar decision head on Qwen3.5 2B/9B/27B, Apache 2.0, local `/v1/systemone` server.
- **Small encoder models** — openJev-verdict-2.0 (151M ModernBERT), Vela (0.3B–9B, 17 languages), von (395M), NanoJev (0.6B). Sub-20 ms CPU-class options; generally behind the 4B+ models on hard items.

### Tooling

- **AnyJev** — library that extracts typed choices, yes/no judgments, scores and probabilities from any open causal LLM's logits without retraining.
- **Benchmarks:** JevBench (benchmarkheaven.com) and the Jev Decision Index are the two independent leaderboards. Most other numbers are self-reported.

## What the research says

- *Releasing the Inner Jev* (arXiv, Oct 2026): open instruction-tuned models already perform comparably to Jev without fine-tuning, and short supervised fine-tuning matches or exceeds it — no special architecture or RL needed.
- *System-One Decisions in Medicine* (arXiv, Oct 2026): across 21 models / 15 datasets, Jev's real edge is **robustness** — lowest decision-reversal rate under question rephrasing. Only one open alternative (StartLux) matched it.
- *Benchmarking System One decision models for automated decision gates* (arXiv, Oct 2026): analysis of 2,170 public projects shows Jev used for attribute judgment, action selection, content filtering, and model/tool routing.

## Bottom line

- **Self-hosted drop-in today:** JevK5 (4B, Apache-2.0) or Cloudflare Clef — the two with independent numbers behind them.
- **Sub-500M / CPU:** Laya, but plan to fine-tune on your task.
- **Jev still leads** on calibration and robustness, by a shrinking margin.
- The field is six weeks old and benchmark positions change weekly; check JevBench before committing.

## Sources

- [Jev (AI model) — Wikipedia](https://en.wikipedia.org/wiki/Jev_(AI_model))
- [The Register: TypeSafe AI debuts model for machines](https://www.theregister.com/a/5296711)
- [The Register: Jev's new AI primitives for coders](https://www.theregister.com/a/5298431)
- [awesome-system-one (andyrewlee)](https://github.com/andyrewlee/awesome-system-one)
- [awesome-system-one (raullenchai)](https://github.com/raullenchai/awesome-system-one)
- [awesome-system-one (yanng981)](https://github.com/yanng981/awesome-system-one)
- [awesome-decision-models](https://github.com/AnotiaWang/awesome-decision-models)
- [jev-alternative GitHub topic](https://github.com/topics/jev-alternative)
- [JevK5](https://github.com/allebee/jevk5)
- [JevBench](https://github.com/fstandhartinger/jevbench)
- [Laya vs Jev benchmark](https://huggingface.co/datasets/Luni/laya-jev-benchmark)
- [Jev vs Laya guide](https://huggingface.co/blog/sora-2/jev-vs-laya-hosted-api-or-open-weights-2026-guide)
- [Releasing the Inner Jev (arXiv)](https://arxiv.org/pdf/2610.17205)
- [System-One Decisions in Medicine (arXiv)](https://arxiv.org/pdf/2610.18529)
- [Benchmarking System One decision models (arXiv)](https://arxiv.org/pdf/2610.00346)
