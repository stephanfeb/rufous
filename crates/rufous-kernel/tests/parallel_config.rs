//! Port of `tests/test_parallel_config.py`: deep merge, env overrides, invalid values fall
//! back, type coercion, factory defaults all off, rule lists are data.

use std::collections::HashMap;

use rufous_kernel::config::Config;
use rufous_kernel::parallel::{current_depth, hard_disabled, CommandRule, ParallelConfig};
use serde_json::{json, Value};

fn from(v: Value) -> ParallelConfig {
    ParallelConfig::from_map(v.as_object().expect("object"))
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn defaults_are_strict() {
    let d = ParallelConfig::default();
    assert!(!d.enabled);
    assert_eq!(d.child_model, ""); // zero hard-coding: the model must be configured
    assert_eq!(d.hard_cap, 2);
    assert!(!d.allow_parent_eviction);
    assert!(!d.allow_overwrite_existing);
    assert!(!d.child_allow_commands);
    assert!(!d.retry_on_severe);
    assert_eq!(d.max_depth, 1);
    assert!(!d.child_allowed_tools.iter().any(|t| t == "run_bash"));
    assert!(!d.child_allowed_tools.iter().any(|t| t == "enable_tools"));
    assert_eq!(d.min_pending_items, 6);
    assert_eq!(d.child_ctx, 8192);
}

#[test]
fn user_config_overrides_defaults_and_unknown_keys_ignored() {
    let c =
        from(json!({"enabled": true, "hard_cap": 3, "child_model": "gemma4:e2b", "nonsense": 1}));
    assert!(c.enabled);
    assert_eq!(c.hard_cap, 3);
    assert_eq!(c.child_model, "gemma4:e2b");
}

#[test]
fn env_overrides_config() {
    let c = from(json!({"hard_cap": 3})).with_env(&env(&[
        ("AGENT_PARALLEL_MAX", "1"),
        ("AGENT_PARALLEL_MODEL", "qwen3.5:4b"),
    ]));
    assert_eq!(c.hard_cap, 1);
    assert_eq!(c.child_model, "qwen3.5:4b");
}

#[test]
fn invalid_values_fall_back_not_crash() {
    let c =
        from(json!({"hard_cap": "abc", "child_ctx": null, "enabled": "yes", "max_retries": -4}));
    assert_eq!(c.hard_cap, 2);
    assert!(c.enabled);
    assert_eq!(c.child_ctx, 8192);
    assert_eq!(c.max_retries, 1);
    let c = from(json!({"hard_cap": "3.9", "per_task_timeout_s": "120"}));
    assert_eq!(c.hard_cap, 3);
    assert_eq!(c.per_task_timeout_s, 120.0);
}

#[test]
fn kill_switch_and_depth() {
    assert!(hard_disabled(&env(&[("AGENT_PARALLEL", "0")])));
    assert!(hard_disabled(&env(&[("AGENT_PARALLEL", "false")])));
    assert!(!hard_disabled(&env(&[])));
    assert_eq!(current_depth(&env(&[])), 0);
    assert_eq!(current_depth(&env(&[("MINGBIRD_DEPTH", "2")])), 2);
    assert_eq!(current_depth(&env(&[("HUMMINGBIRD_DEPTH", "2")])), 2);
    assert_eq!(
        current_depth(&env(&[("MINGBIRD_DEPTH", "3"), ("HUMMINGBIRD_DEPTH", "1")])),
        3
    );
    assert_eq!(current_depth(&env(&[("HUMMINGBIRD_DEPTH", "garbage")])), 0);
}

#[test]
fn rule_lists_are_data_not_code() {
    let strict = ParallelConfig::default();
    assert!(strict.sensitive_dir_patterns.is_empty()); // empty = factory strictest default
    assert!(strict.dangerous_command_patterns.is_empty());
    let c = from(json!({"extra_dangerous_command_patterns": ["mycompany_tool"]}));
    assert_eq!(
        c.extra_dangerous_command_patterns,
        vec![CommandRule::deny("mycompany_tool", "user_rule")]
    );
    let c2 =
        from(json!({"dangerous_command_patterns": [{"pattern": "foo", "severity": "severe"}]}));
    assert_eq!(c2.dangerous_command_patterns.len(), 1);
    assert!(c2.dangerous_command_patterns[0].is_severe());
}

#[test]
fn no_model_name_hardcoded_anywhere() {
    // Zero hard-coding: no concrete model tag in the dispatch logic.
    let sources = [
        include_str!("../src/parallel/config.rs"),
        include_str!("../src/parallel/probe.rs"),
        include_str!("../src/parallel/trigger.rs"),
        include_str!("../src/parallel/dispatch.rs"),
        include_str!("../src/parallel/sandbox.rs"),
        include_str!("../src/parallel/hook.rs"),
    ];
    for src in sources {
        for tag in ["gemma4", "qwen3.5", "ornith"] {
            assert!(
                !src.contains(tag),
                "{tag} is hard-coded in the parallel module"
            );
        }
    }
}

#[test]
fn load_config_keeps_parallel_section() {
    let d = tempfile::tempdir().expect("tmp");
    let f = d.path().join("config.json");
    std::fs::write(&f, r#"{"parallel": {"enabled": true, "hard_cap": 3}}"#).expect("write");
    let cfg = Config::load_from(&f).expect("load");
    assert_eq!(cfg.parallel.get("enabled"), Some(&Value::Bool(true)));
    let c = ParallelConfig::from_config(&cfg).with_env(&env(&[]));
    assert!(c.enabled);
    assert_eq!(c.hard_cap, 3);
}
