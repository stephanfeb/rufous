//! `try_parse_tool_calls`: small models leak tool calls into plain text in several
//! shapes. Each shape here was observed in a real run and is normalised into a call so
//! the regular gates can give corrective feedback instead of letting the turn burn.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::tools::is_known_tool;

pub type LeakedCall = (String, Map<String, Value>);

static TOOL_CALL_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<tool_call>\s*(.*?)\s*</tool_call>").expect("regex"));
static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```(?:json|tool)?\s*(.*?)\s*```").expect("regex"));
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)\b([a-zA-Z_][a-zA-Z0-9_.]*)[\({]\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*:\s*<\|"\|>(.*?)<\|"\|>\s*[\)}]"#,
    )
    .expect("regex")
});
static FUNC_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^([a-zA-Z_][a-zA-Z0-9_.]*)\s*\((.*)\)$").expect("regex"));
static FUNC_ARG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)([a-zA-Z_][a-zA-Z0-9_]*)\s*=\s*(<\|"\|>.*?<\|"\|>|"[^"]*"|'[^']*'|-?\d+(?:\.\d+)?|true|false|null)"#,
    )
    .expect("regex")
});

/// Supported shapes, in priority order:
/// 1. JSON: `{"todo": {...}}`, `{"name":"todo","arguments":{...}}`, or a list of those,
///    optionally wrapped in a ```` ```json ```` fence or `<tool_call>` tags;
/// 2. placeholder-quoted single argument: `finish(summary:<|"|>...<|"|>)` or the brace form;
/// 3. a whole-text function expression: `finish(summary="0")`, see [`parse_func_call_expr`].
pub fn try_parse_tool_calls(content: &str) -> Option<Vec<LeakedCall>> {
    let mut c = content.trim().to_string();
    if !c.starts_with(['{', '[']) {
        if c.contains("<tool_call>") {
            if let Some(m) = TOOL_CALL_TAG.captures(&c) {
                c = m[1].trim().to_string();
            }
        } else if c.contains("```") {
            if let Some(m) = FENCE.captures(&c) {
                c = m[1].trim().to_string();
            }
        }
    }
    if c.starts_with(['{', '[']) {
        if let Ok(data) = serde_json::from_str::<Value>(&c) {
            let items: Vec<Value> = match data {
                Value::Array(a) => a,
                other => vec![other],
            };
            let mut calls = Vec::new();
            for it in items {
                let Value::Object(obj) = it else { continue };
                if obj.contains_key("name") && obj.contains_key("arguments") {
                    let name = obj["name"].as_str().unwrap_or_default().to_string();
                    let args = match &obj["arguments"] {
                        Value::Object(m) => m.clone(),
                        _ => Map::new(),
                    };
                    calls.push((name, args));
                } else if obj.len() == 1 {
                    let (name, v) = obj.into_iter().next().expect("one entry");
                    if let Value::Object(m) = v {
                        calls.push((name, m));
                    }
                }
            }
            if !calls.is_empty() {
                return Some(calls);
            }
        }
    }
    let calls: Vec<LeakedCall> = PLACEHOLDER
        .captures_iter(&c)
        .map(|m| {
            let mut args = Map::new();
            args.insert(m[2].to_string(), Value::String(m[3].trim().to_string()));
            (m[1].to_string(), args)
        })
        .collect();
    if !calls.is_empty() {
        return Some(calls);
    }
    parse_func_call_expr(&c)
}

static FINISH_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|[\s.!?])finish:\s*(.+?)\s*$").expect("regex"));
static FINISH_BARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)^\s*finish\s*[\({]\s*summary\s*[:=]\s*(.+?)\s*[\)}]\s*$").expect("regex")
});

/// Profile-gated finish rescue (`ModelProfile::finish_prose_rescue`, gemma4): the model
/// ends a text-only task turn with `Finish: <summary>` on its last line, or writes a bare
/// `finish{summary: <text>}` without the placeholder quotes the generic parser needs.
/// Both are read as `finish(summary)` so the finish gates can judge the claim instead of
/// the dead-loop guard ending the run on repeated prose. The summary must be at least ten
/// characters; `Finish:` inside an earlier line is left alone.
pub fn parse_finish_prose(content: &str) -> Option<Vec<LeakedCall>> {
    let c = content.trim();
    if c.is_empty() {
        return None;
    }
    let summary = if let Some(m) = FINISH_BARE.captures(c) {
        let s = m[1].trim();
        s.trim_matches(['"', '\''])
            .trim_end_matches("<|\"|>")
            .trim_start_matches("<|\"|>")
            .trim()
            .to_string()
    } else {
        let last = c.lines().rev().find(|l| !l.trim().is_empty())?;
        let m = FINISH_LINE.captures(last.trim())?;
        m[1].trim().to_string()
    };
    if summary.chars().count() < 10 {
        return None;
    }
    let mut args = Map::new();
    args.insert("summary".to_string(), Value::String(summary));
    Some(vec![("finish".to_string(), args)])
}

/// Whole text is one `name(k=v, ...)` expression. Three rejection gates keep prose safe:
/// any surrounding prose, any uncovered fragment between arguments (nested values), and
/// unknown tool names without a dot (MCP names are `server.tool`).
pub fn parse_func_call_expr(c: &str) -> Option<Vec<LeakedCall>> {
    let m = FUNC_CALL.captures(c)?;
    let name = m[1].to_string();
    let argstr = m[2].trim();
    if !is_known_tool(&name) && !name.contains('.') {
        return None;
    }
    let mut args = Map::new();
    if !argstr.is_empty() {
        let mut pos = 0;
        for am in FUNC_ARG.captures_iter(argstr) {
            let whole = am.get(0).expect("match");
            if whole.start() != pos {
                return None;
            }
            let k = am[1].to_string();
            let v = &am[2];
            let value = if let Some(inner) = v.strip_prefix("<|\"|>") {
                Value::String(inner.strip_suffix("<|\"|>").unwrap_or(inner).to_string())
            } else if v.starts_with('"') || v.starts_with('\'') {
                Value::String(v[1..v.len() - 1].to_string())
            } else if v == "true" || v == "false" {
                Value::Bool(v == "true")
            } else if v == "null" {
                Value::Null
            } else if let Ok(i) = v.parse::<i64>() {
                Value::from(i)
            } else {
                Value::from(v.parse::<f64>().ok()?)
            };
            args.insert(k, value);
            pos = whole.end();
            while pos < argstr.len() && argstr.as_bytes()[pos].is_ascii_whitespace()
                || (pos < argstr.len() && argstr.as_bytes()[pos] == b',')
            {
                pos += 1;
            }
        }
        if pos != argstr.len() {
            return None;
        }
    }
    Some(vec![(name, args)])
}
