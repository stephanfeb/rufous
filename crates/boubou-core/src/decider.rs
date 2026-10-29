//! System One decider (bead boubou-acy, `docs/system-one-decider.md`): typed decisions over a
//! block of loop state, heuristic first, a local decision model second.
//!
//! Boubou decides a dozen things per turn by keyword list or threshold. Those lists are
//! deterministic and testable and fail when a case they did not anticipate flips the loop
//! ("Are you capable of software development ?" routed as a task, 2026-10-28). A decision
//! model answers a typed question over a block of state in one forward pass with a calibrated
//! probability and no free text. Ollama 0.40 serves such models natively
//! (`POST /v1/systemone`, question types noul / choice / score; `ollama pull d1`).
//!
//! Shape: a call site asks a named question through a policy function here
//! ([`LayeredDecider::intent_of`]). The heuristic answers first and reports an honest
//! confidence: 1.0 when a rule matched, 0.5 when it fell through to a default. The local
//! model is consulted according to [`DeciderMode`]: `off` (default) never; `shadow` always,
//! with the answer logged and the heuristic still acting, so transcripts become the labelled
//! set the design asks for before adoption; `on` only where the heuristic was guessing
//! (confidence below `threshold`), and then the model's answer acts. A failing or absent model
//! never changes behaviour: the heuristic answer stands, and after
//! [`MAX_CONSECUTIVE_FAILURES`] the model is not asked again this run.
//!
//! The safety gates (`safety.rs`, the child sandbox) are not call sites and never will be.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::guards::qa::{self, HeuristicIntent, Intent};
use crate::ollama::{Client, Message, SystemOneAnswer, SystemOneQuestion, SystemOneRequest};

/// Consecutive backend failures after which the model is left alone for the rest of the run.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeciderMode {
    /// Heuristics only (today's behaviour).
    #[default]
    Off,
    /// Ask the model every time, log its answer, act on the heuristic.
    Shadow,
    /// Act on the model where the heuristic confidence is below `threshold`.
    On,
}

/// `config.json` `decider` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeciderConfig {
    pub mode: DeciderMode,
    /// Ollama tag of a model with the `decision` capability.
    pub model: String,
    /// Per-call HTTP timeout; on expiry the heuristic answer stands.
    pub timeout_ms: u64,
    /// In `on` mode the model acts only when the heuristic confidence is below this. The
    /// heuristic reports 1.0 on a rule hit and 0.5 on a default, so 0.9 means "only where
    /// the heuristic was guessing".
    pub threshold: f64,
}

impl Default for DeciderConfig {
    fn default() -> Self {
        Self {
            mode: DeciderMode::Off,
            model: "d1:latest".to_string(),
            timeout_ms: 1500,
            threshold: 0.9,
        }
    }
}

/// A local decision model: one request carries the state and named typed questions.
pub trait DecisionBackend: Send {
    fn name(&self) -> String;
    fn decide(
        &mut self,
        state: &str,
        questions: BTreeMap<String, SystemOneQuestion>,
    ) -> Result<BTreeMap<String, SystemOneAnswer>, String>;
}

/// No model configured: every ask fails and the heuristic stands.
#[derive(Debug, Default)]
pub struct NullBackend;

impl DecisionBackend for NullBackend {
    fn name(&self) -> String {
        "none".to_string()
    }
    fn decide(
        &mut self,
        _state: &str,
        _questions: BTreeMap<String, SystemOneQuestion>,
    ) -> Result<BTreeMap<String, SystemOneAnswer>, String> {
        Err("no decision model configured".to_string())
    }
}

/// Ollama's native decision API (`/v1/systemone`), driven synchronously from the loop.
#[derive(Debug, Clone)]
pub struct OllamaSystemOne {
    client: Client,
    model: String,
    timeout: Duration,
}

impl OllamaSystemOne {
    pub fn new(host: impl Into<String>, model: impl Into<String>, timeout: Duration) -> Self {
        Self {
            client: Client::new(host),
            model: model.into(),
            timeout,
        }
    }

    /// One blocking call, usable from the sync loop and from an async context alike.
    pub fn ask(
        &self,
        state: &str,
        questions: BTreeMap<String, SystemOneQuestion>,
    ) -> Result<BTreeMap<String, SystemOneAnswer>, String> {
        let req = SystemOneRequest {
            model: self.model.clone(),
            state: state.to_string(),
            questions,
            keep_alive: None,
        };
        let client = self.client.clone();
        let timeout = self.timeout;
        crate::ollama_proc::block_on_fresh(async move { client.systemone(&req, timeout).await })
            .map(|r| r.answers)
            .map_err(|e| e.to_string())
    }
}

impl DecisionBackend for OllamaSystemOne {
    fn name(&self) -> String {
        format!("ollama systemone · {}", self.model)
    }
    fn decide(
        &mut self,
        state: &str,
        questions: BTreeMap<String, SystemOneQuestion>,
    ) -> Result<BTreeMap<String, SystemOneAnswer>, String> {
        self.ask(state, questions)
    }
}

/// What the local model said about the intent, if it was asked.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalOutcome {
    NotAsked,
    Answered {
        model: String,
        intent: Intent,
        /// Probability of the chosen option.
        probability: f64,
        /// The model's own confidence that one option dominates (0..1).
        confidence: f64,
    },
    Failed(String),
}

/// The intent decision for a run, with everything the log and the labelled set need.
#[derive(Debug, Clone, PartialEq)]
pub struct IntentDecision {
    /// The intent the loop acts on.
    pub intent: Intent,
    /// Confidence behind `intent`: the heuristic's (1.0 / 0.5) or the model's probability.
    pub confidence: f64,
    /// Whether `intent` came from the heuristic or the model.
    pub acted_on_model: bool,
    pub heuristic: HeuristicIntent,
    pub local: LocalOutcome,
    pub latency_ms: u32,
}

impl IntentDecision {
    /// One sidecar line (`<sessions>/<session>.decisions.jsonl`, bead boubou-acy.3): the
    /// text and progress state the heuristic saw, both verdicts and what the loop acted on,
    /// so shadow runs become labelling candidates without replaying transcripts.
    pub fn record(&self, mode: DeciderMode, session: &str) -> serde_json::Value {
        let mut v = serde_json::json!({
            "ts": chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "session": session,
            "mode": mode,
            "text": self.heuristic.text,
            "has_progress": self.heuristic.has_progress,
            "heuristic": {
                "intent": self.heuristic.intent.as_str(),
                "rule": self.heuristic.rule,
                "confidence": self.heuristic.confidence,
            },
            "model": serde_json::Value::Null,
            "latency_ms": self.latency_ms,
            "acted_on": self.intent.as_str(),
            "acted_on_model": self.acted_on_model,
        });
        match &self.local {
            LocalOutcome::NotAsked => {}
            LocalOutcome::Answered {
                model,
                intent,
                probability,
                confidence,
            } => {
                v["model"] = serde_json::json!({
                    "name": model,
                    "intent": intent.as_str(),
                    "probability": probability,
                    "confidence": confidence,
                });
            }
            LocalOutcome::Failed(e) => {
                v["model_error"] = serde_json::Value::String(e.clone());
            }
        }
        v
    }

    /// One log line: `intent=question ← heuristic (trailing `?`, 1.00); d1: question 0.89 (290 ms)`.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "intent={} ← {} ({}, {:.2})",
            self.intent.as_str(),
            if self.acted_on_model {
                "model"
            } else {
                "heuristic"
            },
            self.heuristic.rule,
            self.heuristic.confidence
        );
        match &self.local {
            LocalOutcome::NotAsked => {}
            LocalOutcome::Answered {
                model,
                intent,
                probability,
                confidence,
            } => s.push_str(&format!(
                "; {model}: {} p={probability:.2} conf={confidence:.2} ({} ms){}",
                intent.as_str(),
                self.latency_ms,
                if *intent != self.heuristic.intent {
                    " DISAGREES"
                } else {
                    ""
                }
            )),
            LocalOutcome::Failed(e) => {
                s.push_str(&format!("; model unavailable ({e}), heuristic stands"))
            }
        }
        s
    }
}

/// Heuristic first, model second, heuristic on any failure.
pub struct LayeredDecider {
    mode: DeciderMode,
    threshold: f64,
    backend: Box<dyn DecisionBackend>,
    consecutive_failures: u32,
    /// Calls made to the backend this run (tests, doctor).
    pub asked: u32,
}

impl std::fmt::Debug for LayeredDecider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayeredDecider")
            .field("mode", &self.mode)
            .field("threshold", &self.threshold)
            .field("backend", &self.backend.name())
            .finish()
    }
}

impl Default for LayeredDecider {
    fn default() -> Self {
        Self::off()
    }
}

impl LayeredDecider {
    /// Heuristics only.
    pub fn off() -> Self {
        Self::new(&DeciderConfig::default(), Box::new(NullBackend))
    }

    pub fn new(cfg: &DeciderConfig, backend: Box<dyn DecisionBackend>) -> Self {
        Self {
            mode: cfg.mode,
            threshold: cfg.threshold,
            backend,
            consecutive_failures: 0,
            asked: 0,
        }
    }

    /// From `config.json`: the Ollama backend when the mode is not `off`.
    pub fn from_config(cfg: &DeciderConfig, ollama_host: &str) -> Self {
        if cfg.mode == DeciderMode::Off {
            return Self::off();
        }
        let b = OllamaSystemOne::new(
            ollama_host,
            cfg.model.clone(),
            Duration::from_millis(cfg.timeout_ms.max(1)),
        );
        Self::new(cfg, Box::new(b))
    }

    pub fn mode(&self) -> DeciderMode {
        self.mode
    }

    pub fn backend_name(&self) -> String {
        self.backend.name()
    }

    /// Whether the model would still be asked (mode and failure count).
    pub fn model_available(&self) -> bool {
        self.mode != DeciderMode::Off && self.consecutive_failures < MAX_CONSECUTIVE_FAILURES
    }

    /// Question / task / follow-up for the latest real user message. Replaces the bare
    /// `qa::is_qa` call: identical answer in `off` mode.
    pub fn intent_of(&mut self, messages: &[Message]) -> IntentDecision {
        let h = qa::intent_heuristic(messages, qa::has_task_progress(messages));
        let mut d = IntentDecision {
            intent: h.intent,
            confidence: h.confidence,
            acted_on_model: false,
            heuristic: h.clone(),
            local: LocalOutcome::NotAsked,
            latency_ms: 0,
        };
        if !self.model_available() || h.text.is_empty() {
            return d;
        }
        if self.mode == DeciderMode::On && h.confidence >= self.threshold {
            return d;
        }
        let state = intent_state(&h);
        let mut qs = BTreeMap::new();
        qs.insert("intent".to_string(), intent_question());
        let t = Instant::now();
        self.asked += 1;
        match self.backend.decide(&state, qs) {
            Ok(answers) => match answers.get("intent") {
                Some(SystemOneAnswer::Choice {
                    choice,
                    probabilities,
                    confidence,
                }) => {
                    self.consecutive_failures = 0;
                    d.latency_ms = t.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
                    let intent = Intent::parse(choice).unwrap_or(h.intent);
                    let p = probabilities.get(choice).copied().unwrap_or(0.0);
                    d.local = LocalOutcome::Answered {
                        model: self.backend.name(),
                        intent,
                        probability: p,
                        confidence: *confidence,
                    };
                    if self.mode == DeciderMode::On {
                        d.intent = intent;
                        d.confidence = p;
                        d.acted_on_model = true;
                    }
                }
                other => {
                    self.consecutive_failures += 1;
                    d.local = LocalOutcome::Failed(format!("unexpected answer shape: {other:?}"));
                }
            },
            Err(e) => {
                self.consecutive_failures += 1;
                d.local = LocalOutcome::Failed(e);
            }
        }
        d
    }
}

/// The state block for the intent question: the message the heuristic classified plus the
/// one fact the heuristic itself uses. Short by design (a decision is a prefill).
pub fn intent_state(h: &HeuristicIntent) -> String {
    format!(
        "Latest user message to a coding/office agent:\n\"\"\"\n{}\n\"\"\"\n\nSession so far: {}",
        h.text,
        if h.has_progress {
            "the agent has already made tool calls in this session (files written or commands run)."
        } else {
            "no tool calls have been made yet."
        }
    )
}

/// The intent question, as the model sees it.
pub fn intent_question() -> SystemOneQuestion {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        Intent::Question.as_str().to_string(),
        "a question, greeting or small talk that expects a text answer, not work".to_string(),
    );
    criteria.insert(
        Intent::Task.as_str().to_string(),
        "an instruction to do work with tools: write, create, run, fix, analyse, produce something"
            .to_string(),
    );
    criteria.insert(
        Intent::FollowUp.as_str().to_string(),
        "a short continuation, correction or hint about work already under way".to_string(),
    );
    SystemOneQuestion::Choice {
        instructions: "What is the user doing with this message?".to_string(),
        criteria,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted(Result<(Intent, f64), String>);
    impl DecisionBackend for Scripted {
        fn name(&self) -> String {
            "scripted".into()
        }
        fn decide(
            &mut self,
            _s: &str,
            _q: BTreeMap<String, SystemOneQuestion>,
        ) -> Result<BTreeMap<String, SystemOneAnswer>, String> {
            let (intent, p) = self.0.clone()?;
            let mut probs = BTreeMap::new();
            for i in [Intent::Question, Intent::Task, Intent::FollowUp] {
                probs.insert(
                    i.as_str().to_string(),
                    if i == intent { p } else { (1.0 - p) / 2.0 },
                );
            }
            let mut m = BTreeMap::new();
            m.insert(
                "intent".to_string(),
                SystemOneAnswer::Choice {
                    choice: intent.as_str().to_string(),
                    probabilities: probs,
                    confidence: p,
                },
            );
            Ok(m)
        }
    }

    fn cfg(mode: DeciderMode) -> DeciderConfig {
        DeciderConfig {
            mode,
            ..Default::default()
        }
    }
    // Heuristic falls through: long, no cue, no `?` → task at 0.5.
    const GUESS: &str =
        "I was wondering whether this repository has anything to do with authentication tokens";

    #[test]
    fn off_mode_is_the_heuristic_and_never_asks() {
        let mut d = LayeredDecider::new(
            &cfg(DeciderMode::Off),
            Box::new(Scripted(Ok((Intent::Question, 0.95)))),
        );
        let r = d.intent_of(&[Message::user(GUESS)]);
        assert_eq!(r.intent, Intent::Task);
        assert_eq!(r.confidence, 0.5);
        assert_eq!(r.local, LocalOutcome::NotAsked);
        assert_eq!(d.asked, 0);
        assert!(!r.acted_on_model);
    }

    #[test]
    fn shadow_mode_asks_logs_and_acts_on_the_heuristic() {
        let mut d = LayeredDecider::new(
            &cfg(DeciderMode::Shadow),
            Box::new(Scripted(Ok((Intent::Question, 0.95)))),
        );
        let r = d.intent_of(&[Message::user(GUESS)]);
        assert_eq!(r.intent, Intent::Task, "heuristic still acts");
        assert!(!r.acted_on_model);
        assert!(matches!(
            r.local,
            LocalOutcome::Answered {
                intent: Intent::Question,
                ..
            }
        ));
        assert!(r.describe().contains("DISAGREES"), "{}", r.describe());
        // Shadow asks even where the heuristic was sure.
        let r2 = d.intent_of(&[Message::user("Write a haiku about rain")]);
        assert_eq!(r2.heuristic.confidence, 1.0);
        assert_eq!(d.asked, 2);
        assert_eq!(r2.intent, Intent::Task);
        assert!(!r2.acted_on_model);
    }

    #[test]
    fn on_mode_acts_on_the_model_only_where_the_heuristic_guessed() {
        let mut d = LayeredDecider::new(
            &cfg(DeciderMode::On),
            Box::new(Scripted(Ok((Intent::Question, 0.95)))),
        );
        let r = d.intent_of(&[Message::user(GUESS)]);
        assert_eq!(r.intent, Intent::Question);
        assert!(r.acted_on_model);
        assert_eq!(r.confidence, 0.95);
        assert!(r.describe().starts_with("intent=question ← model"));
        // A rule hit is not second-guessed.
        let r2 = d.intent_of(&[Message::user("Write a haiku about rain")]);
        assert_eq!(r2.intent, Intent::Task);
        assert!(!r2.acted_on_model);
        assert_eq!(r2.local, LocalOutcome::NotAsked);
        assert_eq!(d.asked, 1);
    }

    #[test]
    fn a_failing_model_leaves_the_heuristic_standing_and_is_dropped_after_three_failures() {
        let mut d = LayeredDecider::new(
            &cfg(DeciderMode::On),
            Box::new(Scripted(Err("connection refused".into()))),
        );
        for _ in 0..MAX_CONSECUTIVE_FAILURES {
            let r = d.intent_of(&[Message::user(GUESS)]);
            assert_eq!(r.intent, Intent::Task);
            assert!(!r.acted_on_model);
            assert!(matches!(r.local, LocalOutcome::Failed(_)));
            assert!(r.describe().contains("heuristic stands"));
        }
        assert!(!d.model_available());
        let r = d.intent_of(&[Message::user(GUESS)]);
        assert_eq!(r.local, LocalOutcome::NotAsked);
        assert_eq!(d.asked, MAX_CONSECUTIVE_FAILURES);
    }

    #[test]
    fn config_section_parses_with_defaults() {
        let c: DeciderConfig = serde_json::from_str(r#"{"mode":"shadow"}"#).expect("parse");
        assert_eq!(c.mode, DeciderMode::Shadow);
        assert_eq!(c.model, "d1:latest");
        assert_eq!(c.timeout_ms, 1500);
        let d = LayeredDecider::from_config(&DeciderConfig::default(), "http://127.0.0.1:11434");
        assert_eq!(d.backend_name(), "none");
        let d = LayeredDecider::from_config(&c, "http://127.0.0.1:11434/");
        assert_eq!(d.backend_name(), "ollama systemone · d1:latest");
        assert!(d.model_available());
    }

    #[test]
    fn the_intent_question_names_the_three_intents() {
        let SystemOneQuestion::Choice { criteria, .. } = intent_question() else {
            panic!("choice")
        };
        assert_eq!(
            criteria.keys().cloned().collect::<Vec<_>>(),
            vec!["follow_up", "question", "task"]
        );
        let h = qa::intent_heuristic(&[Message::user("hello there")], true);
        let s = intent_state(&h);
        assert!(s.contains("hello there") && s.contains("already made tool calls"));
    }
}
