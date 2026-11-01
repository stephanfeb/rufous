#!/usr/bin/env python3
"""Offline comparison of the intent heuristic against a decision model (docs/system-one-decider.md
section 5, bead boubou-acy.2).

Reads a labelled set (JSONL: text, has_progress, label in question|task|follow_up, source),
runs `rufous decide` once over the originals and once over light rephrasings, and reports:

- accuracy of the heuristic, of the model, and of simulated `on` mode (heuristic where its
  confidence >= threshold, model below), overall and on the heuristic-default subset (the only
  cases `on` mode ever acts on);
- the loop-relevant binary accuracy (question vs not): follow_up and task both run the task loop;
- confusion matrices;
- reversal rate under rephrasing: fraction of cases whose verdict changes on any variant;
- model latency p50 / p95.

    python3 bench/decider_eval.py --set bench/decider/intent_set.jsonl --model d1:latest
    python3 bench/decider_eval.py --set … --model d1:latest --out bench/results/decider/run1

Acceptance (design note): +10 points accuracy over the heuristic on the labelled set, reversal rate
no worse, p95 latency under 1.5 s on the laptop; otherwise the decider stays off.
"""
import argparse, json, os, statistics, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
RUFOUS = os.path.join(ROOT, "target", "release", "rufous")
LABELS = ("question", "task", "follow_up")


def rephrasings(text):
    """Light, label-preserving variants (the robustness axis). Deterministic, no model."""
    t = text.strip()
    out = []
    low = t[0].lower() + t[1:] if t else t
    out.append(("please_prefix", "Please " + low if not low.lower().startswith("please") else t))
    out.append(("lowercase", t.lower()))
    stripped = t.rstrip(".!")
    if stripped != t:
        out.append(("no_final_stop", stripped))
    else:
        out.append(("final_stop", t + "."))
    if "can you" in t.lower():
        out.append(("could_you", t.replace("Can you", "Could you").replace("can you", "could you")))
    elif "could you" in t.lower():
        out.append(("can_you", t.replace("Could you", "Can you").replace("could you", "can you")))
    else:
        out.append(("hey_prefix", "hey, " + low))
    out.append(("thanks_suffix", t + " thanks"))
    # dedupe, drop identical-to-original
    seen = {t}
    uniq = []
    for k, v in out:
        if v not in seen:
            seen.add(v)
            uniq.append((k, v))
    return uniq


def run_decide(cases, model, timeout_ms):
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
        for c in cases:
            f.write(json.dumps({"text": c["text"], "has_progress": bool(c.get("has_progress"))}) + "\n")
        path = f.name
    cmd = [RUFOUS, "decide", "--file", path, "--timeout-ms", str(timeout_ms)]
    if model:
        cmd += ["--model", model]
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, check=True)
    finally:
        os.unlink(path)
    rows = [json.loads(l) for l in p.stdout.splitlines() if l.strip()]
    if len(rows) != len(cases):
        sys.exit(f"rufous decide returned {len(rows)} rows for {len(cases)} cases: {p.stderr[-500:]}")
    return rows


def verdicts(row, threshold):
    h = row["heuristic"]["intent"]
    hc = row["heuristic"]["confidence"]
    m = (row.get("model") or {}).get("intent")
    on = m if (m and hc < threshold) else h
    return h, m, on


def policy_verdict(name, row, threshold):
    """Alternative layering policies, scored offline from the same rows. `on` is the shipped
    one (model acts where the heuristic confidence is below `threshold`)."""
    h = row["heuristic"]["intent"]
    hc = row["heuristic"]["confidence"]
    rule = row["heuristic"]["rule"]
    mo = row.get("model") or {}
    m, mp, mc = mo.get("intent"), mo.get("p", 0.0), mo.get("confidence", 0.0)
    if not m:
        return h
    if name == "heuristic":
        return h
    if name == "model":
        return m
    if name == "on":
        return m if hc < threshold else h
    if name.startswith("model_p>="):          # model when its own probability clears a bar
        return m if mp >= float(name.split(">=")[1]) else h
    if name == "verb_override":               # model may only turn a task-hint hit into a question
        return "question" if (rule.startswith("task hint") and m == "question" and mp >= 0.5) else h
    if name == "binary_model":                # model decides question-vs-not, heuristic keeps the rest
        if m == "question":
            return "question"
        return h if h != "question" else "task"
    raise ValueError(name)


POLICIES = ["heuristic", "model", "on", "model_p>=0.6", "model_p>=0.7", "model_p>=0.8", "verb_override", "binary_model"]


def acc(pairs):
    pairs = list(pairs)
    return (sum(1 for a, b in pairs if a == b) / len(pairs)) if pairs else float("nan")


def confusion(pairs):
    cm = {l: {k: 0 for k in LABELS} for l in LABELS}
    for gold, pred in pairs:
        if gold in cm and pred in cm[gold]:
            cm[gold][pred] += 1
    return cm


def fmt_cm(cm):
    lines = ["| gold \\ pred | " + " | ".join(LABELS) + " |", "|---|" + "---|" * len(LABELS)]
    for g in LABELS:
        lines.append(f"| {g} | " + " | ".join(str(cm[g][p]) for p in LABELS) + " |")
    return "\n".join(lines)


def pct(x):
    return "n/a" if x != x else f"{100 * x:.1f}%"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default=os.path.join(HERE, "decider", "intent_set.jsonl"))
    ap.add_argument("--model", default="d1:latest", help="decision model tag; '' = heuristic only")
    ap.add_argument("--threshold", type=float, default=0.9)
    ap.add_argument("--timeout-ms", type=int, default=5000)
    ap.add_argument("--no-rephrase", action="store_true")
    ap.add_argument("--out", default="", help="directory for rows.jsonl and summary.md")
    a = ap.parse_args()
    model = a.model or None

    cases = [json.loads(l) for l in open(a.set, encoding="utf-8") if l.strip()]
    cases = [c for c in cases if c.get("label") in LABELS]
    if not cases:
        sys.exit("no labelled cases")
    rows = run_decide(cases, model, a.timeout_ms)
    for c, r in zip(cases, rows):
        r["label"] = c["label"]
        r["source"] = c.get("source", "")
        r["id"] = c.get("id", "")

    gold = [r["label"] for r in rows]
    vs = [verdicts(r, a.threshold) for r in rows]
    H = [v[0] for v in vs]
    M = [v[1] for v in vs]
    ON = [v[2] for v in vs]
    defaults = [i for i, r in enumerate(rows) if r["heuristic"]["confidence"] < a.threshold]
    sure = [i for i in range(len(rows)) if i not in set(defaults)]

    def binary(x):
        return "question" if x == "question" else "not_question"

    lines = []
    lines.append(f"# decider eval · {len(rows)} cases · model {model or 'none'} · threshold {a.threshold}")
    lines.append("")
    lines.append("| metric | heuristic | model | simulated on |")
    lines.append("|---|---|---|---|")
    lines.append(f"| accuracy, 3 classes | {pct(acc(zip(gold, H)))} | {pct(acc(zip(gold, M)))} | {pct(acc(zip(gold, ON)))} |")
    lines.append(f"| accuracy, question vs not | {pct(acc((binary(g), binary(h)) for g, h in zip(gold, H)))} | {pct(acc((binary(g), binary(m)) for g, m in zip(gold, M)))} | {pct(acc((binary(g), binary(o)) for g, o in zip(gold, ON)))} |")
    lines.append(f"| accuracy on heuristic defaults (n={len(defaults)}) | {pct(acc((gold[i], H[i]) for i in defaults))} | {pct(acc((gold[i], M[i]) for i in defaults))} | {pct(acc((gold[i], ON[i]) for i in defaults))} |")
    lines.append(f"| accuracy on heuristic rule hits (n={len(sure)}) | {pct(acc((gold[i], H[i]) for i in sure))} | {pct(acc((gold[i], M[i]) for i in sure))} | {pct(acc((gold[i], ON[i]) for i in sure))} |")
    if model:
        ms = sorted(r["model"]["ms"] for r in rows if "ms" in (r.get("model") or {}))
        errs = sum(1 for r in rows if "error" in (r.get("model") or {}))
        if ms:
            p50 = statistics.median(ms)
            p95 = ms[min(len(ms) - 1, int(round(0.95 * (len(ms) - 1))))]
            lines.append(f"| model latency p50 / p95 | | {p50:.0f} ms / {p95:.0f} ms | |")
        lines.append(f"| model errors | | {errs} | |")
    lines.append("")
    lines.append("## confusion, heuristic\n\n" + fmt_cm(confusion(zip(gold, H))))
    if model:
        lines.append("\n## confusion, model\n\n" + fmt_cm(confusion(zip(gold, M))))
        lines.append("\n## confusion, simulated on\n\n" + fmt_cm(confusion(zip(gold, ON))))

    # Disagreements where the heuristic was sure: a bad rule or a bad model, either way worth a look.
    if model:
        lines.append("\n## model disagrees with a heuristic rule hit")
        n = 0
        for i in sure:
            if M[i] and M[i] != H[i]:
                n += 1
                r = rows[i]
                who = "model right" if M[i] == gold[i] else ("heuristic right" if H[i] == gold[i] else "both wrong")
                lines.append(f"- [{who}] gold={gold[i]} heur={H[i]} ({r['heuristic']['rule']}) model={M[i]} p={r['model'].get('p', 0):.2f} · {r['text'][:110]!r}")
        if n == 0:
            lines.append("- none")

    # Rephrasing robustness.
    rev = {}
    vrows = []
    if not a.no_rephrase:
        var_cases, owner = [], []
        for i, c in enumerate(cases):
            for k, v in rephrasings(c["text"]):
                var_cases.append({"text": v, "has_progress": c.get("has_progress", False), "variant": k})
                owner.append(i)
        vrows = run_decide(var_cases, model, a.timeout_ms)
        for i, vr, vc in zip(owner, vrows, var_cases):
            vr["owner"] = rows[i].get("id", "")
            vr["variant"] = vc["variant"]
        flips = {"heuristic": set(), "model": set(), "on": set()}
        for i, vr in zip(owner, vrows):
            vh, vm, von = verdicts(vr, a.threshold)
            if vh != H[i]:
                flips["heuristic"].add(i)
            if model and vm and M[i] and vm != M[i]:
                flips["model"].add(i)
            if von != ON[i]:
                flips["on"].add(i)
        rev = {k: len(v) / len(cases) for k, v in flips.items()}
        if model:
            # Policy grid: accuracy (3-class, question-vs-not), split by source, reversal.
            coll = [i for i, r in enumerate(rows) if not r["source"].startswith("authored")]
            auth = [i for i, r in enumerate(rows) if r["source"].startswith("authored")]
            lines.append("\n## layering policies, scored offline from the same rows")
            lines.append("| policy | 3-class | question vs not | q-vs-not collected (n=%d) | q-vs-not authored (n=%d) | reversal |" % (len(coll), len(auth)))
            lines.append("|---|---|---|---|---|---|")
            for pol in POLICIES:
                P = [policy_verdict(pol, r, a.threshold) for r in rows]
                pflips = set()
                for i, vr in zip(owner, vrows):
                    if policy_verdict(pol, vr, a.threshold) != P[i]:
                        pflips.add(i)
                lines.append("| %s | %s | %s | %s | %s | %s |" % (
                    pol,
                    pct(acc(zip(gold, P))),
                    pct(acc((binary(g), binary(p)) for g, p in zip(gold, P))),
                    pct(acc((binary(gold[i]), binary(P[i])) for i in coll)),
                    pct(acc((binary(gold[i]), binary(P[i])) for i in auth)),
                    pct(len(pflips) / len(cases)),
                ))
            lines.append("- `on` is the shipped policy; `model_p>=x` acts on the model when its chosen-option probability clears x; `verb_override` lets the model only turn a task-verb hit into a question; `binary_model` takes question-vs-not from the model and the task/follow-up split from the heuristic.")
        lines.append("\n## reversal rate under light rephrasing (share of cases whose verdict changes on any variant)")
        lines.append(f"- heuristic {pct(rev['heuristic'])} · model {pct(rev['model']) if model else 'n/a'} · simulated on {pct(rev['on'])}")
        lines.append(f"- {len(var_cases)} variants over {len(cases)} cases; variants: " + ", ".join(sorted({v['variant'] for v in var_cases})))

    # Acceptance verdict.
    if model:
        gain = acc(zip(gold, ON)) - acc(zip(gold, H))
        ok_gain = gain >= 0.10
        ok_rev = (not rev) or rev["on"] <= rev["heuristic"] + 1e-9
        ms = sorted(r["model"]["ms"] for r in rows if "ms" in (r.get("model") or {}))
        p95 = ms[min(len(ms) - 1, int(round(0.95 * (len(ms) - 1))))] if ms else float("inf")
        ok_lat = p95 < 1500
        lines.append("\n## acceptance (design note section 5)")
        lines.append(f"- accuracy gain of simulated on over heuristic: {100 * gain:+.1f} points (need +10): {'pass' if ok_gain else 'fail'}")
        lines.append(f"- reversal rate no worse: {'pass' if ok_rev else 'fail'}")
        lines.append(f"- p95 latency under 1.5 s: {p95:.0f} ms: {'pass' if ok_lat else 'fail'}")
        lines.append(f"- verdict: {'ADOPT for intent' if ok_gain and ok_rev and ok_lat else 'keep off (or shadow)'}")

    report = "\n".join(lines)
    print(report)
    if a.out:
        os.makedirs(a.out, exist_ok=True)
        with open(os.path.join(a.out, "rows.jsonl"), "w", encoding="utf-8") as f:
            for r in rows:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
        with open(os.path.join(a.out, "summary.md"), "w", encoding="utf-8") as f:
            f.write(report + "\n")
        if not a.no_rephrase:
            with open(os.path.join(a.out, "variants.jsonl"), "w", encoding="utf-8") as f:
                for vr in vrows:
                    f.write(json.dumps(vr, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
