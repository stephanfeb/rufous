//! System One decider through the public API: config, the Ollama wire shapes, and the loop's
//! intent call site in each mode (bead boubou-acy.1).

use std::collections::BTreeMap;
use std::path::Path;

use boubou_core::agent::chat::{ChatBackend, ChatError, ChatResponse, StreamPiece};
use boubou_core::agent::{Agent, RunParams, ToolRunner};
use boubou_core::config::Config;
use boubou_core::decider::{
    DeciderConfig, DeciderMode, DecisionBackend, LayeredDecider, LocalOutcome,
};
use boubou_core::guards::qa::{intent_heuristic, is_qa, Intent};
use boubou_core::ollama::{Message, Role, SystemOneAnswer, SystemOneQuestion, SystemOneRequest};
use serde_json::{json, Value};

struct Talk;
impl ChatBackend for Talk {
    fn chat(
        &mut self,
        _m: &str,
        _msgs: &[Message],
        _t: &[Value],
        _on: &mut dyn FnMut(StreamPiece<'_>),
    ) -> Result<ChatResponse, ChatError> {
        Ok(ChatResponse::text(
            "This repository is a Rust agent harness; nothing in it handles authentication tokens.",
        ))
    }
}
struct OkTools;
impl ToolRunner for OkTools {
    fn run_tool(&mut self, name: &str, _args: &Value) -> String {
        if name == "finish" {
            "[TASK_COMPLETE] ok".into()
        } else {
            "[ok]".into()
        }
    }
}

/// A decision model that always answers `intent` with probability `p`.
struct Always(Intent, f64);
impl DecisionBackend for Always {
    fn name(&self) -> String {
        "always".into()
    }
    fn decide(
        &mut self,
        _s: &str,
        _q: BTreeMap<String, SystemOneQuestion>,
    ) -> Result<BTreeMap<String, SystemOneAnswer>, String> {
        let mut probs = BTreeMap::new();
        for i in [Intent::Question, Intent::Task, Intent::FollowUp] {
            probs.insert(
                i.as_str().to_string(),
                if i == self.0 {
                    self.1
                } else {
                    (1.0 - self.1) / 2.0
                },
            );
        }
        let mut m = BTreeMap::new();
        m.insert(
            "intent".to_string(),
            SystemOneAnswer::Choice {
                choice: self.0.as_str().to_string(),
                probabilities: probs,
                confidence: self.1,
            },
        );
        Ok(m)
    }
}

// Long, no task or question cue, no trailing `?`: the heuristic defaults to task at 0.5.
const GUESS: &str =
    "I was wondering whether this repository has anything to do with authentication tokens";

fn run_with(mode: DeciderMode, d: &Path) -> Vec<Message> {
    let cfg = DeciderConfig {
        mode,
        ..Default::default()
    };
    let mut a = Agent::new(RunParams::plain("gemma4:12b", d), Talk, OkTools).with_decider(
        LayeredDecider::new(&cfg, Box::new(Always(Intent::Question, 0.93))),
    );
    a.sessions_dir = d.join("sessions");
    a.run(
        vec![Message::system("sys"), Message::user(GUESS)],
        Some("chat_1029_120000"),
        None,
    )
    .expect("loop")
}

/// The decisions sidecar of the session `run_with` names.
fn sidecar(d: &Path) -> Vec<Value> {
    let p = d.join("sessions").join("chat_1029_120000.decisions.jsonl");
    std::fs::read_to_string(p)
        .map(|s| {
            s.lines()
                .map(|l| serde_json::from_str(l).expect("json line"))
                .collect()
        })
        .unwrap_or_default()
}

fn user_msgs<'a>(msgs: &'a [Message], needle: &str) -> Vec<&'a Message> {
    msgs.iter()
        .filter(|m| m.role == Role::User && m.content.contains(needle))
        .collect()
}

#[test]
fn heuristic_confidence_is_one_on_a_rule_and_half_on_a_default() {
    let h = intent_heuristic(&[Message::user("Write a haiku about rain")], false);
    assert_eq!((h.intent, h.confidence), (Intent::Task, 1.0));
    assert_eq!(h.rule, "task hint `write`");
    let h = intent_heuristic(
        &[Message::user("Are you capable of software development ?")],
        false,
    );
    assert_eq!((h.intent, h.confidence), (Intent::Question, 1.0));
    assert_eq!(h.rule, "question hint `are you`");
    let h = intent_heuristic(&[Message::user("Do you know what day it is today")], false);
    assert_eq!(h.rule, "question hint `do you`");
    let h = intent_heuristic(&[Message::user("Fancy a game of chess ?")], false);
    assert_eq!((h.intent, h.confidence), (Intent::Question, 1.0));
    assert_eq!(h.rule, "trailing `?`");
    let h = intent_heuristic(&[Message::user("hello there")], false);
    assert_eq!(h.rule, "question hint `hello`");
    let h = intent_heuristic(&[Message::user("ok go on")], true);
    assert_eq!((h.intent, h.confidence), (Intent::FollowUp, 0.5));
    let h = intent_heuristic(&[Message::user("ok go on")], false);
    assert_eq!((h.intent, h.confidence), (Intent::Question, 0.5));
    let h = intent_heuristic(&[Message::user(GUESS)], false);
    assert_eq!((h.intent, h.confidence), (Intent::Task, 0.5));
    assert!(h.rule.starts_with("default:"));
    // Harness notes are skipped as before; no real message is a task default.
    let h = intent_heuristic(&[Message::user("⚠️ Pre-delivery check: …")], false);
    assert_eq!(h.text, "");
    assert_eq!(h.intent, Intent::Task);
}

#[test]
fn is_qa_is_the_heuristic_intent_being_question() {
    let cases = [
        ("Are you capable of software development ?", false),
        (
            "Is there a config file somewhere in this repository?",
            false,
        ),
        (
            "How many thousand hours did it take? Output just the number.",
            false,
        ),
        (
            "Can you write a script that parses the logs in this directory?",
            false,
        ),
        ("there are other tools for these files", false),
        ("there are other tools for these files", true),
        ("thanks", true),
        ("help me check the code for bugs", false),
        (GUESS, false),
    ];
    for (text, progress) in cases {
        let msgs = [Message::user(text)];
        assert_eq!(
            is_qa(&msgs, progress),
            intent_heuristic(&msgs, progress).intent == Intent::Question,
            "{text:?} progress={progress}"
        );
    }
}

#[test]
fn off_mode_runs_the_task_loop_exactly_as_before() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_with(DeciderMode::Off, d.path());
    assert!(
        sidecar(d.path()).is_empty(),
        "off mode leaves no decisions sidecar"
    );
    // Text-only replies in task mode draw the "continue with tools" note.
    assert!(!user_msgs(&out, "Continue:").is_empty());
    assert!(user_msgs(&out, "Your answer is already sufficient").is_empty());
}

#[test]
fn shadow_mode_changes_nothing_but_records_the_model() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_with(DeciderMode::Shadow, d.path());
    assert!(!user_msgs(&out, "Continue:").is_empty());
    assert!(user_msgs(&out, "Your answer is already sufficient").is_empty());
    // The decision is persisted beside the session files (boubou-acy.3): one line per turn
    // with both verdicts, the live model answer and what the loop acted on.
    let lines = sidecar(d.path());
    assert_eq!(lines.len(), 1, "{lines:?}");
    let r = &lines[0];
    assert_eq!(r["session"], "chat_1029_120000");
    assert_eq!(r["mode"], "shadow");
    assert_eq!(r["text"], GUESS);
    assert_eq!(r["has_progress"], false);
    assert_eq!(r["heuristic"]["intent"], "task");
    assert_eq!(r["heuristic"]["confidence"], 0.5);
    assert_eq!(r["model"]["name"], "always");
    assert_eq!(r["model"]["intent"], "question");
    assert_eq!(r["model"]["probability"], 0.93);
    assert_eq!(r["acted_on"], "task");
    assert_eq!(r["acted_on_model"], false);
    assert!(r["ts"].as_str().is_some_and(|t| t.len() >= 19));
    // The decision itself is visible through the decider.
    let cfg = DeciderConfig {
        mode: DeciderMode::Shadow,
        ..Default::default()
    };
    let mut dec = LayeredDecider::new(&cfg, Box::new(Always(Intent::Question, 0.93)));
    let r = dec.intent_of(&[Message::user(GUESS)]);
    assert_eq!(r.intent, Intent::Task);
    assert!(matches!(
        r.local,
        LocalOutcome::Answered {
            intent: Intent::Question,
            ..
        }
    ));
    assert!(r.describe().contains("DISAGREES"));
}

#[test]
fn on_mode_routes_a_guessed_task_to_qa_when_the_model_says_question() {
    let d = tempfile::tempdir().expect("tmp");
    let out = run_with(DeciderMode::On, d.path());
    let lines = sidecar(d.path());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["mode"], "on");
    assert_eq!(lines[0]["acted_on"], "question");
    assert_eq!(lines[0]["acted_on_model"], true);
    assert!(user_msgs(&out, "Continue:").is_empty());
    assert!(!user_msgs(&out, "Your answer is already sufficient").is_empty());
    assert!(out[0].content.contains("chatting with the user"));
}

#[test]
fn a_default_agent_is_heuristics_only_and_a_child_never_asks() {
    let d = tempfile::tempdir().expect("tmp");
    let a = Agent::new(RunParams::plain("gemma4:12b", d.path()), Talk, OkTools);
    assert_eq!(a.decider.mode(), DeciderMode::Off);
    assert_eq!(a.decider.backend_name(), "none");
    assert!(!a.decider.model_available());
}

#[test]
fn config_decider_section_parses_and_defaults_off() {
    let c: Config =
        serde_json::from_str(r#"{"decider": {"mode": "on", "model": "d1:3b", "threshold": 0.8}}"#)
            .expect("parse");
    assert_eq!(c.decider.mode, DeciderMode::On);
    assert_eq!(c.decider.model, "d1:3b");
    assert_eq!(c.decider.threshold, 0.8);
    assert_eq!(c.decider.timeout_ms, 1500);
    let c: Config = serde_json::from_str("{}").expect("parse");
    assert_eq!(c.decider, DeciderConfig::default());
    assert_eq!(c.decider.mode, DeciderMode::Off);
}

#[test]
fn systemone_wire_shapes_match_ollama_0_40() {
    // Request: what Ollama documents for POST /v1/systemone.
    let mut qs = BTreeMap::new();
    qs.insert(
        "intent".to_string(),
        boubou_core::decider::intent_question(),
    );
    qs.insert(
        "urgent".to_string(),
        SystemOneQuestion::Noul {
            instructions: "Is this urgent?".into(),
        },
    );
    qs.insert(
        "severity".to_string(),
        SystemOneQuestion::Score {
            instructions: "How severe?".into(),
            criteria: vec!["No impact".into(), "Minor".into(), "Major".into()],
        },
    );
    let req = SystemOneRequest {
        model: "d1:latest".into(),
        state: "hello".into(),
        questions: qs,
        keep_alive: None,
    };
    let v = serde_json::to_value(&req).expect("json");
    assert_eq!(v["model"], "d1:latest");
    assert_eq!(
        v["questions"]["urgent"],
        json!({"type": "noul", "instructions": "Is this urgent?"})
    );
    assert_eq!(v["questions"]["severity"]["type"], "score");
    assert_eq!(v["questions"]["severity"]["criteria"][2], "Major");
    assert_eq!(v["questions"]["intent"]["type"], "choice");
    assert!(v["questions"]["intent"]["criteria"]["follow_up"].is_string());
    assert!(v.get("keep_alive").is_none());

    // Response: verbatim from the live server (d1:latest, Ollama 0.40.12, 2026-10-29).
    let live = r#"{"model":"d1:latest","answers":{"intent":{"type":"choice","choice":"question","probabilities":{"question":0.8928762758179377,"task":0.09767159727541973,"follow_up":0.00945212690664249},"confidence":0.8393144137269064},"is_question":{"type":"noul","noul":0.9671647400274882},"effort":{"type":"score","score":0.4462562278337653,"legend":{"0":"none, a reply suffices","1":"a small single-step action","2":"a multi-step task"},"probabilities":{"0":0.6630074262659442,"1":0.2277289196343462,"2":0.10926365409970955},"confidence":0.330615658249352}},"usage":{"input_tokens":197,"output_tokens":0}}"#;
    let r: boubou_core::ollama::SystemOneResponse = serde_json::from_str(live).expect("parse");
    assert_eq!(r.answers.len(), 3);
    match &r.answers["intent"] {
        SystemOneAnswer::Choice {
            choice,
            probabilities,
            confidence,
        } => {
            assert_eq!(choice, "question");
            assert!((probabilities["question"] - 0.8929).abs() < 1e-3);
            assert!((confidence - 0.839).abs() < 1e-2);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(r.answers["is_question"], SystemOneAnswer::Noul { noul } if noul > 0.96));
    match &r.answers["effort"] {
        SystemOneAnswer::Score { score, legend, .. } => {
            assert!(*score < 0.5);
            assert_eq!(legend["2"], "a multi-step task");
        }
        other => panic!("{other:?}"),
    }
}
