//! Guards: the pure-logic heuristics Mingbird accumulated from real failure post-mortems.
//! Each submodule names the Python function it ports. None of them touch the network.

pub mod artifacts;
pub mod budget;
pub mod edit;
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
            "\n[提示: exit 9009 = 本机找不到这个命令(无任何输出)。Windows 上没有 python3,运行 Python 请用 python;heredoc/cat/grep 等 bash 专用语法在此也不可用,写文件请用 create_file 工具。]",
        );
    }
    if small_model && (text::clen(stdout) > 2500 || text::clen(stderr) > 1200) {
        out = format!(
            "[exit {returncode}]\nSTDOUT(末尾):\n{}\nSTDERR(末尾):\n{}\n[超长命令输出已精简(只保留末尾的摘要/报错;上下文预算紧张时的保护)。需要完整输出时:run_bash 执行 `命令 > out.txt 2>&1` 再 read_file 分段读。]",
            text::tail(stdout, 2500),
            text::tail(stderr, 1200)
        );
    }
    out
}
