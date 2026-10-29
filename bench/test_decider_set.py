"""The labelled-set builder ingests shadow decider sidecars (bead boubou-acy.3)."""
import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def load_builder():
    path = os.path.join(HERE, "decider", "build_intent_set.py")
    spec = importlib.util.spec_from_file_location("build_intent_set", path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["build_intent_set"] = mod
    spec.loader.exec_module(mod)
    return mod


def write_sidecar(d, session, records):
    with open(os.path.join(d, f"{session}.decisions.jsonl"), "w", encoding="utf-8") as f:
        for r in records:
            f.write(json.dumps(r) + "\n")


def rec(text, h="task", m="question", has_progress=False, model="d1:latest"):
    return {"ts": "2026-10-29T10:00:00+00:00", "session": "x", "mode": "shadow", "text": text,
            "has_progress": has_progress,
            "heuristic": {"intent": h, "rule": "default: long, no cue", "confidence": 0.5},
            "model": {"name": model, "intent": m, "probability": 0.91, "confidence": 0.9},
            "latency_ms": 300, "acted_on": h, "acted_on_model": False}


def test_shadow_sidecars_become_candidates_and_labelled_ones_join_the_set(tmp_path):
    b = load_builder()
    sessions = tmp_path / "sessions"
    sessions.mkdir()
    write_sidecar(sessions, "acp_1029_094732", [
        rec("assess the current state of our project"),
        rec("assess the current state of our project", m="task"),  # same text, repeat
        rec("", h="question"),  # no text: ignored
    ])
    write_sidecar(sessions, "chat_1029_120000", [rec("thanks, that's all", h="follow_up", m="question", has_progress=True)])
    (sessions / "broken.decisions.jsonl").write_text("not json\n", encoding="utf-8")
    out = tmp_path / "out"
    out.mkdir()

    key = b.shadow_key("Assess  the current state of our project")
    rows, shadow, awaiting = b.build(str(sessions), str(out), labels={key: ("question", False)})

    assert len(shadow) == 2, [c["text"] for c in shadow]
    first = next(c for c in shadow if c["key"] == key)
    assert first["source"] == "shadow:acp_1029_094732"
    assert first["n"] == 2 and len(first["verdicts"]) == 2, "both model answers kept, once each"
    assert first["verdicts"][0]["model"] == "d1:latest"
    assert awaiting == 1
    cand = json.load(open(out / "shadow_candidates.json", encoding="utf-8"))
    assert {c["key"] for c in cand} == {c["key"] for c in shadow}

    lines = [json.loads(l) for l in open(out / "intent_set.jsonl", encoding="utf-8")]
    labelled = [r for r in lines if r["source"].startswith("shadow:")]
    assert labelled == [{"id": f"s{key}", "text": "assess the current state of our project", "has_progress": False,
                         "label": "question", "uncertain": False, "source": "shadow:acp_1029_094732"}]
    assert len(lines) == len(rows) and len(rows) > 300, "the collected and authored cases are untouched"


def test_without_a_shadow_dir_the_set_is_as_before(tmp_path):
    b = load_builder()
    rows, shadow, awaiting = b.build("", str(tmp_path))
    assert shadow == [] and awaiting == 0
    assert not (tmp_path / "shadow_candidates.json").exists()
    assert all(not r["source"].startswith("shadow:") for r in rows)
