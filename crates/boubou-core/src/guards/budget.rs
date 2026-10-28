//! Time budget (Task #73): parsing a user-stated deadline, deciding its source, and the
//! three pacing nudges the loop injects as the budget runs down. Off by default.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::text;
use crate::todo::atomic_write_json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BudgetMark {
    Half,
    ThreeQuarters,
    Ninety,
}

impl BudgetMark {
    pub const ALL: [BudgetMark; 3] = [Self::Half, Self::ThreeQuarters, Self::Ninety];

    pub fn frac(self) -> f64 {
        match self {
            Self::Half => 0.50,
            Self::ThreeQuarters => 0.75,
            Self::Ninety => 0.90,
        }
    }

    fn template(self, remain_min: i64) -> String {
        match self {
            Self::Half => format!(
                "⏱ Time budget: about 50% used, roughly {remain_min} minutes left. Check the remaining workload: finish the core deliverables first; if one problem keeps resisting, note the current state and move to the next step rather than grinding on it."
            ),
            Self::ThreeQuarters => format!(
                "⏱ Time budget: about 75% used, roughly {remain_min} minutes left. Stop exploratory attempts: complete the artefacts that must be delivered and leave time to verify them (run the tests / check the files really exist)."
            ),
            Self::Ninety => format!(
                "⏱ Time budget: about 90% used, nearly exhausted (roughly {remain_min} minutes left). Wrap up now: claim only artefact files that really exist, then call finish. Do not start new sub-tasks."
            ),
        }
    }
}

/// `_budget_nudge`: the highest mark just crossed that has not fired yet, or `None`.
/// Lower marks are marked fired too, so a resume at 80% emits only the 75% nudge.
pub fn budget_nudge(
    budget_sec: f64,
    elapsed_sec: f64,
    fired: &mut BTreeSet<BudgetMark>,
) -> Option<String> {
    if budget_sec.is_nan() || budget_sec <= 0.0 {
        return None;
    }
    let ratio = elapsed_sec / budget_sec;
    let hit = BudgetMark::ALL
        .iter()
        .copied()
        .rev()
        .find(|m| ratio >= m.frac() && !fired.contains(m))?;
    for m in BudgetMark::ALL {
        if ratio >= m.frac() {
            fired.insert(m);
        }
    }
    let remain_min = (((budget_sec - elapsed_sec) / 60.0).round() as i64).max(0);
    Some(hit.template(remain_min))
}

/// Cross-process elapsed-time sidecar so a kill-and-resume continues the clock.
pub fn elapsed_meta_path(workdir: &Path) -> PathBuf {
    workdir.join(".agent_state.meta.json")
}

pub fn load_elapsed(workdir: &Path) -> f64 {
    std::fs::read_to_string(elapsed_meta_path(workdir))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("elapsed_sec")?.as_f64())
        .filter(|v| *v > 0.0)
        .unwrap_or(0.0)
}

pub fn save_elapsed(workdir: &Path, sec: f64) {
    let v = serde_json::json!({"elapsed_sec": (sec * 10.0).round() / 10.0});
    if let Err(e) = atomic_write_json(&elapsed_meta_path(workdir), &v) {
        tracing::warn!("elapsed sidecar: {e}");
    }
}

// ---- parsing a stated deadline ----

const NUM_ALT: &str = r"(?:\d+(?:\.\d+)?|(?:twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety)(?:-(?:one|two|three|four|five|six|seven|eight|nine))?|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen)";
const HU: &str = r"(?:hours?\b|hrs?\b|h\b)";
const MU: &str = r"(?:minutes?\b|mins?\b)";
/// Ordinal / frequency / relative-position words: the number after them is not a deadline.
const SKIP_BEFORE: &[&str] = &[
    "per", "every", "next", "last", "first", "final", "after", "before", "step", "chapter",
    "section", "page", "line",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `X and a half hours` / `X.5 hours`: (n + 0.5) hours
    HalfH,
    /// `X hours`: n hours
    H,
    Fixed30,
    Fixed60,
    /// `an hour and a half`
    Fixed90,
    /// `X minutes`: n minutes
    M,
}

struct Pat {
    re: Regex,
    kind: Kind,
    /// Python `(?<![A-Za-z0-9.])`: the char before the match must not be alnum or '.'.
    guard_before: bool,
}

static PATTERNS: LazyLock<Vec<Pat>> = LazyLock::new(|| {
    let mk = |re: String, kind, guard_before| Pat {
        re: Regex::new(&re).expect("regex"),
        kind,
        guard_before,
    };
    vec![
        mk(
            format!(r"(?P<n>{NUM_ALT})\s+and\s+a\s+half\s+{HU}"),
            Kind::HalfH,
            true,
        ),
        mk(
            format!(r"(?P<n>{NUM_ALT})\s*{HU}\s+and\s+a\s+half\b"),
            Kind::HalfH,
            true,
        ),
        mk(format!(r"(?P<n>{NUM_ALT})\s*{HU}"), Kind::H, true),
        mk(r"half\s+an?\s+hours?\b".to_string(), Kind::Fixed30, false),
        mk(r"an?\s+half\s+hours?\b".to_string(), Kind::Fixed30, false),
        mk(
            r"\ban?\s+hours?\s+and\s+a\s+half\b".to_string(),
            Kind::Fixed90,
            false,
        ),
        mk(r"\ban?\s+hours?\b".to_string(), Kind::Fixed60, false),
        mk(format!(r"(?P<n>{NUM_ALT})\s*{MU}"), Kind::M, true),
    ]
});

fn en_num(word: &str) -> Option<f64> {
    const ONES: [(&str, f64); 19] = [
        ("one", 1.0),
        ("two", 2.0),
        ("three", 3.0),
        ("four", 4.0),
        ("five", 5.0),
        ("six", 6.0),
        ("seven", 7.0),
        ("eight", 8.0),
        ("nine", 9.0),
        ("ten", 10.0),
        ("eleven", 11.0),
        ("twelve", 12.0),
        ("thirteen", 13.0),
        ("fourteen", 14.0),
        ("fifteen", 15.0),
        ("sixteen", 16.0),
        ("seventeen", 17.0),
        ("eighteen", 18.0),
        ("nineteen", 19.0),
    ];
    const TENS: [(&str, f64); 8] = [
        ("twenty", 20.0),
        ("thirty", 30.0),
        ("forty", 40.0),
        ("fifty", 50.0),
        ("sixty", 60.0),
        ("seventy", 70.0),
        ("eighty", 80.0),
        ("ninety", 90.0),
    ];
    let lookup = |t: &str| {
        ONES.iter()
            .chain(TENS.iter())
            .find(|(w, _)| *w == t)
            .map(|(_, v)| *v)
    };
    if let Some(v) = lookup(word) {
        return Some(v);
    }
    let (a, b) = word.split_once('-')?;
    let tens = TENS.iter().find(|(w, _)| *w == a)?.1;
    let ones = ONES.iter().find(|(w, _)| *w == b)?.1;
    Some(tens + ones)
}

fn value_minutes(raw: &str, kind: Kind) -> Option<f64> {
    match kind {
        Kind::Fixed30 => return Some(30.0),
        Kind::Fixed60 => return Some(60.0),
        Kind::Fixed90 => return Some(90.0),
        _ => {}
    }
    let raw = raw.trim();
    let first = raw.chars().next()?;
    let v = if first.is_ascii_digit() {
        raw.parse::<f64>().ok()?
    } else {
        en_num(&raw.to_lowercase())?
    };
    Some(match kind {
        Kind::H => v * 60.0,
        Kind::HalfH => (v + 0.5) * 60.0,
        _ => v,
    })
}

fn char_before(s: &str, byte_idx: usize) -> Option<char> {
    s[..byte_idx].chars().next_back()
}

/// `parse_time_budget`: minutes stated in the task text, or `None`.
/// Rules: an explicit unit is required (a bare number may be a score or a count); the
/// earliest hit wins, longest span on ties; ordinal/frequency/relative numbers and path
/// fragments are skipped; values under 0.5 min or over 24 h are ignored.
pub fn parse_time_budget(text: &str) -> Option<f64> {
    if text.is_empty() {
        return None;
    }
    let low = text.to_lowercase();
    // (start, -len, kind, n-group)
    let mut cands: Vec<(usize, usize, Kind, Option<String>)> = Vec::new();
    for pat in PATTERNS.iter() {
        let mut pos = 0;
        while pos <= low.len() {
            let Some(caps) = pat.re.captures_at(&low, pos) else {
                break;
            };
            let whole = caps.get(0).expect("match");
            let (s, e) = (whole.start(), whole.end());
            let mut ok = true;
            if pat.guard_before {
                if let Some(c) = char_before(&low, s) {
                    if c.is_ascii_alphanumeric() || c == '.' {
                        ok = false;
                    }
                }
            }
            if ok {
                cands.push((
                    s,
                    e,
                    pat.kind,
                    caps.name("n").map(|m| m.as_str().to_string()),
                ));
                pos = e.max(s + 1);
            } else {
                pos = s + low[s..].chars().next().map_or(1, char::len_utf8);
            }
            if e == s {
                pos += 1;
            }
        }
    }
    cands.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| (b.1 - b.0).cmp(&(a.1 - a.0))));
    for (s, _e, kind, n) in cands {
        let prefix: String = low[..s]
            .chars()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let prev = prefix.trim_end();
        if !prev.is_empty() && SKIP_BEFORE.iter().any(|b| prev.contains(b)) {
            continue;
        }
        if let Some(c) = char_before(&low, s) {
            if matches!(c, '/' | '\\' | '_') {
                continue;
            }
        }
        let Some(v) = value_minutes(n.as_deref().unwrap_or(""), kind) else {
            continue;
        };
        if !(0.5..=1440.0).contains(&v) {
            continue;
        }
        return Some((v * 100.0).round() / 100.0);
    }
    None
}

/// `parse_duration_str`: the deadline input box / `--time-budget` value. Bare numbers
/// are minutes; `1.5h`, `90m`, `2 hours`, `half an hour` go through the text parser.
pub fn parse_duration_str(s: &str) -> Option<f64> {
    static PLAIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\d+(?:\.\d+)?$").expect("regex"));
    static MINUTES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\d+(?:\.\d+)?)\s*m$").expect("regex"));
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    if PLAIN.is_match(&t) {
        let v: f64 = t.parse().ok()?;
        return (v > 0.0 && v <= 1440.0).then_some(v);
    }
    if let Some(v) = parse_time_budget(&t) {
        return Some(v);
    }
    MINUTES.captures(&t).and_then(|c| c[1].parse().ok())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    Prompt,
    Cli,
    Env,
    Off,
}

/// `resolve_time_budget`: prompt > CLI > env. `env_seconds` is `AGENT_TIME_BUDGET_SEC`;
/// `parse_prompt` is `AGENT_TIME_BUDGET_PARSE != 0` (the benchmark kill switch).
pub fn resolve_time_budget(
    cli_minutes: Option<f64>,
    task_text: Option<&str>,
    env_seconds: f64,
    parse_prompt: bool,
) -> (Option<f64>, BudgetSource) {
    if parse_prompt {
        if let Some(p) = task_text.and_then(parse_time_budget) {
            if p > 0.0 {
                return (Some(p), BudgetSource::Prompt);
            }
        }
    }
    if let Some(c) = cli_minutes.filter(|c| *c > 0.0) {
        return (Some(c), BudgetSource::Cli);
    }
    if env_seconds > 0.0 {
        return (Some(env_seconds / 60.0), BudgetSource::Env);
    }
    (None, BudgetSource::Off)
}

/// `budget_env_value`: minutes → whole seconds as the env var string; `None` = unset.
pub fn budget_env_value(minutes: Option<f64>) -> Option<String> {
    let m = minutes.filter(|m| *m > 0.0)?;
    Some(((m * 60.0).round() as i64).to_string())
}

/// `_fmt_hm`: `40 → "40 min"`, `90 → "90 min (1.5 h)"`.
pub fn fmt_hm(minutes: f64) -> String {
    let mut txt = format!("{} min", text::fmt_g(minutes));
    let hours = minutes / 60.0;
    if minutes >= 60.0 && (hours - (hours * 100.0).round() / 100.0).abs() < 1e-9 {
        txt.push_str(&format!(" ({} h)", text::fmt_g(hours)));
    }
    txt
}
