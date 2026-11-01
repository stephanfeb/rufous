//! Port of the Task #73 time-budget tests in `tests/test_guards.py`.

use std::collections::BTreeSet;

use rufous_kernel::guards::budget::{
    budget_env_value, budget_nudge, elapsed_meta_path, fmt_hm, load_elapsed, parse_duration_str,
    parse_time_budget, resolve_time_budget, save_elapsed, BudgetMark, BudgetSource,
};

#[test]
fn budget_disabled_returns_none() {
    let mut fired = BTreeSet::new();
    assert!(budget_nudge(0.0, 1e9, &mut fired).is_none());
    assert!(budget_nudge(-5.0, 100.0, &mut fired).is_none());
}

#[test]
fn budget_marks_fire_once_each() {
    let mut fired = BTreeSet::new();
    let b = 5400.0;
    let m1 = budget_nudge(b, 0.51 * b, &mut fired).expect("50%");
    assert!(m1.contains("50%"));
    assert_eq!(fired, [BudgetMark::Half].into_iter().collect());
    assert!(budget_nudge(b, 0.60 * b, &mut fired).is_none());
    assert!(budget_nudge(b, 0.80 * b, &mut fired)
        .expect("75%")
        .contains("75%"));
    assert!(budget_nudge(b, 0.95 * b, &mut fired)
        .expect("90%")
        .contains("90%"));
    assert!(budget_nudge(b, 0.99 * b, &mut fired).is_none());
    assert_eq!(fired, BudgetMark::ALL.into_iter().collect());
}

#[test]
fn budget_catchup_fires_highest_mark_only() {
    let mut fired = BTreeSet::new();
    let m = budget_nudge(3600.0, 0.80 * 3600.0, &mut fired).expect("75%");
    assert!(m.contains("75%") && !m.contains("50%"));
    assert!(budget_nudge(3600.0, 0.82 * 3600.0, &mut fired).is_none());
    assert!(budget_nudge(3600.0, 0.95 * 3600.0, &mut fired)
        .expect("90%")
        .contains("90%"));
}

#[test]
fn budget_message_has_remaining_minutes() {
    let m = budget_nudge(5400.0, 0.51 * 5400.0, &mut BTreeSet::new()).expect("msg");
    assert!(m.contains("roughly") && m.contains("minutes left"));
    let m2 = budget_nudge(5400.0, 5395.0, &mut BTreeSet::new()).expect("msg");
    assert!(m2.contains("roughly 0 minutes left"));
}

#[test]
fn budget_marks_are_ordered_and_ascending() {
    let fracs: Vec<f64> = BudgetMark::ALL.iter().map(|m| m.frac()).collect();
    assert!(fracs.windows(2).all(|w| w[0] < w[1]) && fracs.len() == 3);
}

#[test]
fn elapsed_meta_roundtrip() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    assert_eq!(load_elapsed(wd), 0.0);
    save_elapsed(wd, 123.4);
    assert_eq!(load_elapsed(wd), 123.4);
    std::fs::write(elapsed_meta_path(wd), "not json").expect("write");
    assert_eq!(load_elapsed(wd), 0.0);
    save_elapsed(wd, -1.0);
    assert_eq!(load_elapsed(wd), 0.0);
    let p = elapsed_meta_path(std::path::Path::new("some/wd"));
    assert_eq!(
        p.file_name().and_then(|n| n.to_str()),
        Some(".agent_state.meta.json")
    );
    assert!(p.parent().expect("dir").ends_with("wd"));
}

#[test]
fn parse_time_budget_table() {
    let table: &[(&str, Option<f64>)] = &[
        ("finish the report within 40minutes", Some(40.0)),
        ("forty minutes should do", Some(40.0)),
        ("give me thirty minutes", Some(30.0)),
        ("within two hours", Some(120.0)),
        ("try to do it in 30 minutes", Some(30.0)),
        ("deliver within 10 minutes", Some(10.0)),
        ("you have an hour and a half", Some(90.0)),
        ("one and a half hours to finish", Some(90.0)),
        ("two hours and a half", Some(150.0)),
        ("90 minutes then", Some(90.0)),
        ("half an hour, please", Some(30.0)),
        ("I'd say 1 hr", Some(60.0)),
        ("within 40 minutes", Some(40.0)),
        ("finish it in 30 min", Some(30.0)),
        ("you have 45 mins", Some(45.0)),
        ("forty-five minutes", Some(45.0)),
        ("half an hour", Some(30.0)),
        ("an hour should be enough", Some(60.0)),
        ("You have 2 hours", Some(120.0)),
        ("1.5h", Some(90.0)),
        ("please take 1.5 hours", Some(90.0)),
        ("process 40 files and generate a report", None),
        ("the repository must score at least 30 points", None),
        ("check progress every 10 minutes", None),
        ("after 5 minutes, log the progress", None),
        ("spend the first 5 minutes reading the files", None),
        ("the first 40 minutes are for reading", None),
        ("read docs/40-minutes.md", None),
        ("see the 40-minutes section", None),
        ("i will be minimal in output", None),
        ("", None),
    ];
    for (text, expect) in table {
        assert_eq!(parse_time_budget(text), *expect, "{text:?}");
    }
}

#[test]
fn parse_time_budget_earliest_wins_and_rejects_nonsense() {
    assert_eq!(
        parse_time_budget("finish the first draft within 20 minutes, then spend 1 hour polishing"),
        Some(20.0)
    );
    assert_eq!(parse_time_budget("40 h"), None);
    assert_eq!(parse_time_budget("0.2 min"), None);
}

#[test]
fn parse_duration_str_table() {
    let table: &[(&str, Option<f64>)] = &[
        ("40", Some(40.0)),
        ("1.5h", Some(90.0)),
        ("90m", Some(90.0)),
        ("1 hour", Some(60.0)),
        ("half an hour", Some(30.0)),
        (" 40 ", Some(40.0)),
        ("0", None),
        ("-5", None),
        ("abc", None),
        ("", None),
    ];
    for (raw, expect) in table {
        assert_eq!(parse_duration_str(raw), *expect, "{raw:?}");
    }
}

#[test]
fn budget_priority_prompt_cli_env_off() {
    assert_eq!(
        resolve_time_budget(
            Some(90.0),
            Some("please finish within 40 minutes"),
            5400.0,
            true
        ),
        (Some(40.0), BudgetSource::Prompt)
    );
    assert_eq!(
        resolve_time_budget(Some(90.0), Some("write a report"), 5400.0, true),
        (Some(90.0), BudgetSource::Cli)
    );
    assert_eq!(
        resolve_time_budget(None, Some("write a report"), 5400.0, true),
        (Some(90.0), BudgetSource::Env)
    );
    assert_eq!(
        resolve_time_budget(None, Some("write a report"), 0.0, true),
        (None, BudgetSource::Off)
    );
    // kill switch: AGENT_TIME_BUDGET_PARSE=0
    assert_eq!(
        resolve_time_budget(None, Some("please finish within 40 minutes"), 0.0, false),
        (None, BudgetSource::Off)
    );
    assert_eq!(
        resolve_time_budget(None, Some("please finish within 40 minutes"), 0.0, true).0,
        Some(40.0)
    );
    assert_eq!(
        resolve_time_budget(
            None,
            Some("write a report analysing 30 data files"),
            0.0,
            true
        ),
        (None, BudgetSource::Off)
    );
}

#[test]
fn budget_env_value_pipeline_and_fmt_hm() {
    assert_eq!(budget_env_value(Some(40.0)), Some("2400".to_string()));
    assert_eq!(budget_env_value(Some(1.5)), Some("90".to_string()));
    assert_eq!(budget_env_value(Some(0.0)), None);
    assert_eq!(budget_env_value(None), None);
    assert_eq!(fmt_hm(40.0), "40 min");
    assert_eq!(fmt_hm(90.0), "90 min (1.5 h)");
    assert_eq!(fmt_hm(60.0), "60 min (1 h)");
}
