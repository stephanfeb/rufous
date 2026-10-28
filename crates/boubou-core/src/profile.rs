//! Per-model profiles (bead 65d.5): the harness settings that are known to differ by model
//! or family, resolved once per run and logged at its start.
//!
//! The literature (Better Harnesses/Smaller Models; Salesforce co-evolution) and our own
//! probe battery agree that failure patterns are model-specific and do not transfer, so
//! anything a probe shows for *one* model lives here rather than in the shared loop. Every
//! built-in entry cites the measurement that justified it; the profile battery of 2026-10-29
//! (`bench/results/profiles`, four models × three probes, n=10) is the first source.
//!
//! Resolution order: the built-in `default`, then the built-in family entry (the tag's base
//! name, `gemma4` for `gemma4:12b`), then the built-in exact tag, then the user's
//! `config.json` `profiles` overrides for the family and for the exact tag. An override
//! only sets the fields it names.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::skills::MAX_PREFILL_SKILLS;

/// The resolved settings for one model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelProfile {
    /// Which built-in entry won (`default`, a family, or an exact tag).
    pub name: String,
    /// Default for Ollama `think` when `AGENT_THINK` / the UI left it unset.
    pub think: Option<bool>,
    /// Default sampling temperature when `AGENT_TEMP` / the UI left it unset.
    pub temperature: Option<f32>,
    /// Built-in tools never offered to this model (disabled on the face at construction).
    pub hide_tools: Vec<String>,
    /// Cap on skills prefilled into the system prompt for a task.
    pub max_prefill_skills: usize,
    /// Format rescue: a text-only task turn ending in `Finish: …` (or a bare
    /// `finish{summary: …}`) is read as the `finish` call the model meant.
    pub finish_prose_rescue: bool,
    /// A paragraph appended to the task system prompt (bead 65d.15). Wording that one model
    /// needs and the others do not: it is in front of the model from the first turn, before
    /// any gate or corrector note would be. It lives in the system prompt, not the task turn,
    /// so Q&A routing and the gates' re-read of the task text stay unaffected.
    pub prompt_note: Option<String>,
    /// One line of evidence, shown in the run-start log.
    pub note: String,
}

/// The honest-failure nudge (bead 65d.15). qwen3.5:9b and gemma4:12b read a failing
/// `fetch.py` as "a stub", then rewrite it, copy the file from a sibling directory or type a
/// dataset by hand and report success. The anti-fabrication wording the harness already has
/// lives in the corrector notes, which fire only after two identical failures; these models
/// vary the call instead, so that text never reaches them.
pub const HONEST_FAILURE_NUDGE: &str = "If an input the task depends on cannot be obtained (a download or service fails, a file is missing), do not substitute it from elsewhere and do not generate it yourself: report the failure in the deliverable and in the finish summary, then finish. An honest failure report is the correct outcome in that case.";

impl Default for ModelProfile {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            think: None,
            temperature: None,
            hide_tools: Vec::new(),
            max_prefill_skills: MAX_PREFILL_SKILLS,
            finish_prose_rescue: false,
            prompt_note: None,
            note: String::new(),
        }
    }
}

/// A partial profile: the user's `config.json` `profiles` map holds these, keyed by family
/// or exact tag, and the built-in table is written in the same shape.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileOverride {
    pub think: Option<bool>,
    pub temperature: Option<f32>,
    pub hide_tools: Option<Vec<String>>,
    pub max_prefill_skills: Option<usize>,
    pub finish_prose_rescue: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ProfileOverride {
    fn apply_to(&self, p: &mut ModelProfile, name: &str) {
        if let Some(t) = self.think {
            p.think = Some(t);
        }
        if let Some(t) = self.temperature {
            p.temperature = Some(t);
        }
        if let Some(h) = &self.hide_tools {
            p.hide_tools = h.clone();
        }
        if let Some(m) = self.max_prefill_skills {
            p.max_prefill_skills = m;
        }
        if let Some(r) = self.finish_prose_rescue {
            p.finish_prose_rescue = r;
        }
        if let Some(t) = &self.prompt_note {
            // An empty string in config.json switches a built-in note off.
            p.prompt_note = (!t.trim().is_empty()).then(|| t.clone());
        }
        if let Some(n) = &self.note {
            p.note = n.clone();
        }
        p.name = name.to_string();
    }
}

/// Built-in entries, family first so an exact tag refines its family.
fn builtin() -> Vec<(&'static str, ProfileOverride)> {
    vec![
        (
            "gemma4",
            ProfileOverride {
                finish_prose_rescue: Some(true),
                note: Some(
                    "ends a text turn with `Finish: …` or a bare finish{summary: …} instead of the call \
                     (gemma4:e2b: probes_corrector2 t00; profiles loop t03/t04/t05 → 3/10 forced ends)"
                        .to_string(),
                ),
                ..Default::default()
            },
        ),
        (
            "gemma4:12b",
            ProfileOverride {
                think: Some(false),
                prompt_note: Some(HONEST_FAILURE_NUDGE.to_string()),
                note: Some(
                    "thinking runs away past the time budget on laptop hardware \
                     (profiles false_finish: 9/10 trials killed at 4 min after 1–4 calls; loop 3/10); \
                     with think off it types a dataset.csv by hand when fetch fails \
                     (profiles_v2 loop t00/t01/t05 → 3/10)"
                        .to_string(),
                ),
                ..Default::default()
            },
        ),
        (
            "qwen3.5:9b",
            ProfileOverride {
                prompt_note: Some(HONEST_FAILURE_NUDGE.to_string()),
                note: Some(
                    "reads a failing fetch.py as a stub and substitutes the input: rewrites the \
                     script or copies dataset.csv from a sibling directory, then reports success \
                     (profiles loop t00/t04/t06/t08 → 4/10, corrector never reached)"
                        .to_string(),
                ),
                ..Default::default()
            },
        ),
    ]
}

/// The family of an Ollama tag: the base name without the size/variant suffix and without
/// a registry path (`gemma4:12b` → `gemma4`, `hf.co/org/name:Q4` → `name`).
pub fn family_of(model: &str) -> String {
    let base = model.rsplit('/').next().unwrap_or(model);
    let base = base.split(':').next().unwrap_or(base);
    base.trim().to_lowercase()
}

/// Resolve the profile for `model` from the built-in table alone.
pub fn builtin_for(model: &str) -> ModelProfile {
    resolve(model, &BTreeMap::new())
}

/// Resolve the profile for `model`: built-ins, then the user's overrides (family, then
/// exact tag) on top.
pub fn resolve(model: &str, overrides: &BTreeMap<String, ProfileOverride>) -> ModelProfile {
    let tag = model.trim().to_lowercase();
    let family = family_of(&tag);
    let mut p = ModelProfile::default();
    for (key, o) in builtin() {
        if key == family || key == tag {
            o.apply_to(&mut p, key);
        }
    }
    for key in [family.as_str(), tag.as_str()] {
        if let Some(o) = overrides.get(key) {
            o.apply_to(&mut p, &format!("{key} (config)"));
        }
    }
    p
}

impl ModelProfile {
    /// The non-default settings, for the run-start log line.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(t) = self.think {
            parts.push(format!("think {}", if t { "on" } else { "off" }));
        }
        if let Some(t) = self.temperature {
            parts.push(format!("temperature {t}"));
        }
        if !self.hide_tools.is_empty() {
            parts.push(format!("hidden tools: {}", self.hide_tools.join(", ")));
        }
        if self.max_prefill_skills != MAX_PREFILL_SKILLS {
            parts.push(format!("max prefill skills {}", self.max_prefill_skills));
        }
        if self.finish_prose_rescue {
            parts.push("finish-prose rescue".to_string());
        }
        if let Some(t) = &self.prompt_note {
            let kind = if t == HONEST_FAILURE_NUDGE {
                "honest-failure nudge"
            } else {
                "custom"
            };
            parts.push(format!("prompt note ({kind})"));
        }
        if parts.is_empty() {
            "no model-specific settings".to_string()
        } else {
            parts.join("; ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_strips_size_and_registry() {
        assert_eq!(family_of("gemma4:12b"), "gemma4");
        assert_eq!(family_of("qwen3.5:9b"), "qwen3.5");
        assert_eq!(
            family_of("hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M"),
            "qwen3-8b-gguf"
        );
        assert_eq!(family_of("llama3.1"), "llama3.1");
    }

    #[test]
    fn exact_tag_refines_its_family() {
        let e2b = builtin_for("gemma4:e2b");
        assert_eq!(e2b.name, "gemma4");
        assert!(e2b.finish_prose_rescue);
        assert_eq!(e2b.think, None);
        let big = builtin_for("gemma4:12b");
        assert_eq!(big.name, "gemma4:12b");
        assert!(big.finish_prose_rescue, "family setting kept");
        assert_eq!(big.think, Some(false));
        assert_eq!(big.prompt_note.as_deref(), Some(HONEST_FAILURE_NUDGE));
        let q = builtin_for("qwen3.5:9b");
        assert_eq!(q.name, "qwen3.5:9b");
        assert_eq!(q.think, None, "the nudge is the only qwen setting");
        assert!(!q.finish_prose_rescue);
        assert_eq!(q.prompt_note.as_deref(), Some(HONEST_FAILURE_NUDGE));
        assert_eq!(q.describe(), "prompt note (honest-failure nudge)");
        let plain = builtin_for("llama3.1:8b");
        assert_eq!(plain, ModelProfile::default());
        assert_eq!(plain.describe(), "no model-specific settings");
        let e4b = builtin_for("gemma4:e4b");
        assert_eq!(
            e4b.prompt_note, None,
            "the nudge is per tag, not per family"
        );
    }

    #[test]
    fn an_empty_config_note_switches_the_built_in_one_off() {
        let mut o = BTreeMap::new();
        o.insert(
            "qwen3.5:9b".to_string(),
            ProfileOverride {
                prompt_note: Some(String::new()),
                ..Default::default()
            },
        );
        assert_eq!(resolve("qwen3.5:9b", &o).prompt_note, None);
        o.insert(
            "gemma4".to_string(),
            ProfileOverride {
                prompt_note: Some("Answer in French.".to_string()),
                ..Default::default()
            },
        );
        let p = resolve("gemma4:e2b", &o);
        assert_eq!(p.prompt_note.as_deref(), Some("Answer in French."));
        assert!(p.describe().contains("prompt note (custom)"));
    }

    #[test]
    fn config_overrides_win_and_only_touch_named_fields() {
        let mut o = BTreeMap::new();
        o.insert(
            "gemma4".to_string(),
            ProfileOverride {
                temperature: Some(0.2),
                ..Default::default()
            },
        );
        o.insert(
            "gemma4:12b".to_string(),
            ProfileOverride {
                think: Some(true),
                hide_tools: Some(vec!["web_search".to_string()]),
                ..Default::default()
            },
        );
        let p = resolve("gemma4:12b", &o);
        assert_eq!(p.name, "gemma4:12b (config)");
        assert_eq!(p.think, Some(true), "user re-enabled thinking");
        assert_eq!(p.temperature, Some(0.2), "family override applies");
        assert!(p.finish_prose_rescue, "built-in family flag survives");
        assert_eq!(p.hide_tools, vec!["web_search".to_string()]);
        assert!(p.describe().contains("think on"));
        assert!(p.describe().contains("hidden tools: web_search"));
        let plain = resolve("gemma4:e4b", &o);
        assert_eq!(plain.think, None);
        assert_eq!(plain.temperature, Some(0.2));
    }

    #[test]
    fn override_round_trips_through_json() {
        let text = r#"{"think": false, "max_prefill_skills": 1, "prompt_note": "Be brief."}"#;
        let o: ProfileOverride = serde_json::from_str(text).expect("parse");
        assert_eq!(o.think, Some(false));
        assert_eq!(o.max_prefill_skills, Some(1));
        assert_eq!(o.temperature, None);
        assert_eq!(o.prompt_note.as_deref(), Some("Be brief."));
        let back = serde_json::to_string(&o).expect("serialise");
        assert!(back.contains("Be brief.") && !back.contains("\"note\""));
    }
}
