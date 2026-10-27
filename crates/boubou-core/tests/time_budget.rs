//! Port of the Task #73 time-budget tests in `tests/test_guards.py`.

use std::collections::BTreeSet;

use boubou_core::guards::budget::{
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
    assert!(m.contains("剩约") && m.contains("分钟"));
    let m2 = budget_nudge(5400.0, 5395.0, &mut BTreeSet::new()).expect("msg");
    assert!(m2.contains("剩约 0 分钟"));
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
        ("请在40分钟内完成报告", Some(40.0)),
        ("四十分钟搞定", Some(40.0)),
        ("给我三十分钟", Some(30.0)),
        ("两个小时以内", Some(120.0)),
        ("尽量在30分钟内", Some(30.0)),
        ("１０分钟内交", Some(10.0)),
        ("你有一个半小时", Some(90.0)),
        ("一个半小时内完成", Some(90.0)),
        ("90 分钟吧", Some(90.0)),
        ("半小时内给我", Some(30.0)),
        ("半个钟头", Some(30.0)),
        ("within 40 minutes", Some(40.0)),
        ("finish it in 30 min", Some(30.0)),
        ("you have 45 mins", Some(45.0)),
        ("forty-five minutes", Some(45.0)),
        ("half an hour", Some(30.0)),
        ("an hour should be enough", Some(60.0)),
        ("You have 2 hours", Some(120.0)),
        ("1.5h", Some(90.0)),
        ("please take 1.5 hours", Some(90.0)),
        ("处理 40 个文件并生成报告", None),
        ("这个仓库的得分不低于 30 分", None),
        ("每 10 分钟检查一次进度", None),
        ("在第 5 分钟记录进度", None),
        ("前 5 分钟用来读文件", None),
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
        parse_time_budget("先在 20 分钟内完成初稿,再花 1 小时润色"),
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
        ("1小时", Some(60.0)),
        ("半小时", Some(30.0)),
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
        resolve_time_budget(Some(90.0), Some("请在 40 分钟内完成"), 5400.0, true),
        (Some(40.0), BudgetSource::Prompt)
    );
    assert_eq!(
        resolve_time_budget(Some(90.0), Some("写个报告"), 5400.0, true),
        (Some(90.0), BudgetSource::Cli)
    );
    assert_eq!(
        resolve_time_budget(None, Some("写个报告"), 5400.0, true),
        (Some(90.0), BudgetSource::Env)
    );
    assert_eq!(
        resolve_time_budget(None, Some("写个报告"), 0.0, true),
        (None, BudgetSource::Off)
    );
    // kill switch: AGENT_TIME_BUDGET_PARSE=0
    assert_eq!(
        resolve_time_budget(None, Some("请在 40 分钟内完成"), 0.0, false),
        (None, BudgetSource::Off)
    );
    assert_eq!(
        resolve_time_budget(None, Some("请在 40 分钟内完成"), 0.0, true).0,
        Some(40.0)
    );
    assert_eq!(
        resolve_time_budget(None, Some("写一个报告,分析 30 个数据文件"), 0.0, true),
        (None, BudgetSource::Off)
    );
}

#[test]
fn budget_env_value_pipeline_and_fmt_hm() {
    assert_eq!(budget_env_value(Some(40.0)), Some("2400".to_string()));
    assert_eq!(budget_env_value(Some(1.5)), Some("90".to_string()));
    assert_eq!(budget_env_value(Some(0.0)), None);
    assert_eq!(budget_env_value(None), None);
    assert_eq!(fmt_hm(40.0), "40 分钟");
    assert_eq!(fmt_hm(90.0), "90 分钟(1.5 小时)");
    assert_eq!(fmt_hm(60.0), "60 分钟(1 小时)");
}
