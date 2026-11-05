//! `try_parse_tool_calls`: small models leak tool calls into plain text in several
//! shapes. Each shape here was observed in a real run and is normalised into a call so
//! the regular gates can give corrective feedback instead of letting the turn burn.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::ollama::{FunctionCall, ToolCall};
use crate::tools::is_known_tool;

pub type LeakedCall = (String, Map<String, Value>);

/// Tool calls salvaged from a text reply, and whether the profile's finish-prose rescue
/// found them.
#[derive(Debug, Clone, PartialEq)]
pub struct Salvaged {
    pub calls: Vec<ToolCall>,
    pub rescued: bool,
    /// The calls were in Gemma 4's own syntax (`call:name{…}`), see [`parse_gemma_native`].
    pub gemma: bool,
}

/// Format-leak salvage: the model wrote its call as text. The generic shapes first
/// ([`try_parse_tool_calls`]); the finish-prose rescue ([`parse_finish_prose`]) only when
/// `finish_prose_rescue` is set, for models known to end a text turn with `Finish: …`.
pub fn salvage(content: &str, finish_prose_rescue: bool) -> Option<Salvaged> {
    let (calls, rescued, gemma) = match parse_shapes(content) {
        Some((c, gemma)) => (c, false, gemma),
        None if finish_prose_rescue => (parse_finish_prose(content)?, true, false),
        None => return None,
    };
    Some(Salvaged {
        calls: calls
            .into_iter()
            .map(|(name, args)| ToolCall {
                function: FunctionCall {
                    name,
                    arguments: Value::Object(args),
                },
            })
            .collect(),
        rescued,
        gemma,
    })
}

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
/// 2. Gemma 4 native calls: `call:todo{action:<|"|>update<|"|>,index:1}`, see
///    [`parse_gemma_native`];
/// 3. placeholder-quoted single argument: `finish(summary:<|"|>...<|"|>)` or the brace form;
/// 4. a whole-text function expression: `finish(summary="0")`, see [`parse_func_call_expr`].
pub fn try_parse_tool_calls(content: &str) -> Option<Vec<LeakedCall>> {
    parse_shapes(content).map(|(calls, _)| calls)
}

/// [`try_parse_tool_calls`], also saying whether the Gemma native shape matched.
fn parse_shapes(content: &str) -> Option<(Vec<LeakedCall>, bool)> {
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
                return Some((calls, false));
            }
        }
    }
    if let Some(calls) = parse_gemma_native(content) {
        return Some((calls, true));
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
        return Some((calls, false));
    }
    parse_func_call_expr(&c).map(|calls| (calls, false))
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

/// Gemma 4's string delimiter and the control tokens that wrap its calls and thoughts.
const GEMMA_QUOTE: &str = "<|\"|>";
const GEMMA_MARKERS: [&str; 5] = [
    GEMMA_QUOTE,
    "<|tool_call>",
    "<tool_call|>",
    "<|channel>",
    "<channel|>",
];

static GEMMA_THOUGHT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<\|channel>.*?<channel\|>").expect("regex"));
static GEMMA_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^A-Za-z0-9_])call:([A-Za-z_][A-Za-z0-9_.\-]*)\s*\{").expect("regex")
});

/// Gemma 4 native calls leaked into text (bead boubou-5hg): `call:NAME{k:v,…}`, strings
/// in `<|"|>…<|"|>`, bare keys, numbers, booleans, null, arrays and objects. Seen from LM
/// Studio when the model opened its thought channel and dropped the `<|tool_call>` opener:
/// `<|channel>::<|channel>call:todo{action:<|"|>update<|"|>,done:true,index:1}<tool_call|>`.
/// A Gemma marker must be present, so prose that mentions `call:x{}` is left alone; closed
/// thought spans are dropped first; the name must be a known tool or a dotted MCP name;
/// a call whose arguments do not read cleanly to the closing brace is not salvaged.
pub fn parse_gemma_native(content: &str) -> Option<Vec<LeakedCall>> {
    if !GEMMA_MARKERS.iter().any(|m| content.contains(m)) {
        return None;
    }
    let c = GEMMA_THOUGHT.replace_all(content, " ");
    let mut calls = Vec::new();
    for m in GEMMA_CALL.captures_iter(&c) {
        let name = m[1].to_string();
        if !is_known_tool(&name) && !name.contains('.') {
            continue;
        }
        let open = m.get(0).expect("match").end() - 1;
        let mut r = GemmaReader { s: &c, pos: open };
        if let Some(Value::Object(args)) = r.value() {
            calls.push((name, args));
        }
    }
    (!calls.is_empty()).then_some(calls)
}

/// A reader for Gemma 4's call-argument literals.
struct GemmaReader<'a> {
    s: &'a str,
    pos: usize,
}

impl GemmaReader<'_> {
    fn rest(&self) -> &str {
        &self.s[self.pos..]
    }

    fn skip_ws(&mut self) {
        let t = self.rest().trim_start();
        self.pos = self.s.len() - t.len();
    }

    fn eat(&mut self, tok: &str) -> bool {
        self.skip_ws();
        if self.rest().starts_with(tok) {
            self.pos += tok.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Option<Value> {
        self.skip_ws();
        if self.eat("{") {
            let mut obj = Map::new();
            if self.eat("}") {
                return Some(Value::Object(obj));
            }
            loop {
                let k = self.key()?;
                if !self.eat(":") {
                    return None;
                }
                obj.insert(k, self.value()?);
                if self.eat("}") {
                    return Some(Value::Object(obj));
                }
                if !self.eat(",") {
                    return None;
                }
            }
        }
        if self.eat("[") {
            let mut arr = Vec::new();
            if self.eat("]") {
                return Some(Value::Array(arr));
            }
            loop {
                arr.push(self.value()?);
                if self.eat("]") {
                    return Some(Value::Array(arr));
                }
                if !self.eat(",") {
                    return None;
                }
            }
        }
        if let Some(st) = self.string() {
            return Some(Value::String(st));
        }
        let word: String = self
            .rest()
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '+' | '.' | '_'))
            .collect();
        let v = match word.as_str() {
            "" => return None,
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "null" => Value::Null,
            w => match w.parse::<i64>() {
                Ok(i) => Value::from(i),
                Err(_) => Value::from(w.parse::<f64>().ok()?),
            },
        };
        self.pos += word.len();
        Some(v)
    }

    /// `<|"|>…<|"|>` or a JSON `"…"` string.
    fn string(&mut self) -> Option<String> {
        self.skip_ws();
        if let Some(body) = self.rest().strip_prefix(GEMMA_QUOTE) {
            let end = body.find(GEMMA_QUOTE)?;
            let st = body[..end].to_string();
            self.pos += GEMMA_QUOTE.len() * 2 + end;
            return Some(st);
        }
        if self.rest().starts_with('"') {
            let mut de = serde_json::Deserializer::from_str(self.rest()).into_iter::<String>();
            let st = de.next()?.ok()?;
            self.pos += de.byte_offset();
            return Some(st);
        }
        None
    }

    fn key(&mut self) -> Option<String> {
        if let Some(k) = self.string() {
            return Some(k);
        }
        let k: String = self
            .rest()
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        if k.is_empty() || k.starts_with(|ch: char| ch.is_ascii_digit()) {
            return None;
        }
        self.pos += k.len();
        Some(k)
    }
}
