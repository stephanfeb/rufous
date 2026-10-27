//! `agent_loop`: the harness around the model. Every guard here exists because a real
//! model session failed in a specific way; the Chinese message strings are Mingbird's and
//! are pinned by the ported tests, so change them together with the tests.
//!
//! Shape: up to `max_iterations` turns. Each turn: flush transcript → dedupe tail →
//! assemble the tool face → pre-call token estimate (compaction ladder) → model call
//! (with the 4xx/5xx triage) → post-call context ladder → either run the tool calls through
//! the guard stack, or handle a text-only turn (empty-turn ladder, repeat detection,
//! Q&A wrap-up). `finish` runs the gate sequence and returns.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::chat::{ChatBackend, StreamPiece};
use super::face::{self, ToolFace, MUTATE, PRODUCTIVE, RESEARCH_ALL};
use super::history::{
    attach_pending_images, compact_history, dedupe_trailing_assistant, estimate_messages_tokens,
    similar, workdir_files_text,
};
use super::prompt;
use super::session::{self, Transcript};
use super::testguard::{self, TestVerdict};
use super::{AgentEvent, RunParams};
use crate::exec::Executor;
use crate::guards::budget::{self, BudgetMark};
use crate::guards::edit::EditConvergenceGuard;
use crate::guards::{self, artifacts, leak, qa};
use crate::ollama::{FunctionCall, Message, Role, ToolCall};
use crate::todo::{format_todo_lines, load_todo, load_todo_str, todo_progress};
use crate::tools::{is_mcp_tool, resolve_alias};
use crate::{text, Error, Result};

/// `_FAIL_MARKERS`: a tool result that means "the action did not happen". Safety-ring
/// refusals count too, otherwise a model that keeps changing paths never hits the ladder.
pub const FAIL_MARKERS: [&str; 27] = [
    "[edit failed",
    "[tool error",
    "[not found",
    "[web_search error",
    "[web_fetch error",
    "[mcp_call error",
    "not found in",
    "error:",
    "cannot find the path",
    "no such file",
    "is not recognized",
    "command not found",
    "not recognized as",
    "access is denied",
    "did not match",
    "fatal:",
    "unknown option",
    "[安全门拦截",
    "[安全垫",
    "[安全门:",
    "[子agent安全门拦截",
    "[覆盖防护",
    "[edit failed:",
    "[tool error:",
    "[not found:",
    "[安全门",
    "[覆盖防护:",
];

/// `_is_tool_error`: marker scan over the first 200 chars, case-insensitive.
pub fn is_tool_error(res: &str) -> bool {
    let r = text::head(res, 200).to_lowercase();
    FAIL_MARKERS.iter().any(|m| r.contains(m))
}

const CONTINUE_NOTE: &str =
    "Continue: keep making real progress with tools, or call finish only when fully verified/answered.";

const CTX_OVERFLOW_KEYS: [&str; 7] = [
    "prompt is too long",
    "too long",
    "exceeds context",
    "context length",
    "input is too long",
    "超过上下文",
    "context window",
];

/// Executes tool calls for the loop. [`Executor`] is the real one; tests script results.
pub trait ToolRunner {
    fn run_tool(&mut self, name: &str, args: &Value) -> String;
    /// `_SLIM_OUTPUT_MODE`: trim long command output (set once per run).
    fn set_slim_output(&mut self, _on: bool) {}
}

impl ToolRunner for Executor {
    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        Executor::run_tool(self, name, args)
    }
    fn set_slim_output(&mut self, on: bool) {
        self.slim_output = on;
    }
}

/// `_ws_note`: the refusal texts always name the real absolute workdir, because small
/// models otherwise guess `/workspace` or the home directory and never find out.
pub fn ws_note(workdir: &Path) -> String {
    let abs = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    format!(
        "你的工作目录(绝对路径)是 {};所有产物必须写在这个目录内,优先用相对路径(如 create_file(path=\"options.md\"))。",
        abs.display()
    )
}

/// `_plan_note`: current plan as text for compaction re-injection; none in Q&A / no plan.
fn plan_note(qa: bool, workdir: &Path) -> Option<String> {
    if qa {
        return None;
    }
    let t = load_todo(workdir);
    if t.is_empty() {
        None
    } else {
        Some(format_todo_lines(&t))
    }
}

/// Tool-call arguments as an object: objects pass, JSON strings are parsed, anything else
/// becomes `{}` (Mingbird `json.loads` fallback).
fn args_map(v: &Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m.clone(),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        _ => Map::new(),
    }
}

fn first_user_text(messages: &[Message]) -> String {
    messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

type Sink = Box<dyn FnMut(AgentEvent) + Send>;

/// The loop plus everything it owns between runs: tool face, transcript cursor, backend.
pub struct Agent<B, T> {
    pub params: RunParams,
    pub backend: B,
    pub tools: T,
    pub face: ToolFace,
    pub transcript: Transcript,
    /// `~/.ollama_agent/sessions` unless relocated.
    pub sessions_dir: PathBuf,
    sink: Sink,
}

impl<B, T> std::fmt::Debug for Agent<B, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("params", &self.params)
            .field("face", &self.face)
            .field("sessions_dir", &self.sessions_dir)
            .finish()
    }
}

impl Agent<super::OllamaBackend, Executor> {
    /// The production wiring: streaming Ollama backend, real executor in the workdir.
    pub fn standard(params: RunParams, host: impl Into<String>, offline: bool) -> Result<Self> {
        let backend = super::OllamaBackend::new(host, &params)?;
        let mut tools = Executor::new(params.workdir.clone());
        tools.slim_output = params.slim_output;
        let mut agent = Agent::new(params, backend, tools);
        agent.face.offline = offline;
        Ok(agent)
    }
}

impl<B: ChatBackend, T: ToolRunner> Agent<B, T> {
    pub fn new(params: RunParams, backend: B, tools: T) -> Self {
        let home = crate::config::agent_home();
        Self {
            params,
            backend,
            tools,
            face: ToolFace::new(),
            transcript: Transcript::new(),
            sessions_dir: session::sessions_dir(&home),
            sink: Box::new(|_| {}),
        }
    }

    /// Receive every [`AgentEvent`] (streamed text, tool receipts, harness log lines).
    pub fn with_sink(mut self, sink: impl FnMut(AgentEvent) + Send + 'static) -> Self {
        self.sink = Box::new(sink);
        self
    }

    pub fn set_sink(&mut self, sink: impl FnMut(AgentEvent) + Send + 'static) {
        self.sink = Box::new(sink);
    }

    /// Build the opening message list for a fresh task.
    pub fn task_messages(&self, task: &str) -> Vec<Message> {
        let opts = prompt::PromptOptions {
            workdir: Some(&self.params.workdir),
            system_file: self.params.system_file.as_deref(),
            unattended: self.params.unattended,
            lang_en: self.params.lang_en,
        };
        vec![
            Message::system(prompt::system_prompt(&opts)),
            Message::user(task),
        ]
    }

    /// `agent_loop(model, messages, workdir, session, budget_sec)`. Returns the final
    /// message list; fatal API errors and the exhausted empty-turn ladder return `Err`
    /// after the session has been saved, so a rerun resumes from the checkpoint.
    pub fn run(
        &mut self,
        mut messages: Vec<Message>,
        session: Option<&str>,
        budget_sec: Option<f64>,
    ) -> Result<Vec<Message>> {
        let Agent {
            params,
            backend,
            tools,
            face,
            transcript,
            sessions_dir,
            sink,
        } = self;
        let model = params.model.as_str();
        let workdir = params.workdir.as_path();
        let ctx_budget = f64::from(params.num_ctx);
        let fg_off = params.ablated("finish_gate") || params.ablated("finish_gate_text_only");

        macro_rules! log {
            ($($arg:tt)*) => { sink(AgentEvent::Log(format!($($arg)*))) };
        }
        macro_rules! save {
            () => {
                session::save_session(sessions_dir, session, &messages, Some(workdir), transcript);
                if session.is_some() {
                    log!("[session saved: {}]", session.unwrap_or_default());
                }
            };
        }

        // Per-run counters (names follow the Python locals).
        let mut fails = 0usize;
        let mut fail_count: HashMap<String, usize> = HashMap::new();
        let mut redirect_warns = 0;
        let mut last_tool: Option<String> = None;
        let mut last_sig: Option<(String, String)> = None;
        let mut same_sig_streak = 0;
        let mut last_sig_now: Option<(String, String)> = None;
        let mut dup_warns = 0;
        let mut disabled_recall: HashMap<String, usize> = HashMap::new();
        let mut tool_streak = 0;
        let mut streak_warns = 0;
        let mut research_streak = 0;
        let mut research_warns = 0;
        let mut research_seen: BTreeSet<String> = BTreeSet::new();
        let mut todo_streak = 0;
        let mut productive_used = false;
        let mut fake_finish_warns = 0;
        let mut finish_claim_warns = 0;
        let mut finish_reread_used = false;
        let mut plan_gate_warns = 0;
        let mut bash_teach_warns = 0;
        let mut empty_turns = 0;
        let mut len_truncated = false;
        let mut empty_resets = 0;
        let mut empty_total = 0;
        let mut test_guard_warns = 0;
        let mut casual_warns = 0;
        let mut casual_force = false;
        let mut last_text = String::new();
        let mut repeat_count = 0;
        let mut compact_lvl: u8 = 0;
        let mut todo_stale = 0;
        let mut todo_nudge_warns = 0;
        let mut todo_gate_warns = 0;
        const TODO_STALE_NUDGE: usize = 4;

        // Q&A or task? A session with real progress treats short follow-ups as task steps.
        let qa = qa::is_qa(&messages, qa::has_task_progress(&messages));
        let cats: Option<BTreeSet<String>> = if qa {
            let cs = prompt::chat_system(Some(workdir), params.lang_en);
            let mut m = vec![Message::system(cs)];
            m.extend(messages.into_iter().filter(|m| m.role != Role::System));
            messages = m;
            None
        } else {
            Some(face::route_categories(&qa::task_routing_text(&messages)))
        };
        let small_model = !qa && guards::is_small_model(model);
        tools.set_slim_output(params.slim_output);
        if small_model {
            // ≤4B: edit_file's exact-match protocol is the top failure mode; rewrite instead.
            face.disabled.insert("edit_file".to_string());
        }
        let mut edit_guard = EditConvergenceGuard::default();

        // Time budget: explicit parameter beats the env fallback; resume continues the clock.
        let budget_total = budget_sec
            .or(params.budget_env_sec)
            .filter(|b| b.is_finite())
            .unwrap_or(0.0)
            .max(0.0);
        let t0 = Instant::now();
        let prev_elapsed = if budget_total > 0.0 {
            budget::load_elapsed(workdir)
        } else {
            0.0
        };
        if prev_elapsed > 0.0 {
            log!(
                "[budget: 续接已用时 {:.0}min / 预算 {:.0}min]",
                prev_elapsed / 60.0,
                budget_total / 60.0
            );
        }
        let mut budget_fired: BTreeSet<BudgetMark> = BTreeSet::new();

        let mut exhausted = true;
        for i in 0..params.max_iterations {
            transcript.flush(sessions_dir, session, &messages);
            if casual_force {
                log!("[收尾] 本轮已足够,强制结束(防问答重复/死循环)");
                messages.push(Message::tool_output("[TASK_COMPLETE] 已回答,结束本轮"));
                exhausted = false;
                break;
            }
            dedupe_trailing_assistant(&mut messages);
            let ct: Vec<Value> = match (&cats, qa) {
                (_, true) => face.chat_tool_defs(),
                (Some(c), false) if !params.ablated("flat_prefill") => face.tools_for_categories(c),
                _ => face.active_defs(),
            };
            // Pre-call estimate: compact before the server has to refuse.
            let est = estimate_messages_tokens(&messages, &ct) as f64;
            let pre = if est > ctx_budget * 0.90 && compact_lvl < 3 {
                Some(3)
            } else if est > ctx_budget * 0.75 && compact_lvl < 2 {
                Some(2)
            } else if est > ctx_budget * 0.60 && compact_lvl < 1 {
                Some(1)
            } else {
                None
            };
            if let Some(level) = pre {
                let (todo, files) = if level >= 2 {
                    (
                        plan_note(qa, workdir),
                        Some(workdir_files_text(workdir, 30)),
                    )
                } else {
                    (None, None)
                };
                messages = compact(
                    backend, model, messages, level, todo, files, transcript, sink,
                );
                compact_lvl = level;
                log!("[{i}] 事前估算 {est:.0} token 超限,已压缩(L{level})");
            }
            let n_img = attach_pending_images(&mut messages, workdir);
            if n_img > 0 {
                log!("[{i}] 🖼 已附载 {n_img} 张图片(多模态输入)");
            }

            let r = match backend.chat(model, &messages, &ct, &mut |p| match p {
                StreamPiece::Token(t) => sink(AgentEvent::Text(t.to_string())),
                StreamPiece::Think(t) => sink(AgentEvent::Thinking(t.to_string())),
            }) {
                Ok(r) => {
                    fails = 0;
                    r
                }
                Err(e) => {
                    let low = e.lower_text();
                    if e.is_4xx() {
                        // Structural 400 (consecutive assistant turns): clean and retry.
                        if low.contains("assistant messages at the end")
                            || low.contains("consecutive")
                        {
                            let before = messages.len();
                            dedupe_trailing_assistant(&mut messages);
                            if messages.len() != before {
                                log!("[{i}] 修复对话结构(连续 assistant),已清理重试");
                                fails += 1;
                                if fails >= 6 {
                                    save!();
                                    return Err(Error::Ollama(e.to_string()));
                                }
                                std::thread::sleep(Duration::from_secs(1));
                                continue;
                            }
                        }
                        log!("[{i}] API 4xx 错误(重试无意义,已停止): {e}");
                        save!();
                        return Err(Error::Ollama(e.to_string()));
                    }
                    if CTX_OVERFLOW_KEYS.iter().any(|k| low.contains(k)) {
                        log!("[{i}] ⚠️ 上下文超限错误 → 强制 L3 压缩后重试");
                        let todo = plan_note(qa, workdir);
                        let files = Some(workdir_files_text(workdir, 30));
                        messages =
                            compact(backend, model, messages, 3, todo, files, transcript, sink);
                        compact_lvl = 3;
                        fails = 0;
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                    fails += 1;
                    if e.is_500() && fails >= 4 {
                        log!("[{i}] 连续 {fails} 次 500(Mingbird 在此自动重启 ollama;Boubou 尚未移植,继续压缩重试)");
                    }
                    if e.is_500() && fails >= 2 {
                        if compact_lvl < 3 {
                            let lvl = (compact_lvl + 1).min(3);
                            compact_lvl = lvl;
                            let todo = plan_note(qa, workdir);
                            let files = Some(workdir_files_text(workdir, 30));
                            messages = compact(
                                backend, model, messages, lvl, todo, files, transcript, sink,
                            );
                            log!("[{i}] 500 错误,已降级压缩上下文(L{lvl})再重试");
                        }
                        let last_user_is_note = messages
                            .iter()
                            .rev()
                            .find(|m| m.role == Role::User)
                            .is_some_and(|m| m.content.starts_with("⚠️"));
                        if !last_user_is_note {
                            messages.push(Message::user(
                                "⚠️ 系统提示:上一请求遇到服务错误。请基于当前已有进度继续完成任务,可以从换一种写法或先做下一步开始。",
                            ));
                            log!("[{i}] 500 错误,已注入继续提示(打破死循环路径)");
                        }
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                    let wait = (3u64 << (fails - 1).min(10)).min(60);
                    log!("[{i}] API error: {e} (retry {fails}, wait {wait}s)");
                    std::thread::sleep(Duration::from_secs(wait));
                    if fails >= 8 {
                        log!("===== FAILED: ollama 连续错误(含重启后仍失败),已停止。修复后勾选'续跑'可从中断处继续 =====");
                        save!();
                        return Err(Error::Ollama(e.to_string()));
                    }
                    continue;
                }
            };

            // Post-call context ladder from the real prompt token count.
            if let Some(pt) = r.prompt_eval_count.filter(|&p| p > 0) {
                let pct = pt * 100 / u64::from(params.num_ctx.max(1));
                log!("[ctx: {pt}/{} = {pct}%]", params.num_ctx);
                sink(AgentEvent::ContextUsage {
                    used: pt,
                    limit: params.num_ctx,
                });
                let ptf = pt as f64;
                if ptf < ctx_budget * 0.60 {
                    compact_lvl = 0; // context fell back: re-arm the ladder
                }
                let post = if ptf > ctx_budget * 0.90 && compact_lvl < 3 {
                    Some(3)
                } else if ptf > ctx_budget * 0.75 && compact_lvl < 2 {
                    Some(2)
                } else if ptf > ctx_budget * 0.60 && compact_lvl < 1 {
                    Some(1)
                } else {
                    None
                };
                if let Some(level) = post {
                    let todo = if level >= 2 {
                        plan_note(qa, workdir)
                    } else {
                        None
                    };
                    messages = compact(
                        backend, model, messages, level, todo, None, transcript, sink,
                    );
                    compact_lvl = level;
                }
            }

            let content = r.message.content.clone();
            let mut tcs: Vec<ToolCall> = r.message.tool_calls.clone();
            if r.done_reason.as_deref() == Some("length")
                && guards::semantically_empty(&content)
                && tcs.is_empty()
            {
                log!(
                    "[{i}] ⚠️ done_reason=length 且无正文无调用(eval={:?})——疑似生成预算被烧尽",
                    r.eval_count
                );
                len_truncated = true;
            }
            if let Some(th) = r.message.thinking.as_deref().filter(|t| !t.is_empty()) {
                if tcs.is_empty() && guards::semantically_empty(&content) {
                    log!(
                        "[{i}] 💭 thinking {} 字符、正文为空。摘录: {}",
                        text::clen(th),
                        text::head(th, 600)
                    );
                }
            }
            if tcs.is_empty() && !qa {
                // Format leak salvage: the model wrote the call as text.
                if let Some(salvaged) = leak::try_parse_tool_calls(&content) {
                    let names: Vec<&str> = salvaged.iter().map(|(n, _)| n.as_str()).collect();
                    log!("[{i}] ⚡ 抢救到文本工具调用: {}", names.join(", "));
                    tcs = salvaged
                        .into_iter()
                        .map(|(name, args)| ToolCall {
                            function: FunctionCall {
                                name,
                                arguments: Value::Object(args),
                            },
                        })
                        .collect();
                }
            }
            for tc in &mut tcs {
                let n = resolve_alias(&tc.function.name);
                if n != tc.function.name {
                    log!("[{i}] ⚡ 别名映射: {} -> {}", tc.function.name, n);
                    tc.function.name = n.to_string();
                }
            }
            let mut turn = Message::assistant(content.clone());
            turn.tool_calls = tcs.clone();
            messages.push(turn);
            session::write_checkpoint(workdir, &messages);

            if tcs.is_empty() {
                // ---- text-only turn ----
                if guards::semantically_empty(&content) {
                    empty_turns += 1;
                    empty_total += 1;
                    if len_truncated && empty_turns == 1 {
                        // Truncation debris: the model is not idle, one shot cannot hold it.
                        len_truncated = false;
                        messages.push(Message::user(format!(
                            "⚠️ 你的上一轮输出触到生成 token 上限({})被截断,没有产生有效工具调用。大内容请分块交付:先用 create_file 写文件骨架或开头部分,再用 append_file 逐段追加(每次几百行以内);或者精简实现。不要试图一次输出完整的大文件。",
                            params.num_predict
                        )));
                        continue;
                    }
                    if empty_turns >= 6 && empty_resets < 3 {
                        empty_resets += 1;
                        empty_turns = 0;
                        let task0 = first_user_text(&messages);
                        let files = workdir_files_text(workdir, 30);
                        let head = messages
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Message::system(""));
                        messages = vec![
                            head,
                            Message::user(format!(
                                "{task0}\n\n⚠️ [系统] 连续多次空输出,上下文已重置。当前进度快照:\n计划:\n{}\n工作目录已有文件:\n{files}\n请从第一个未完成的步骤继续,立即用工具推进,不要输出空文本。",
                                load_todo_str(workdir)
                            )),
                        ];
                        log!("[{i}] ⚠️ 连续空轮×6,硬复位上下文(第 {empty_resets}/3 次,累计空轮 {empty_total})");
                        transcript.mark_compacted();
                        continue;
                    }
                    if empty_turns >= 3 && empty_resets >= 3 {
                        log!("[{i}] 累计 {empty_total} 个空轮且 3 次复位无效,优雅退出");
                        save!();
                        return Err(Error::Aborted(format!(
                            "{empty_total} empty turns after 3 context resets"
                        )));
                    }
                    if empty_turns == 3 {
                        messages.push(Message::user(
                            "⚠️ 你连续输出了空文本。立即调用工具做实际工作:read_file 读文件 / run_bash 运行命令 / create_file 写文件。不要输出空文本。",
                        ));
                        continue;
                    }
                } else {
                    empty_turns = 0;
                    len_truncated = false;
                    if !last_text.is_empty() && similar(&last_text, &content, 0.75) {
                        repeat_count += 1;
                        log!("[{i}] ⚠️ 检测到重复输出({repeat_count})");
                        if repeat_count >= 2 {
                            casual_force = true;
                            messages.push(Message::user(
                                "⚠️ 你重复输出了几乎相同的内容,判定为死循环。立即调用 finish 结束本轮,不要重复。",
                            ));
                        } else {
                            messages.push(Message::user(CONTINUE_NOTE));
                        }
                    } else {
                        last_text = content.clone();
                        repeat_count = 0;
                        if qa {
                            casual_force = true;
                            messages.push(Message::user(
                                "回答已经足够,用户是在问答。立即调用 finish 结束本轮,不要重复输出。",
                            ));
                        } else {
                            messages.push(Message::user(CONTINUE_NOTE));
                        }
                    }
                }
                continue;
            }

            // ---- tool calls ----
            for tc in &tcs {
                let name = tc.function.name.clone();
                let args = args_map(&tc.function.arguments);
                let args_v = Value::Object(args.clone());
                let sig_json = args_v.to_string(); // keys sorted: serde_json Map is a BTreeMap
                let res: String;
                let n = name.as_str();

                if (face.is_disabled(n) || !face.is_active(n)) && !is_mcp_tool(n) {
                    // A disabled/absent tool called again. The refusal itself feeds the
                    // loop: strong note at 3, hard reset at 10.
                    let cnt = disabled_recall.entry(name.clone()).or_insert(0);
                    *cnt += 1;
                    let cnt = *cnt;
                    if cnt >= 10 {
                        let task0 = first_user_text(&messages);
                        let head = messages
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Message::system(""));
                        messages = vec![
                            head,
                            Message::user(format!(
                                "{task0}\n\n⚠️ [系统] 上下文已重置:工具 {name} 已被禁用,再次调用也不会执行,且不会再出现在你的工具列表里。当前进度快照:\n计划:\n{}\n工作目录已有文件:\n{}\n请立即改用 create_file 写产出文件推进任务。",
                                load_todo_str(workdir),
                                workdir_files_text(workdir, 30)
                            )),
                        ];
                        disabled_recall.insert(name.clone(), 0);
                        log!("[{i}] ⚠️ {name} 被禁用后仍被调用 10 次,硬复位上下文");
                        transcript.mark_compacted();
                        continue;
                    }
                    res = if face.is_disabled(n) {
                        format!("[tool {name} 已被禁用(连续重复调用保护)。不要尝试用其他工具绕道读同一内容:直接用 create_file 写产出文件,或已有信息足够就调用 finish。]")
                    } else {
                        format!("[工具 {name} 当前不可用(不在本次装配的工具列表里——可能本次未装配该类别,也可能这个名字并不存在)。不要发明工具名:产出文件用 create_file,改文件用 edit_file/create_file,读文件用 read_file;完整可用列表见你的工具定义。]")
                    };
                    if cnt == 3 {
                        messages.push(Message::user(format!(
                            "⚠️ 工具 {name} 已不可用(被禁用或不在装配列表)且你已多次尝试调用它。再次调用也不会执行,该工具不会再出现在你的工具列表里。现在就换动作:用 create_file 写产出文件推进,需要看已有产物就用 read_file/list_dir。"
                        )));
                    }
                } else if n != "finish"
                    && last_sig.as_ref() == Some(&(name.clone(), sig_json.clone()))
                    && dup_warns < 3
                {
                    // Exactly the same call as last time: the result will not change.
                    dup_warns += 1;
                    let res = format!("[已拦截:你刚调用过完全相同的 {name}(参数一致),结果不会变。请换策略:read_file 看真实情况/换实现方式。]");
                    log!("[{i}] ⚠️ 重复调用拦截: {name}({dup_warns})");
                    sink(AgentEvent::ToolResult {
                        name: name.clone(),
                        args: args_v.clone(),
                        output: res.clone(),
                        blocked: true,
                    });
                    messages.push(Message::tool_output(res));
                    messages.push(Message::user(
                        "⚠️ 上一次调用被判定为重复(与此前完全一致),结果不会变。不要原样重试:先用 read_file/list_dir 确认现状,或换一种实现方式继续推进。",
                    ));
                    continue;
                } else if qa && MUTATE.contains(&n) && casual_warns < 3 {
                    // Q&A guard: a question never needs files written or commands run.
                    casual_warns += 1;
                    if casual_warns >= 2 {
                        casual_force = true;
                    }
                    let res = format!("[已拦截:用户是问答,无需 {name}。请直接用文字回答用户。]");
                    log!("[{i}] ⚠️ 问答守护:拦截 {name}(问答消息,非任务)");
                    sink(AgentEvent::ToolResult {
                        name: name.clone(),
                        args: args_v.clone(),
                        output: res.clone(),
                        blocked: true,
                    });
                    messages.push(Message::tool_output(res));
                    messages.push(Message::user(format!(
                        "⚠️ 提醒(第 {casual_warns} 次):用户是在问答(问句/寒暄),不是要执行任务。你的文字回答已经足够。请【立即调用 finish(summary=你的回答)】结束本轮。不要重复输出同一段文字,不要调用任何工具。"
                    )));
                    continue;
                } else {
                    sink(AgentEvent::ToolCall(tc.clone()));
                    res = tools.run_tool(n, &args_v);
                    last_sig = Some((name.clone(), sig_json.clone()));
                }

                if n == "enable_tools" {
                    if let Some(list) = args.get("tools").and_then(Value::as_array) {
                        for t in list.iter().filter_map(Value::as_str) {
                            face.enable(t);
                        }
                    }
                }
                let mcp_producing = is_mcp_tool(n) && face::is_mcp_producing(n);
                if matches!(n, "create_file" | "edit_file" | "append_file" | "run_bash")
                    || mcp_producing
                {
                    productive_used = true;
                }
                if PRODUCTIVE.contains(&n) || mcp_producing {
                    todo_stale += 1;
                } else if n == "todo" {
                    todo_stale = 0;
                }
                log!(
                    "[{i}|+{}s] ⚙ {name} {} -> {}",
                    t0.elapsed().as_secs(),
                    text::head(&sig_json, 60),
                    text::head(&res, 90)
                );
                sink(AgentEvent::ToolResult {
                    name: name.clone(),
                    args: args_v.clone(),
                    output: res.clone(),
                    blocked: false,
                });

                if n == "finish" {
                    let summary = args
                        .get("summary")
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            Value::Null => String::new(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    // Gate 1: a delivery must exist — file/command, body text, or a
                    // substantive summary. Q&A is exempt (the answer is the chat).
                    if !fg_off
                        && !qa
                        && !productive_used
                        && guards::semantically_empty(&content)
                        && guards::semantically_empty(&summary)
                        && fake_finish_warns < 2
                    {
                        fake_finish_warns += 1;
                        log!("[{i}] ⚠️ 拒绝假 finish:未使用任何产出型工具(create_file/edit_file/run_bash)");
                        messages.push(Message::user(format!(
                            "⚠️ 你的 finish 被拒绝:你还没有做任何实际工作(未创建/修改文件或运行命令)。下一步三选一:\n1. 若最终答案/交付内容已确定:先用 create_file 把它写成文件(任务要求写 answer.txt 时就是: create_file(path=\"answer.txt\", content=\"<你的最终答案>\")),然后重新 finish。\n2. 若工作尚未完成:继续用工具推进(读写文件、运行命令、搜索),完成后再调用 finish。\n3. 若这是分析/讲解类任务、本就不该产出文件:把结论直接作为正文输出给用户(不是放在 summary 里),然后重新 finish。{}",
                            ws_note(workdir)
                        )));
                        messages.push(Message::tool_output(
                            "[finish 被拒绝:未检测到任何交付。按上面三选一行动后再 finish。]",
                        ));
                        continue;
                    }
                    // Gate 2: tests in the workdir must pass (3 refusals, then let through).
                    if !qa
                        && !fg_off
                        && testguard::has_python_tests(workdir)
                        && test_guard_warns < 3
                    {
                        let (verdict, thint) = testguard::run_test_guard(workdir);
                        match verdict {
                            TestVerdict::Skip => {
                                log!("[{i}] 测试守护跳过:环境无 pytest/python,无法自动验证测试");
                            }
                            TestVerdict::Fail => {
                                test_guard_warns += 1;
                                let h = if params.ablated("verify_feedback") {
                                    "pytest 输出末行".to_string()
                                } else {
                                    thint
                                };
                                let mut bak_hint = String::new();
                                if h.contains("SyntaxError") || h.contains("IndentationError") {
                                    if let Some(b) = first_py_bak(workdir) {
                                        bak_hint = format!("检测到语法错误。可用备份回滚:先把对应 .bak 内容恢复(如 copy {b} 覆盖原文件),或重建文件。\n");
                                    }
                                }
                                log!("[{i}] ⚠️ 测试未通过,拒绝 finish(第 {test_guard_warns}/3 次,满 3 次后放行)");
                                messages.push(Message::user(format!(
                                    "⚠️ 你的 finish 被拒绝:目录里的测试未通过。\n精确失败: {h}\n{bak_hint}请 read_file 查看失败处代码,修复测试与实现使其一致。运行方式选环境可用的:python -m pytest -q;环境没有 pytest 就用 python -m unittest 或直接 python test_xxx.py。全绿后再 finish(第 {test_guard_warns}/3 次拒绝,满 3 次后 finish 将放行)。"
                                )));
                                messages.push(Message::tool_output(
                                    "[finish 被拒绝:测试未全绿。修复实现与测试后再 finish。]",
                                ));
                                continue;
                            }
                            TestVerdict::Pass => {}
                        }
                    }
                    // Gate 3: files the summary claims must exist.
                    if !qa && !fg_off && finish_claim_warns < 2 {
                        let missing = artifacts::claimed_missing_files(&summary, workdir);
                        if !missing.is_empty() {
                            finish_claim_warns += 1;
                            log!("[{i}] ⚠️ 产物核对:summary 声称但缺失 {missing:?},拒绝 finish");
                            messages.push(Message::user(format!(
                                "⚠️ 你的 finish 被拒绝:summary 声称的以下产物在工作目录中不存在:\n{}\n请用 create_file/run_bash 真正生成它们;或修改 summary 只声称确实存在的文件,然后重新 finish。{}",
                                bullet(&missing),
                                ws_note(workdir)
                            )));
                            messages.push(Message::tool_output(
                                "[finish 被拒绝:summary 点名的文件缺失。先生成或修正 summary 后再 finish。]",
                            ));
                            continue;
                        }
                    }
                    // Gate 4: files the plan names must exist (existence, not checkmarks).
                    if !qa && !fg_off && plan_gate_warns < 2 {
                        let p_missing = artifacts::plan_named_missing(workdir);
                        if !p_missing.is_empty() {
                            plan_gate_warns += 1;
                            log!("[{i}] ⚠️ 计划核对:计划点名但缺失 {p_missing:?},拒绝 finish");
                            messages.push(Message::user(format!(
                                "⚠️ 你的 finish 被拒绝:你的计划(todo)里点名了以下产物文件,但它们在工作目录中不存在:\n{}\n两条路二选一:①用 create_file 真正生成它们;②若计划已不符合实际,用 todo(action=create, items=[与实际一致的新计划]) 重建计划后重新 finish。{}",
                                bullet(&p_missing),
                                ws_note(workdir)
                            )));
                            messages.push(Message::tool_output(
                                "[finish 被拒绝:计划点名的产物缺失。生成产物或重建计划后再 finish。]",
                            ));
                            continue;
                        }
                    }
                    // Gate 5: the plan must be synced before the end (2 refusals max).
                    if !qa && !params.ablated("finish_gate_text_only") && todo_gate_warns < 2 {
                        let (t_done, t_all) = todo_progress(&load_todo(workdir));
                        if t_all > 0 && t_done < t_all {
                            todo_gate_warns += 1;
                            log!("[{i}] ⚠️ 计划未同步({t_done}/{t_all}),拒绝 finish");
                            messages.push(Message::user(format!(
                                "⚠️ 你的 finish 被拒绝:计划(todo)里还有 {} 项未标记完成({t_done}/{t_all})。\n当前计划:\n{}\n请先把已完成的步骤逐个勾掉: todo(action=update, index=步骤号);已全部完成可一次同步: todo(action=update, all=true)。若仍有步骤没做,请继续执行后再 finish;重新 finish 时务必在 summary 里用两三句话向用户总结成果(完成了什么、产物是什么)。",
                                t_all - t_done,
                                load_todo_str(workdir)
                            )));
                            messages.push(Message::tool_output(
                                "[finish 暂缓:先把计划与实际进度同步(todo update)后再 finish。]",
                            ));
                            continue;
                        }
                    }
                    // Gate 6: once, re-inject the original task for a self-check.
                    if !qa && !finish_reread_used {
                        finish_reread_used = true;
                        let orig = qa::task_original_text(&messages, 2000);
                        if !orig.is_empty() {
                            log!(
                                "[{i}] ⚠️ 交付自查:回注任务原文({} 字符),要求自查后重新 finish",
                                text::clen(&orig)
                            );
                            messages.push(Message::user(format!(
                                "⚠️ 交付前自查:结束前逐条核对以下【原始任务指令】,确认你的最终交付完全满足其中每一条要求——特别是:答案/产物的格式(单位、小数位、取整方式)、命名、数值范围,以及任务明确点名的每一个子项。长任务执行到末尾容易遗忘题面细节。\n若发现任何一条不满足:先修正交付物,再重新 finish;若全部满足:重新 finish,并在 summary 里用两三句话向用户总结成果(完成了什么、产物/答案是什么)。\n--- 原始任务指令 ---\n{orig}\n--- 原始任务指令结束 ---"
                            )));
                            messages.push(Message::tool_output(
                                "[finish 待自查:按上方要求核对原始任务指令后重新 finish。]",
                            ));
                            continue;
                        }
                    }
                    // Closing visibility: a bare finish (no body, no summary) gets one
                    // tool-less call so the user sees a wrap-up.
                    if guards::semantically_empty(&content)
                        && summary.trim().is_empty()
                        && !qa
                        && !fg_off
                    {
                        let mut wm = messages.clone();
                        wm.push(Message::tool_output(res.clone()));
                        wm.push(Message::user(
                            "任务已收尾。请直接用文字向用户总结(不要调用任何工具):完成了什么、关键产物/结论是什么。不超过 5 句话。",
                        ));
                        if let Ok(wr) = backend.chat(model, &wm, &[], &mut |p| {
                            if let StreamPiece::Token(t) = p {
                                sink(AgentEvent::Text(t.to_string()));
                            }
                        }) {
                            let wt = wr.message.content.trim().to_string();
                            if !wt.is_empty() {
                                messages.push(Message::assistant(wt));
                            }
                        }
                    }
                    log!("\n===== TASK COMPLETE =====");
                    log!("{res}");
                    sink(AgentEvent::Finished {
                        summary: if summary.trim().is_empty() {
                            content.clone()
                        } else {
                            summary.clone()
                        },
                    });
                    save!();
                    return Ok(messages);
                }

                // Failure ladder: same tool failing repeatedly → change strategy / disable.
                if is_tool_error(&res) {
                    if small_model
                        && n == "run_bash"
                        && res.to_lowercase().contains("not recognized")
                        && bash_teach_warns < 2
                    {
                        bash_teach_warns += 1;
                        messages.push(Message::user(
                            "提示:run_bash 执行的是 Windows shell 命令(cmd),不能直接运行 Python 代码。正确做法:先用 create_file 把代码写入 .py 文件,再用 run_bash 运行:python 文件名.py",
                        ));
                    }
                    let fc = fail_count.entry(name.clone()).or_insert(0);
                    *fc += 1;
                    if *fc >= 3 {
                        if n == "edit_file" {
                            face.disable("edit_file");
                            messages.push(Message::user(
                                "⚠️ edit_file 已连续 3 次失败,已被禁用。以后一律用 create_file 写入完整文件内容来修改文件。",
                            ));
                            *fc = 0;
                        } else if redirect_warns < 4 {
                            redirect_warns += 1;
                            let path_refused =
                                res.contains("在工作目录之外") || res.contains("越界");
                            let hint = if path_refused {
                                "⚠️ 被拒绝的路径在工作目录之外。任务的全部输入文件都已经在你的工作目录里:立即执行 list_dir(path='.') 列出当前目录,直接读取列出的文件;不要再访问工作目录以外的任何路径。"
                            } else if n == "run_bash" {
                                "⚠️ 不要 cd 到 /workspace 等绝对路径——工作目录已设定,直接运行命令或用相对路径;先用 list_dir 确认目录内容,用 read_file/pwd 确认实际情况。"
                            } else {
                                "⚠️ 先用 list_dir / read_file 确认实际情况,重新构造参数;或换一个实现方式。"
                            };
                            let count = *fc;
                            messages.push(Message::user(format!(
                                "⚠️ 你已连续 {count} 次调用 {name} 失败。立刻停止重试,必须换策略:{hint}"
                            )));
                            *fc = 0;
                        }
                    }
                } else if fail_count.get(n).is_some_and(|c| *c > 0) {
                    fail_count.insert(name.clone(), 0); // only a success of the same tool resets
                }

                // Repeat-success streaks.
                let sig_now = (name.clone(), sig_json.clone());
                if last_sig_now.as_ref() == Some(&sig_now) {
                    same_sig_streak += 1;
                } else {
                    same_sig_streak = 1;
                }
                last_sig_now = Some(sig_now);
                if last_tool.as_deref() == Some(n) {
                    tool_streak += 1;
                } else {
                    last_tool = Some(name.clone());
                    tool_streak = 1;
                }
                if tool_streak >= 5 && streak_warns < 3 {
                    streak_warns += 1;
                    messages.push(Message::user(format!(
                        "⚠️ 你已连续 {tool_streak} 次调用 {name}。若结果类似或没有新进展,停止重复:综合已有结果推进到下一步(写文件 / 换其他工具 / 调用 finish)。若你在做批量步骤(每次参数不同)则属正常,继续。"
                    )));
                    tool_streak = 1;
                }
                if !params.ablated("anti_loop")
                    && tool_streak >= 8
                    && same_sig_streak >= 4
                    && n != "finish"
                {
                    // Only a true loop: same tool 8×, same arguments 4× in a row.
                    face.disable(n);
                    messages.push(Message::user(format!(
                        "⚠️ {name} 已连续调用 {tool_streak} 次,现被禁用。停止重复它,直接推进实际工作(写文件/执行/调用 finish)。"
                    )));
                    tool_streak = 1;
                }
                // Research streak, fingerprint level: a new query is progress.
                if RESEARCH_ALL.contains(&n) {
                    let fp = text::head(&sig_json, 256).to_string();
                    if research_seen.contains(&fp) {
                        research_streak += 1;
                    } else {
                        research_seen.insert(fp);
                        research_streak = 0;
                    }
                    if research_streak >= 6 && research_warns < 3 {
                        research_warns += 1;
                        for t in RESEARCH_ALL {
                            face.disable(t);
                        }
                        messages.push(Message::user(format!(
                            "⚠️ 你已连续 {research_streak} 次重复完全相同的网络查询且尚未产出任何文件。网络工具已被禁用:换一个具体不同的查询角度,或先用 create_file 把已发现的线索写成文件,然后调用 finish。被禁用后如需继续搜索,请改写查询词并调用 enable_tools 重新启用。"
                        )));
                        research_streak = 0;
                        research_seen.clear();
                    }
                } else if matches!(
                    n,
                    "create_file" | "edit_file" | "append_file" | "finish" | "run_bash"
                ) {
                    research_streak = 0;
                }
                // Plan-tool loop: same (action,index) 6× in a row, or 15 todo calls without output.
                if n == "todo" {
                    todo_streak += 1;
                    if same_sig_streak >= 6 || todo_streak >= 15 {
                        face.disable("todo");
                        messages.push(Message::user(format!(
                            "⚠️ todo 已被禁用(同一调用连发 {same_sig_streak} 次/累计 {todo_streak} 次无实际工作)。立即改用产出型工具推进:create_file 写产出文件、run_bash 执行验证;需要记录计划就直接写进产出文件里。"
                        )));
                        todo_streak = 0;
                    }
                } else if PRODUCTIVE.contains(&n) {
                    todo_streak = 0;
                }
                // Shotgun-edit guard counts only landed edits.
                if n == "edit_file" && res.starts_with("[edited") {
                    let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                    if let Some(hint) = edit_guard.observe(path) {
                        log!("[{i}] ⚠️ 霰弹枪编辑提示: {path}");
                        messages.push(Message::user(hint));
                    }
                }
                // Time budget pacing (task mode only).
                if budget_total > 0.0 && !qa {
                    let el = prev_elapsed + t0.elapsed().as_secs_f64();
                    budget::save_elapsed(workdir, el);
                    if let Some(bn) = budget::budget_nudge(budget_total, el, &mut budget_fired) {
                        log!("[{i}] ⏱ 预算提示已注入(已用 {:.0}min)", el / 60.0);
                        messages.push(Message::user(bn));
                    }
                }
                messages.push(Message::tool_output(res));
                // Stale-plan reminder (never a block, twice at most).
                if !qa && todo_nudge_warns < 2 && todo_stale >= TODO_STALE_NUDGE {
                    let (t_done, t_all) = todo_progress(&load_todo(workdir));
                    if t_all > 0 && t_done < t_all {
                        let stale = todo_stale;
                        todo_nudge_warns += 1;
                        todo_stale = 0;
                        messages.push(Message::user(format!(
                            "⚠️ 计划进度提醒:你已完成多步实际工作,但计划仍是 {t_done}/{t_all}。每完成一步就同步一次: todo(action=update, index=已完成步骤号)。\n当前计划:\n{}",
                            load_todo_str(workdir)
                        )));
                        log!("[{i}] 📌 计划停滞提醒({t_done}/{t_all},连续 {stale} 步未更新)");
                    }
                }
            }
        }
        if exhausted {
            log!("===== MAX ITERATIONS =====");
            sink(AgentEvent::MaxIterations);
        }
        save!();
        Ok(messages)
    }
}

fn bullet(items: &[String]) -> String {
    items
        .iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_py_bak(workdir: &Path) -> Option<String> {
    let mut baks: Vec<String> = std::fs::read_dir(workdir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".py.bak"))
        .collect();
    baks.sort();
    baks.into_iter().next()
}

#[allow(clippy::too_many_arguments)]
fn compact<B: ChatBackend>(
    backend: &mut B,
    model: &str,
    messages: Vec<Message>,
    level: u8,
    todo: Option<String>,
    files: Option<String>,
    transcript: &mut Transcript,
    sink: &mut dyn FnMut(AgentEvent),
) -> Vec<Message> {
    let before = messages.len();
    let c = compact_history(
        backend,
        model,
        messages,
        level,
        todo.as_deref(),
        files.as_deref(),
        &mut |s| sink(AgentEvent::Log(s)),
    );
    if c.summarised {
        transcript.mark_compacted();
    }
    if c.messages.len() != before {
        sink(AgentEvent::Compacted {
            level,
            before,
            after: c.messages.len(),
        });
    }
    c.messages
}
