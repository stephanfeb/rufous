//! `boubou`: command-line entry point (counterpart of `ollama_agent.py`'s `main()`).
//!
//! The agent loop is synchronous and owns its own Ollama backend, so `main` is plain
//! Rust; the async-only subcommands (model listing, doctor) run on a local runtime.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use boubou_core::agent::session::{self, load_checkpoint, load_session, sessions_dir};
use boubou_core::agent::{Agent, AgentEvent, RunParams};
use boubou_core::config::{agent_home, config_path, Config};
use boubou_core::guards::budget::{
    elapsed_meta_path, fmt_hm, parse_duration_str, resolve_time_budget, BudgetSource,
};
use boubou_core::ollama::{Client, Message};
use boubou_core::parallel::{
    env_snapshot, format_capacity_table, progress_line, CapacityPlanner, ParallelConfig,
    ResourceProbe, SEVERE_EXIT_CODE,
};
use boubou_core::skills::SkillSet;
use boubou_core::text::read_text;
use boubou_core::todo::todo_file;
use boubou_core::{mcp, tools};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "boubou", version = boubou_core::VERSION, about = "Boubou (布布) · local-model autonomous agent")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run a task file to completion (resumes a checkpoint in the workdir by default).
    Run {
        model: String,
        taskfile: PathBuf,
        workdir: PathBuf,
        /// Session name under `~/.ollama_agent/sessions`.
        #[arg(long)]
        session: Option<String>,
        /// Start fresh: discard the checkpoint and the elapsed-time sidecar.
        #[arg(long)]
        new: bool,
        /// Continue the conversation: append the task to the saved session/checkpoint.
        #[arg(long)]
        append: bool,
        /// Time budget in minutes, or with a unit: `40`, `1.5h`, `90m`, `半小时`.
        #[arg(long = "time-budget")]
        time_budget: Option<String>,
    },
    /// Interactive chat in a workspace.
    Chat {
        model: String,
        workdir: PathBuf,
        #[arg(long)]
        session: Option<String>,
        #[arg(long = "time-budget")]
        time_budget: Option<String>,
    },
    /// List models installed in Ollama.
    Models,
    /// List skills visible from a workspace.
    Skills {
        #[arg(default_value = ".")]
        workdir: PathBuf,
    },
    /// Print the tool schemas sent to the model (core, plus any named advanced tools).
    Tools {
        /// Advanced tools to include, e.g. `--enable web_fetch --enable mcp_call`.
        #[arg(long = "enable")]
        enable: Vec<String>,
    },
    /// Check the local setup: config, Ollama, models, skills, MCP servers.
    Doctor,
    /// Print this machine's parallel-dispatch capacity plan (read-only, nothing is spawned).
    Plan,
    /// Serve the local web UI on 127.0.0.1 (Mingbird's `webui/server.py`).
    Serve {
        #[arg(long, default_value_t = 8765)]
        port: u16,
        /// Workspace the agent works in (default `~/agent_tasks/work`).
        #[arg(long)]
        workdir: Option<PathBuf>,
    },
    /// Run one tool call directly (debugging): `boubou tool web_search '{"query":"rust"}'`.
    Tool {
        name: String,
        /// JSON object of arguments (default `{}`).
        #[arg(default_value = "{}")]
        args: String,
        /// Workspace the tool runs in.
        #[arg(long, default_value = ".")]
        workdir: PathBuf,
    },
}

fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let cfg = Config::load().context("loading config.json")?;

    match cli.cmd {
        Cmd::Models => {
            let rt = tokio::runtime::Runtime::new()?;
            let client = Client::new(cfg.ollama_host());
            for m in rt.block_on(client.list_models()).context("GET /api/tags")? {
                println!("{:<40} {}", m.name, human_size(m.size));
            }
        }
        Cmd::Skills { workdir } => {
            let set = SkillSet::standard(&workdir, &agent_home(), &install_dir());
            if set.is_empty() {
                println!("(no skills found)");
            } else {
                println!("{}", set.listing());
            }
        }
        Cmd::Tools { enable } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&tools::tool_schemas(&enable))?
            );
        }
        Cmd::Doctor => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(doctor(&cfg))?;
        }
        Cmd::Plan => {
            let pc = ParallelConfig::from_config(&cfg).with_env(&env_snapshot());
            let model = if pc.child_model.is_empty() {
                "(未配置 child_model)".to_string()
            } else {
                pc.child_model.clone()
            };
            let plan = ResourceProbe::new(cfg.ollama_host()).plan_capacity(&model, &pc);
            println!("{}", format_capacity_table(&plan, &pc));
            println!(
                "  (enabled={}, child_model={:?}, hard_cap={})",
                pc.enabled, pc.child_model, pc.hard_cap
            );
        }
        Cmd::Serve { port, workdir } => {
            let wd = match workdir {
                Some(w) => std::path::absolute(w)?,
                None => boubou_web::state::default_workdir(),
            };
            let state = std::sync::Arc::new(boubou_web::AppState::new(agent_home(), wd));
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async move {
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                let (local, server) = boubou_web::bind(addr, state).await?;
                println!(
                    "Boubou Web UI {}: http://{local}  (local only · 仅本机访问)",
                    boubou_core::VERSION
                );
                server.await
            })?;
        }
        Cmd::Tool {
            name,
            args,
            workdir,
        } => {
            let args: serde_json::Value =
                serde_json::from_str(&args).context("parsing tool arguments as JSON")?;
            let mut ex = boubou_core::exec::Executor::new(std::path::absolute(workdir)?)
                .with_configured_mcp();
            println!("{}", ex.run_tool(&name, &args));
        }
        Cmd::Run {
            model,
            taskfile,
            workdir,
            session,
            new,
            append,
            time_budget,
        } => {
            let opts = RunOpts {
                session,
                new,
                append,
                time_budget,
            };
            run_task(&cfg, model, &taskfile, &workdir, opts)?;
        }
        Cmd::Chat {
            model,
            workdir,
            session,
            time_budget,
        } => chat(&cfg, model, &workdir, session, time_budget)?,
    }
    Ok(())
}

#[derive(Debug)]
struct RunOpts {
    session: Option<String>,
    new: bool,
    append: bool,
    time_budget: Option<String>,
}

/// `--time-budget` value → minutes, with Mingbird's transparency lines.
fn cli_budget(raw: Option<&str>) -> Option<f64> {
    let raw = raw?;
    match parse_duration_str(raw) {
        Some(m) => {
            println!("[time-budget] CLI 时限 {}", fmt_hm(m));
            Some(m)
        }
        None => {
            println!(
                "[time-budget] 无法解析 --time-budget 的值,已忽略(示例: 40 / 1.5h / 90m / 半小时)"
            );
            None
        }
    }
}

fn budget_label(src: BudgetSource, minutes: f64) -> String {
    let v = boubou_core::text::fmt_g(minutes);
    match src {
        BudgetSource::Prompt => format!("提示词识别到时限 {v} 分钟"),
        BudgetSource::Cli => format!("CLI 时限 {v} 分钟"),
        BudgetSource::Env => format!("环境变量时限 {v} 分钟"),
        BudgetSource::Off => format!("{v} 分钟"),
    }
}

fn run_task(
    cfg: &Config,
    model: String,
    taskfile: &Path,
    workdir: &Path,
    opts: RunOpts,
) -> Result<()> {
    let workdir = std::path::absolute(workdir)?;
    std::fs::create_dir_all(&workdir).with_context(|| format!("creating {}", workdir.display()))?;
    let task = read_text(&std::path::absolute(taskfile)?)
        .with_context(|| format!("reading {}", taskfile.display()))?;
    let params = RunParams::from_env(model, &workdir)?;
    let cli_tb = cli_budget(opts.time_budget.as_deref());

    let (tb_min, tb_src) = resolve_time_budget(
        cli_tb,
        Some(&task),
        params.budget_env_sec.unwrap_or(0.0),
        std::env::var("AGENT_TIME_BUDGET_PARSE").map_or(true, |v| v != "0"),
    );
    if let Some(m) = tb_min {
        println!("[time-budget] {} — {}", budget_label(tb_src, m), fmt_hm(m));
    }
    let budget_sec = tb_min.map(|m| m * 60.0);

    let ckpt = session::checkpoint_path(&workdir);
    if opts.new {
        if ckpt.exists() {
            std::fs::remove_file(&ckpt)?;
        }
        let meta = elapsed_meta_path(&workdir);
        if meta.exists() {
            std::fs::remove_file(meta)?;
        }
    }

    let mut agent =
        Agent::standard(params, cfg.ollama_host(), cfg.offline())?.with_sink(print_event);
    let sdir = sessions_dir(&agent_home());
    let session = opts.session.as_deref();

    let msgs: Vec<Message> = if opts.new {
        agent.task_messages(&task)
    } else if opts.append {
        let mut msgs = session
            .and_then(|s| load_session(&sdir, s))
            .or_else(|| load_checkpoint(&workdir))
            .unwrap_or_default();
        if msgs.is_empty() {
            agent.task_messages(&task)
        } else {
            println!("[RESUMED: {} msgs + 追加新消息]", msgs.len());
            msgs.push(Message::user(task.clone()));
            msgs
        }
    } else if let Some(msgs) = load_checkpoint(&workdir) {
        println!("[RESUMED checkpoint: {} msgs]", msgs.len());
        msgs
    } else if let Some(mut msgs) = session.and_then(|s| load_session(&sdir, s)) {
        println!(
            "[RESUMED session: {} ({} msgs)]",
            session.unwrap_or_default(),
            msgs.len()
        );
        msgs.push(Message::user("继续之前的对话,完成或回答当前需求。"));
        msgs
    } else {
        agent.task_messages(&task)
    };
    if msgs.len() == 2 {
        // A brand-new task: clear the previous task's plan so the panel does not show it.
        let tj = todo_file(&workdir);
        if tj.exists() {
            let _ = std::fs::remove_file(tj);
        }
    }
    match agent.run(msgs, session, budget_sec) {
        Ok(_) => Ok(()),
        Err(boubou_core::Error::ChildTerminated(reason)) => {
            // The child sandbox fuse: the dispatcher reads exit 53 and falls back without a retry.
            println!("[child-mode] 严重违规熔断: {reason}");
            std::process::exit(SEVERE_EXIT_CODE);
        }
        Err(e) => Err(e.into()),
    }
}

fn chat(
    cfg: &Config,
    model: String,
    workdir: &Path,
    session: Option<String>,
    time_budget: Option<String>,
) -> Result<()> {
    let workdir = std::path::absolute(workdir)?;
    std::fs::create_dir_all(&workdir)?;
    let params = RunParams::from_env(model, &workdir)?;
    let cli_tb = cli_budget(time_budget.as_deref());
    let mut agent =
        Agent::standard(params, cfg.ollama_host(), cfg.offline())?.with_sink(print_event);
    let sdir = sessions_dir(&agent_home());
    let mut msgs = session
        .as_deref()
        .and_then(|s| load_session(&sdir, s))
        .unwrap_or_else(|| {
            let mut m = agent.task_messages("");
            m.pop();
            m
        });
    println!("[交互模式] 输入 exit 退出。");
    let stdin = std::io::stdin();
    loop {
        print!("你> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let user = line.trim();
        if user.is_empty() {
            continue;
        }
        if user.eq_ignore_ascii_case("exit") || user.eq_ignore_ascii_case("quit") {
            break;
        }
        msgs.push(Message::user(user));
        msgs = agent.run(msgs, session.as_deref(), cli_tb.map(|m| m * 60.0))?;
        println!();
    }
    Ok(())
}

/// Mingbird's stdout protocol: streamed text as is, harness lines verbatim.
fn print_event(ev: AgentEvent) {
    match ev {
        AgentEvent::Text(t) | AgentEvent::Thinking(t) => {
            print!("{t}");
            let _ = std::io::stdout().flush();
        }
        AgentEvent::Log(l) => println!("{l}"),
        AgentEvent::Dispatch(v) => println!("{}", progress_line(&v)),
        AgentEvent::Error(e) => eprintln!("[error] {e}"),
        AgentEvent::ToolCall(_)
        | AgentEvent::ToolResult { .. }
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::Compacted { .. }
        | AgentEvent::Finished { .. }
        | AgentEvent::MaxIterations => {}
    }
}

async fn doctor(cfg: &Config) -> Result<()> {
    let home = agent_home();
    println!("boubou {}", boubou_core::VERSION);
    println!("agent home   : {}", home.display());
    println!(
        "config       : {} ({})",
        config_path().display(),
        if config_path().exists() {
            "present"
        } else {
            "defaults"
        }
    );
    println!("offline mode : {}", cfg.offline());

    let client = Client::new(cfg.ollama_host());
    print!("ollama       : {} ... ", client.base_url());
    match client.version().await {
        Ok(v) => {
            println!("ok (v{v})");
            match client.list_models().await {
                Ok(models) if models.is_empty() => {
                    println!("models       : none installed (try `ollama pull gemma4:e2b`)")
                }
                Ok(models) => {
                    println!("models       : {}", models.len());
                    for m in models {
                        println!("  {:<38} {}", cfg.display_name(&m.name), human_size(m.size));
                    }
                }
                Err(e) => println!("models       : error: {e}"),
            }
        }
        Err(e) => println!("unreachable ({e})"),
    }

    let skills = SkillSet::standard(&std::env::current_dir()?, &home, &install_dir());
    println!("skills       : {}", skills.len());

    match mcp::load(&home, &install_dir()) {
        Ok(servers) => {
            println!("mcp servers  : {}", servers.len());
            let manifest = if servers.is_empty() {
                Default::default()
            } else {
                println!("  (probing, up to {}s each)", mcp::PROBE_TIMEOUT.as_secs());
                mcp::McpRegistry::new(&home, servers.clone()).manifest(true)
            };
            for (name, s) in &servers {
                let kind = if s.is_http() { "http" } else { "stdio" };
                let bad = s.unknown_categories();
                let warn = if bad.is_empty() {
                    String::new()
                } else {
                    format!("  ⚠ unrouted categories: {}", bad.join(", "))
                };
                let tools = manifest.get(name).map(|m| &m.tools[..]).unwrap_or(&[]);
                let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).take(8).collect();
                let more = if tools.len() > 8 {
                    format!(" (+{} more)", tools.len() - 8)
                } else {
                    String::new()
                };
                println!(
                    "  {name:<20} {kind}  {} tools: {}{more}{warn}",
                    tools.len(),
                    names.join(", ")
                );
            }
        }
        Err(e) => println!("mcp servers  : error: {e}"),
    }
    Ok(())
}

fn human_size(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    if bytes == 0 {
        return String::new();
    }
    format!("{:.1} GB", bytes as f64 / GB)
}
