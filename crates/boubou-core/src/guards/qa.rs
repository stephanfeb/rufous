//! `_is_qa` / `_has_task_progress` / `_task_original_text`: telling a question or
//! greeting from a task, and finding the real user instruction among harness notes.

use std::sync::LazyLock;

use regex::Regex;

use crate::ollama::{Message, Role};
use crate::text;
use crate::tools::is_mcp_tool;

/// Marker prefix of the parallel-dispatch receipt message.
pub const DISPATCH_NOTE_MARK: &str = "[parallel-dispatch]";

/// Every user-role message the harness injects starts with one of these, so routing,
/// Q&A detection and task re-injection can skip them. Invariant: any new injected
/// message must start with a prefix from this list.
pub const HARNESS_NOTE_PREFIXES: [&str; 8] = [
    "⚠️",
    "Continue:",
    "[已拦截",
    "回答已经足够",
    "⏱",
    "提示:",
    "继续之前的对话",
    DISPATCH_NOTE_MARK,
];

pub fn is_harness_note(text: &str) -> bool {
    HARNESS_NOTE_PREFIXES.iter().any(|p| text.starts_with(p))
        || text.contains("[先前上下文摘要]")
        || text.contains("上下文已压缩")
}

const TASK_HINTS: &[&str] = &[
    "写",
    "建",
    "改",
    "创建",
    "修改",
    "删",
    "删除",
    "运行",
    "执行",
    "实现",
    "编写",
    "重构",
    "修复",
    "生成",
    "统计",
    "翻译",
    "总结",
    "对比",
    "测试",
    "调试",
    "安装",
    "下载",
    "部署",
    "搭建",
    "配置",
    "启动",
    "停止",
    "整理",
    "转换",
    "爬取",
    "优化",
    "检查",
    "分析",
    "设计",
    "代码",
    "程序",
    "脚本",
    "帮我做",
    "write ",
    "create ",
    "build ",
    "implement",
    "fix ",
    "debug",
    "analyze",
    "analyse",
    "clean ",
    "optimize",
    "optimise",
    "refactor",
    "generate",
    "design ",
    "audit ",
    "review ",
    "compute",
    "convert",
    "export",
    "import ",
    "plot ",
    "profile ",
    "implement ",
    "produce ",
    "research ",
    "search for",
    "find all",
    "list all",
    "step 1",
    "required steps",
    "plan at least",
    "todo(action",
];

const QA_HINTS: &[&str] = &[
    "什么",
    "怎么",
    "为什么",
    "如何",
    "解释",
    "说明",
    "介绍",
    "区别",
    "原理",
    "能否",
    "可以吗",
    "能不能",
    "吗",
    "呢",
    "?",
    "？",
    "你好",
    "hi",
    "hello",
    "在吗",
    "谢谢",
    "再见",
    "早安",
    "晚安",
    "你是谁",
    "what is ",
    "what are ",
    "how do i",
    "how does ",
    "why is ",
    "why does ",
    "can you explain",
    "tell me about",
    "who is ",
    "when was ",
];

/// Non-ASCII hints match as substrings; ASCII hints need word boundaries so `hi` does
/// not hit `this` and `fix` does not hit `prefix`.
fn hint_hit(hint: &str, low: &str) -> bool {
    static CACHE: LazyLock<std::collections::HashMap<&'static str, Regex>> = LazyLock::new(|| {
        TASK_HINTS
            .iter()
            .chain(QA_HINTS.iter())
            .filter(|h| h.is_ascii())
            .map(|h| {
                let re = Regex::new(&format!(r"\b{}\b", regex::escape(h.trim()))).expect("regex");
                (*h, re)
            })
            .collect()
    });
    if !hint.is_ascii() {
        return low.contains(hint);
    }
    CACHE.get(hint).is_some_and(|re| re.is_match(low))
}

/// `_has_task_progress`: has this conversation already done real work (a mutating or
/// MCP tool call)? Read-only tools do not count: they appear in Q&A too.
pub fn has_task_progress(messages: &[Message]) -> bool {
    messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| m.tool_calls.iter())
        .any(|tc| {
            let n = tc.function.name.as_str();
            matches!(
                n,
                "create_file" | "edit_file" | "append_file" | "delete_file" | "run_bash"
            ) || is_mcp_tool(n)
        })
}

/// `_is_qa`: is the latest real user message a question/greeting rather than a task?
/// Task words win, then question words, then "short and no action verb" is Q&A unless
/// the conversation already has progress (then it is a follow-up instruction).
pub fn is_qa(messages: &[Message], has_progress: bool) -> bool {
    for m in messages.iter().rev() {
        if m.role != Role::User {
            continue;
        }
        let t = m.content.trim();
        if t.is_empty() || is_harness_note(t) {
            continue;
        }
        let low = t.to_lowercase();
        if TASK_HINTS.iter().any(|h| hint_hit(h, &low)) {
            return false;
        }
        if QA_HINTS.iter().any(|h| hint_hit(h, &low)) {
            return true;
        }
        if text::clen(t) <= 40 {
            return !has_progress;
        }
        return false;
    }
    false
}

/// `_task_original_text`: the instruction this delivery answers = the most recent real
/// user input, skipping harness notes and compaction summaries. Long texts keep head and
/// tail halves with an elision marker.
pub fn task_original_text(messages: &[Message], max_chars: usize) -> String {
    for m in messages.iter().rev() {
        if m.role != Role::User {
            continue;
        }
        let t = m.content.trim();
        if t.is_empty() || is_harness_note(t) {
            continue;
        }
        if text::clen(t) <= max_chars {
            return t.to_string();
        }
        let half = max_chars / 2;
        return format!(
            "{}\n...(中略)...\n{}",
            text::head(t, half),
            text::tail(t, half)
        );
    }
    String::new()
}

/// `_task_routing_text`: latest real user input for category routing (no truncation).
pub fn task_routing_text(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| {
            m.role == Role::User
                && !HARNESS_NOTE_PREFIXES
                    .iter()
                    .any(|p| m.content.starts_with(p))
        })
        .map(|m| m.content.clone())
        .unwrap_or_default()
}
