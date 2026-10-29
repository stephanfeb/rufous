//! Guards: the pure-logic heuristics Mingbird accumulated from real failure post-mortems.
//! Each submodule names the Python function it ports. None of them touch the network.

pub mod artifacts;
pub mod budget;
pub mod edit;
pub mod idle;
pub mod integrity;
pub mod leak;
pub mod qa;

use std::sync::LazyLock;

use regex::Regex;

use crate::text;

/// `_model_params_b`: parameter count in billions from a model tag.
/// `gemma4:e2b` → 2.0, `qwen3.5:4b` → 4.0. `None` when unparseable (treat as large).
pub fn model_params_b(model: &str) -> Option<f64> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\d+(?:\.\d+)?)b\b").expect("regex"));
    RE.captures(&model.to_lowercase())
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

/// Models at or below this many billion parameters get the small-model protocol.
pub const SMALL_MODEL_MAX_B: f64 = 4.0;

pub fn is_small_model(model: &str) -> bool {
    model_params_b(model).unwrap_or(99.0) <= SMALL_MODEL_MAX_B
}

/// `_semantically_empty`: content with think-tag shells removed has no real text.
/// A reply that *starts* with `<think>` and never closes it is truncation debris and
/// counts as empty; text before a `<think>` is real content.
pub fn semantically_empty(text: &str) -> bool {
    static BLOCK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<think>.*?</think>|</?think>").expect("regex"));
    static CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</think>").expect("regex"));
    let s = text.trim_start();
    if s.to_lowercase().starts_with("<think>") && !CLOSE.is_match(s) {
        return true;
    }
    BLOCK.replace_all(text, "").trim().is_empty()
}

/// `_estimate_tokens`: ~2 tokens per CJK char, ~3 chars per token otherwise. Overestimates
/// on purpose: compacting early is cheap, overflowing the context is fatal.
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let cjk = text
        .chars()
        .filter(|&c| {
            ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c)
        })
        .count();
    let other = text.chars().count() - cjk;
    cjk * 2 + other / 3 + 1
}

/// `_format_bash_out`: the receipt for a command. With `small_model`, long output keeps
/// only the tail (summaries and errors live there) plus an escape hatch to the full text.
pub fn format_bash_out(returncode: i32, stdout: &str, stderr: &str, small_model: bool) -> String {
    let mut out = format!(
        "[exit {returncode}]\nSTDOUT:\n{}\nSTDERR:\n{}",
        text::tail(stdout, 5000),
        text::tail(stderr, 2000)
    );
    if returncode == 9009 && stdout.trim().is_empty() && stderr.trim().is_empty() {
        out.push_str(
            "\n[hint: exit 9009 = this machine cannot find the command (no output at all). Windows has no python3; run Python as python. Bash-only syntax such as heredocs/cat/grep does not work here either; write files with the create_file tool.]",
        );
    }
    if small_model && (text::clen(stdout) > 2500 || text::clen(stderr) > 1200) {
        out = format!(
            "[exit {returncode}]\nSTDOUT (tail):\n{}\nSTDERR (tail):\n{}\n[very long command output was trimmed to its tail (summary/errors) to protect a tight context budget. For the full output run `command > out.txt 2>&1` with run_bash and read it in parts with read_file.]",
            text::tail(stdout, 2500),
            text::tail(stderr, 1200)
        );
    }
    out
}
