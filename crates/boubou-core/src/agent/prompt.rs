//! System prompts (`SYSTEM`, `CHAT_SYSTEM`, `system_prompt()`) and the platform block
//! (`platform_profile.prompt_block`). Text is Mingbird's, with the dynamic lines added at
//! run time so the factory prefill stays byte-stable.

use std::path::Path;

use crate::text::read_text;

const SYSTEM_BASE: &str = r#"You are an autonomous AI agent in a workspace directory.
JUDGE INTENT FIRST — 先判断用户意图,再决定怎么做:
- 闲聊/寒暄/介绍自己/一般知识问答(如"你好""你是谁""你能做什么""今天天气如何"):
  【直接用文字回答】,绝不调用任何工具,绝不建 todo,绝不创建文件或跑命令,然后调用 finish。
- 真正要干活的请求(写代码、改/建文件、分析数据、查资料、跑命令、修 bug):
  先调用 todo(action=create) 建编号计划,再逐步用工具执行,边做边更新 todo。
RULES:
- 别把"你好/你是干嘛的"当成任务去演示工具——直接回答即可。
- Actually DO the work with tools when it's a real task (write code, run it, fix errors until verified). Never just describe.
- Need web/memory/MCP/append/delete/search? Call enable_tools first, they then become available.
- {PLATFORM_LINE}
- 主目录:{USER_HOME};保存到工作区外的位置(如桌面 {USER_HOME}/Desktop)直接用真实绝对路径(越界确认会放行),勿造假目录。
- Be concise in text; put large content in tool arguments. Call finish only when answered or fully verified done."#;

const PLATFORM_LINE_WINDOWS: &str = "Work only in the current directory (Windows). Never cd to absolute paths like /workspace — commands already run here; use relative paths.";
const PLATFORM_LINE_POSIX: &str = "Work only in the current directory. Never cd outside it and never invent paths like C:\\ — commands already run here; use relative paths.";

/// `CHAT_SYSTEM`: the Q&A prefill. Boubou keeps Mingbird's self-introduction so the
/// behaviour benchmarks carry over; the product name is the only edit.
pub const CHAT_SYSTEM: &str = "你是布布(Boubou),本地 AI 助手,正在和用户对话。
- 直接、自然、简洁地回答用户的问题。像真人聊天。
- 普通寒暄/问句,直接回答即可,一句话或几句话都行,不要长篇大论。
- 只有需要查资料/读文件/搜索时才用工具,其余情况纯粹用文字回答。
- 回答完就结束,不要重复,不要自我展示,不要说\"我能帮你做什么\"这类套话。";

/// The static task prefill with the home directory and platform line filled in.
pub fn system_base(lang_en: bool) -> String {
    let home = dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "~".to_string());
    let line = if cfg!(windows) {
        PLATFORM_LINE_WINDOWS
    } else {
        PLATFORM_LINE_POSIX
    };
    let mut s = SYSTEM_BASE
        .replace("{PLATFORM_LINE}", line)
        .replace("{USER_HOME}", &home);
    if lang_en {
        s.push_str("\nAlways respond in English — user-facing text, todo items, and summaries.");
    }
    s
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::Windows => "Windows",
            Platform::MacOs => "macOS",
            Platform::Linux => "Linux",
        }
    }

    pub fn shell(self) -> &'static str {
        match self {
            Platform::Windows => "cmd.exe",
            _ => "/bin/sh",
        }
    }

    pub fn path_sep(self) -> &'static str {
        match self {
            Platform::Windows => "\\",
            _ => "/",
        }
    }

    fn command_map(self) -> &'static str {
        match self {
            Platform::Windows => "列目录=dir / 看文件=type / 搜索文本=findstr / 复制=copy",
            _ => "列目录=ls / 看文件=cat / 搜索文本=grep / 复制=cp",
        }
    }
}

/// `platform_profile.prompt_block`. `full=false` is the one-line Q&A variant.
pub fn platform_block(full: bool, unattended: bool) -> String {
    let p = Platform::current();
    if !full {
        return format!("平台:{},路径分隔符 {}", p.label(), p.path_sep());
    }
    let guard = if unattended {
        "- 命令在沙箱外执行;递归删除/卸载/改系统环境在无人值守模式下会被直接拒绝,不要反复重试,改用工作目录内的安全路径"
    } else {
        "- 命令在沙箱外执行;递归删除/卸载/改系统环境会触发用户确认——这是正常防护不是故障,继续干活即可"
    };
    format!(
        "平台:{} · 命令 shell={}(支持 && 串联执行)\n- 命令对照:{};路径分隔符 {},含空格的路径加引号\n{}",
        p.label(),
        p.shell(),
        p.command_map(),
        p.path_sep(),
        guard
    )
}

/// `_CHILD_SYSTEM`: the minimal prompt of a dispatched child (local cost of a short
/// small-model session; the parent's prefill is untouched).
pub const CHILD_SYSTEM: &str = "You are a sub-agent doing one small mechanical file task.
- Work ONLY inside your current directory. Never touch paths outside it.
- Use create_file to write the result, list_dir/read_file to inspect, todo to plan.
- No commands, no network, no deleting, no editing other files.
- When the output file exists and is correct, call finish(summary=files you created).";

/// The child prompt plus the dynamic workdir line. No platform block: a child has no
/// command tools, so the shell notes would only cost tokens.
pub fn child_system(workdir: Option<&Path>) -> String {
    let mut s = CHILD_SYSTEM.to_string();
    if let Some(wd) = workdir {
        s.push_str(&format!(
            "\n当前工作目录(所有命令与相对路径都在它之下运行):{}",
            wd.display()
        ));
    }
    s
}

/// Options for [`system_prompt`].
#[derive(Debug, Clone, Default)]
pub struct PromptOptions<'a> {
    /// Absolute workdir, injected as the dynamic "当前工作目录" line.
    pub workdir: Option<&'a Path>,
    /// `AGENT_SYSTEM_FILE`: replaces the static prefill when it exists.
    pub system_file: Option<&'a Path>,
    pub unattended: bool,
    pub lang_en: bool,
}

/// `system_prompt()`: static prefill + dynamic workdir line + platform block.
pub fn system_prompt(opts: &PromptOptions<'_>) -> String {
    let mut base = match opts.system_file {
        Some(p) if p.exists() => read_text(p).unwrap_or_else(|_| system_base(opts.lang_en)),
        _ => system_base(opts.lang_en),
    };
    if let Some(wd) = opts.workdir {
        base.push_str(&format!(
            "\n当前工作目录(所有命令与相对路径都在它之下运行):{}",
            wd.display()
        ));
    }
    base.push('\n');
    base.push_str(&platform_block(true, opts.unattended));
    base
}

/// The Q&A system prompt for this workdir (`CHAT_SYSTEM` + workdir + one-line platform).
pub fn chat_system(workdir: Option<&Path>, lang_en: bool) -> String {
    let mut s = CHAT_SYSTEM.to_string();
    if lang_en {
        s.push_str("\nAlways respond in English.");
    }
    if let Some(wd) = workdir {
        s.push_str(&format!("\n当前工作目录:{}", wd.display()));
    }
    s.push('\n');
    s.push_str(&platform_block(false, false));
    s
}
