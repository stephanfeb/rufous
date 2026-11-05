//! `rufous`: command-line entry point (counterpart of `ollama_agent.py`'s `main()`).
//!
//! The agent loop is synchronous and owns its own Ollama backend, so `main` is plain
//! Rust; the async-only subcommands (model listing, doctor) run on a local runtime.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rufous_kernel::agent::chat::model_can_think;
use rufous_kernel::agent::openai;
use rufous_kernel::agent::session::{self, load_checkpoint, load_session, sessions_dir};
use rufous_kernel::agent::{Agent, AgentEvent, RunParams, TurnPolicy};
use rufous_kernel::config::{agent_home, config_path, BackendKind, Config};
use rufous_kernel::guards::budget::{
    elapsed_meta_path, fmt_hm, parse_duration_str, resolve_time_budget, BudgetSource,
};
use rufous_kernel::ollama::{Client, Message};
use rufous_kernel::ollama_proc::OllamaProc;
use rufous_kernel::parallel::{
    env_snapshot, format_capacity_table, progress_line, CapacityPlanner, ParallelConfig,
    ResourceProbe, SEVERE_EXIT_CODE,
};
use rufous_kernel::skills::{Skill, SkillSet};
use rufous_kernel::text::read_text;
use rufous_kernel::todo::todo_file;
use rufous_kernel::{mcp, tools};

#[derive(Debug, Parser)]
#[command(name = "rufous", version = rufous_kernel::VERSION, about = "rufous · local-model autonomous agent")]
struct Cli {
    /// Model server: `ollama`, `lmstudio` (`local_backend.base_url` in config.json, else
    /// http://127.0.0.1:1234/v1) or `openai` (needs `local_backend.base_url`). Over the
    /// config's `backend` and `local_backend.type`, and over an enabled cloud provider.
    #[arg(long, global = true, value_parser = BackendKind::NAMES)]
    backend: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

/// `rufous mcp …` (bead boubou-r5f), in the shape of `claude mcp`.
#[derive(Debug, Subcommand)]
enum McpCmd {
    /// Add a server: everything after the name (or after `--`) is the command and its
    /// arguments, or the URL with `--transport http`.
    Add {
        /// `user` (every session, `~/.ollama_agent/mcp.json`) or `project` (`./.mcp.json`).
        #[arg(short, long, default_value = "user", value_parser = ["user", "project"])]
        scope: String,
        /// `stdio` (spawn a command) or `http` (streamable HTTP at a URL).
        #[arg(short, long, default_value = "stdio", value_parser = ["stdio", "http"])]
        transport: String,
        /// Environment for a stdio server, `KEY=VALUE`; repeatable.
        #[arg(short, long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// Header for an http server, `"Name: value"`; repeatable.
        #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
        header: Vec<String>,
        /// Replace a server of the same name.
        #[arg(long)]
        force: bool,
        name: String,
        /// The command and its arguments, or the URL.
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// List the configured servers and the file each comes from (no probing; `rufous doctor`
    /// probes them).
    List,
    /// Print one server's entry and its file.
    Get { name: String },
    /// Remove a server; `--scope` picks the file when both have the name.
    Remove {
        #[arg(short, long, value_parser = ["user", "project"])]
        scope: Option<String>,
        name: String,
    },
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
        /// Time budget in minutes, or with a unit: `40`, `1.5h`, `90m`, `half an hour`.
        #[arg(long = "time-budget")]
        time_budget: Option<String>,
        /// The turn policy and skills to run the task under: `unattended` is the batch
        /// harness; `general` and `coding` run the task file as the one message of a web
        /// chat or an editor turn, for the bench (docs/refactor.md, step 10).
        #[arg(long, default_value = "unattended", value_parser = ["unattended", "general", "coding"])]
        policy: String,
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
    /// Configure MCP servers: `rufous mcp add framit -- node ./dist/cli.js serve`.
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
    /// Print this machine's parallel-dispatch capacity plan (read-only, nothing is spawned).
    Plan,
    /// Transcribe a 16 kHz WAV file (local sherpa-onnx model, else an Ollama audio model).
    Voice {
        wav: PathBuf,
        /// Language of the bundled model to use (`en` is the only one shipped).
        #[arg(long, default_value = "en")]
        lang: String,
        /// Ollama audio-capable model to fall back to when no local model is installed.
        #[arg(long)]
        model: Option<String>,
    },
    /// Serve the Agent Client Protocol over stdio, for an editor (Zed, JetBrains, Neovim)
    /// that spawns rufous as its agent. stdout carries the protocol; logs go to stderr.
    Acp {
        /// Model tag (or a display name from `config.json` `models`); `RUFOUS_MODEL` otherwise.
        #[arg(long)]
        model: Option<String>,
        /// Context window in tokens for every editor session (`AGENT_CTX` otherwise, default
        /// 131072). Editors pass no environment, so this is how the window is set.
        #[arg(long)]
        ctx: Option<u32>,
        /// Turn policy for every session: `coding` (an editor's agent, the default) or
        /// `general` (a desktop app's assistant: the web chat's preset and skills).
        #[arg(long, default_value = "coding", value_parser = ["coding", "general"])]
        policy: String,
        /// The MCP servers the client passes in `session/new` are the host app's own tools,
        /// offered on every turn (docs/host-tools.md). Defaults to on under `--policy general`
        /// and off under `coding`.
        #[arg(long, value_parser = ["on", "off"])]
        host_tools: Option<String>,
    },
    /// Serve the local web UI on 127.0.0.1 (Mingbird's `webui/server.py`).
    Serve {
        #[arg(long, default_value_t = 8765)]
        port: u16,
        /// Workspace the agent works in (default `~/agent_tasks/work`).
        #[arg(long)]
        workdir: Option<PathBuf>,
    },
    /// Classify user messages as question / task / follow-up with the heuristic and, with
    /// `--model`, the decision model too; one JSON object per line (`bench/decider_eval.py`).
    Decide {
        /// JSONL file of `{"text": …, "has_progress": bool}` (default: stdin).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Decision model tag (Ollama `decision` capability), e.g. `d1:latest`.
        #[arg(long)]
        model: Option<String>,
        /// Per-call timeout in milliseconds.
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,
    },
    /// Run one tool call directly (debugging): `rufous tool web_search '{"query":"rust"}'`.
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

fn scope_of(s: &str) -> mcp::edit::Scope {
    if s == "project" {
        mcp::edit::Scope::Project
    } else {
        mcp::edit::Scope::User
    }
}

/// `KEY=VALUE` (env) or `Name: value` (header) pairs into a map.
fn pairs(items: &[String], sep: char, what: &str) -> Result<BTreeMap<String, String>> {
    items
        .iter()
        .map(|it| match it.split_once(sep) {
            Some((k, v)) if !k.trim().is_empty() => {
                Ok((k.trim().to_string(), v.trim().to_string()))
            }
            _ => anyhow::bail!(
                "{what} {it:?}: expected {}",
                if sep == '=' {
                    "KEY=VALUE"
                } else {
                    "\"Name: value\""
                }
            ),
        })
        .collect()
}

/// `rufous mcp …`: edits the user or project server file (`mcp::edit`).
fn mcp_cmd(cmd: McpCmd, home: &Path, cwd: &Path) -> Result<()> {
    use mcp::edit::{self, Scope, Transport};
    match cmd {
        McpCmd::Add {
            scope,
            transport,
            env,
            header,
            force,
            name,
            command,
        } => {
            let scope = scope_of(&scope);
            let t = if transport == "http" {
                if command.len() != 1 {
                    anyhow::bail!("--transport http takes one URL after the name");
                }
                if !env.is_empty() {
                    anyhow::bail!("--env is for stdio servers; an http server takes --header");
                }
                Transport::Http {
                    url: command[0].clone(),
                    headers: pairs(&header, ':', "--header")?,
                }
            } else {
                if !header.is_empty() {
                    anyhow::bail!("--header is for http servers; a stdio server takes --env");
                }
                let mut it = command.into_iter();
                Transport::Stdio {
                    command: it.next().unwrap_or_default(),
                    args: it.collect(),
                    env: pairs(&env, '=', "--env")?,
                }
            };
            let path = scope.path(home, cwd);
            let seed = install_dir().join("mcp.json");
            let seed = (scope == Scope::User).then_some(seed.as_path());
            let replaced = edit::add_server(&path, seed, &name, &t, force)?;
            edit::invalidate_manifest(home);
            println!(
                "{} MCP server {name} ({} scope) in {}",
                if replaced { "Replaced" } else { "Added" },
                scope.label(),
                path.display()
            );
        }
        McpCmd::List => {
            let mut any = false;
            for scope in [Scope::User, Scope::Project] {
                let path = scope.path(home, cwd);
                let servers = edit::servers_in(&path)?;
                if servers.is_empty() {
                    continue;
                }
                any = true;
                println!("{} ({}):", scope.label(), path.display());
                for (name, s) in &servers {
                    let what = match (&s.url, &s.command) {
                        (Some(u), _) => format!("{u} (http)"),
                        (None, Some(c)) => std::iter::once(c.as_str())
                            .chain(s.args.iter().map(String::as_str))
                            .collect::<Vec<_>>()
                            .join(" "),
                        (None, None) => "(no command or url)".to_string(),
                    };
                    println!("  {name}: {what}");
                }
            }
            if !any {
                println!("No MCP servers configured. Add one with `rufous mcp add <name> -- <command> [args…]`.");
            }
        }
        McpCmd::Get { name } => {
            let mut found = false;
            for scope in [Scope::User, Scope::Project] {
                let path = scope.path(home, cwd);
                if let Some(e) = edit::raw_entry(&path, &name)? {
                    found = true;
                    println!("{name} ({} scope, {}):", scope.label(), path.display());
                    println!("{}", serde_json::to_string_pretty(&e)?);
                }
            }
            if !found {
                anyhow::bail!("no MCP server named {name:?} (see `rufous mcp list`)");
            }
        }
        McpCmd::Remove { scope, name } => {
            let scopes: Vec<Scope> = match scope.as_deref() {
                Some(s) => vec![scope_of(s)],
                None => [Scope::User, Scope::Project]
                    .into_iter()
                    .filter(|sc| {
                        edit::raw_entry(&sc.path(home, cwd), &name)
                            .ok()
                            .flatten()
                            .is_some()
                    })
                    .collect(),
            };
            match scopes.as_slice() {
                [] => anyhow::bail!("no MCP server named {name:?} (see `rufous mcp list`)"),
                [sc] => {
                    let path = sc.path(home, cwd);
                    if !edit::remove_server(&path, &name)? {
                        anyhow::bail!("no MCP server named {name:?} in {}", path.display());
                    }
                    edit::invalidate_manifest(home);
                    println!("Removed MCP server {name} from {}", path.display());
                }
                _ => anyhow::bail!(
                    "{name:?} is in both the user and the project file; pass --scope user or --scope project"
                ),
            }
        }
    }
    Ok(())
}

fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Mirrors every `WARN`/`ERROR` tracing event into the open session logs
/// ([`session::mirror_warning`]), so the skills and fs warnings sit in sequence with the
/// calls of the session they belong to; stderr keeps its own copy as before.
struct SessionLogMirror;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SessionLogMirror {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let meta = event.metadata();
        if *meta.level() > tracing::Level::WARN {
            return;
        }
        struct Msg(String);
        impl tracing::field::Visit for Msg {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                } else {
                    if !self.0.is_empty() {
                        self.0.push(' ');
                    }
                    self.0.push_str(&format!("{}={value:?}", field.name()));
                }
            }
        }
        let mut msg = Msg(String::new());
        event.record(&mut msg);
        session::mirror_warning(&format!("{} {}: {}", meta.level(), meta.target(), msg.0));
    }
}

fn main() -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(SessionLogMirror)
        .init();

    let cli = Cli::parse();
    let mut cfg = Config::load().context("loading config.json")?;
    cfg.backend_flag = cli.backend.as_deref().and_then(BackendKind::parse);

    match cli.cmd {
        Cmd::Models => {
            let rt = tokio::runtime::Runtime::new()?;
            let client = Client::new(cfg.ollama_host());
            for m in rt.block_on(client.list_models()).context("GET /api/tags")? {
                println!("{:<40} {}", m.name, human_size(m.size));
            }
        }
        Cmd::Skills { workdir } => {
            let set = SkillSet::standard(&workdir, &agent_home(), &install_dir())
                .with_builtin(builtin_skills());
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
        Cmd::Decide {
            file,
            model,
            timeout_ms,
        } => decide(&cfg, file.as_deref(), model.as_deref(), timeout_ms)?,
        Cmd::Doctor => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(doctor(&cfg))?;
        }
        Cmd::Mcp { cmd } => mcp_cmd(cmd, &agent_home(), &std::env::current_dir()?)?,
        Cmd::Voice { wav, lang, model } => {
            let bytes =
                std::fs::read(&wav).with_context(|| format!("reading {}", wav.display()))?;
            let bases = rufous_voice::stt_bases(&install_dir(), &agent_home());
            let host = cfg.ollama_host();
            let req = rufous_voice::Request {
                wav: &bytes,
                lang: &lang,
                bases: &bases,
                ollama_host: &host,
                fallback_model: model.as_deref(),
            };
            match rufous_voice::transcribe(&req) {
                Ok(t) => {
                    eprintln!(
                        "[voice] engine={} {:.1}s",
                        t.engine.as_str(),
                        t.duration_secs
                    );
                    println!("{}", t.text);
                }
                Err(rufous_voice::VoiceError::Unavailable(why)) => {
                    anyhow::bail!(
                        "{why}: {}",
                        rufous_voice::install_hint(&lang, &agent_home())
                    );
                }
                Err(e) => anyhow::bail!("{e}"),
            }
        }
        Cmd::Plan => {
            let pc = ParallelConfig::from_config(&cfg).with_env(&env_snapshot());
            let model = if pc.child_model.is_empty() {
                "(child_model not configured)".to_string()
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
        Cmd::Acp {
            model,
            ctx,
            policy,
            host_tools,
        } => {
            let key = match model
                .or_else(|| std::env::var("RUFOUS_MODEL").ok())
                .or_else(|| std::env::var("BOUBOU_MODEL").ok())
            {
                Some(m) if !m.trim().is_empty() => m,
                _ => anyhow::bail!("rufous acp needs --model <tag> (or RUFOUS_MODEL)"),
            };
            let model = cfg.models.get(&key).cloned().unwrap_or(key);
            if let Some(problem) = cfg.backend_problem() {
                anyhow::bail!("rufous acp: {problem}");
            }
            if cfg.backend_openai().is_none()
                && cfg.cloud_provider().is_none()
                && !OllamaProc::from_config(&cfg).ensure(std::time::Duration::from_secs(25))
            {
                eprintln!("[rufous acp] could not start ollama; run `ollama serve` first");
            }
            let host_tools = acp_host_tools(&policy, host_tools.as_deref());
            let (policy, skills) = product(&policy);
            eprintln!(
                "[rufous acp] policy {}, host tools {}",
                policy.name,
                if host_tools { "on" } else { "off" }
            );
            let factory = rufous_coding::runner::StandardFactory {
                cfg: cfg.clone(),
                num_ctx: ctx,
                policy,
                skills,
                host_tools,
            };
            let agent = rufous_coding::RufousAcp::with_factory(
                agent_home(),
                model,
                std::sync::Arc::new(factory),
            );
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(agent.serve_stdio())
                .map_err(|e| anyhow::anyhow!("acp: {e}"))?;
        }
        Cmd::Serve { port, workdir } => {
            let wd = match workdir {
                Some(w) => std::path::absolute(w)?,
                None => rufous_general::state::default_workdir(),
            };
            let mut state = rufous_general::AppState::new(agent_home(), wd);
            state.backend_flag = cfg.backend_flag;
            let state = std::sync::Arc::new(state);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async move {
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                let (local, server) = rufous_general::bind(addr, state).await?;
                println!(
                    "rufous Web UI {}: http://{local}  (local only)",
                    rufous_kernel::VERSION
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
            let mut ex = rufous_kernel::exec::Executor::new(std::path::absolute(workdir)?)
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
            policy,
        } => {
            let opts = RunOpts {
                session,
                new,
                append,
                time_budget,
                policy,
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
    policy: String,
}

/// `rufous acp --host-tools`: what the flag says, else on for a desktop app's `general`
/// sessions and off for an editor's `coding` ones (docs/host-tools.md).
fn acp_host_tools(policy: &str, flag: Option<&str>) -> bool {
    match flag {
        Some(v) => v == "on",
        None => policy == "general",
    }
}

/// The preset and the built-in skills `rufous run --policy <name>` runs under. `unattended`
/// is the batch harness with every product's skills; a product's name is that product's
/// preset and skills, so the bench can measure what the web chat or an editor turn would
/// do with the task. The workdir layout and the checkpoint stay the CLI's either way.
fn product(policy: &str) -> (TurnPolicy, Vec<Skill>) {
    match policy {
        "general" => (
            rufous_general::harness::policy(),
            rufous_general::harness::skills(),
        ),
        "coding" => (
            rufous_coding::harness::policy(),
            rufous_coding::harness::skills(),
        ),
        _ => (TurnPolicy::unattended(), builtin_skills()),
    }
}

/// `--time-budget` value → minutes, with Mingbird's transparency lines.
fn cli_budget(raw: Option<&str>) -> Option<f64> {
    let raw = raw?;
    match parse_duration_str(raw) {
        Some(m) => {
            println!("[time-budget] CLI limit {}", fmt_hm(m));
            Some(m)
        }
        None => {
            println!(
                "[time-budget] could not parse the --time-budget value; ignored (examples: 40 / 1.5h / 90m / half an hour)"
            );
            None
        }
    }
}

fn budget_label(src: BudgetSource, minutes: f64) -> String {
    let v = rufous_kernel::text::fmt_g(minutes);
    match src {
        BudgetSource::Prompt => format!("time limit of {v} min found in the prompt"),
        BudgetSource::Cli => format!("CLI time limit {v} min"),
        BudgetSource::Env => format!("environment time limit {v} min"),
        BudgetSource::Off => format!("{v} min"),
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
    let mut params = RunParams::from_env(model, &workdir)?;
    let (policy, skills) = product(&opts.policy);
    params.policy = policy;
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

    ensure_engine(cfg);
    let notice = params.decider_notice(&cfg.decider);
    let mut agent = Agent::standard(params, cfg)?
        .with_sink(print_event)
        .with_builtin_skills(skills);
    if let Some(n) = notice {
        println!("[decider] {n}");
    }
    install_ctrl_c(agent.stop_flag());
    println!("[engine] {}", agent.engine);
    let sdir = sessions_dir(&agent_home());
    let session = opts.session.as_deref();
    if let Some(s) = session {
        agent.set_session_log(session::SessionLog::open(&sdir, s));
    }

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
            println!("[RESUMED: {} msgs + new message appended]", msgs.len());
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
        msgs.push(Message::user(
            "Continue the previous conversation: complete or answer the current request.",
        ));
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
        Err(rufous_kernel::Error::ChildTerminated(reason)) => {
            // The child sandbox fuse: the dispatcher reads exit 53 and falls back without a retry.
            println!("[child-mode] serious violation, circuit broken: {reason}");
            std::process::exit(SEVERE_EXIT_CODE);
        }
        Err(e) => Err(e.into()),
    }
}

/// `rufous chat`: the general-purpose harness at a terminal. The wiring is the CLI's (the
/// engine, the event printer, Ctrl-C); the policy, the prompt, the skills and the loop
/// itself are `rufous-general`'s.
fn chat(
    cfg: &Config,
    model: String,
    workdir: &Path,
    session: Option<String>,
    time_budget: Option<String>,
) -> Result<()> {
    let workdir = std::path::absolute(workdir)?;
    std::fs::create_dir_all(&workdir)?;
    let params = rufous_general::chat::params(model, &workdir)?;
    let cli_tb = cli_budget(time_budget.as_deref());
    ensure_engine(cfg);
    let notice = params.decider_notice(&cfg.decider);
    let mut agent = Agent::standard(params, cfg)?
        .with_sink(print_event)
        .with_builtin_skills(rufous_general::harness::skills());
    if let Some(n) = notice {
        println!("[decider] {n}");
    }
    install_ctrl_c(agent.stop_flag());
    println!("[engine] {}", agent.engine);
    rufous_general::chat::repl(
        &mut agent,
        &sessions_dir(&agent_home()),
        session.as_deref(),
        cli_tb.map(|m| m * 60.0),
    )
}

/// The skills built into every product, for the batch harness (`rufous run`), the bench,
/// `rufous skills` and `rufous doctor`: the binary that is every product sees every skill.
fn builtin_skills() -> Vec<Skill> {
    let mut skills = rufous_general::harness::skills();
    skills.extend(rufous_coding::harness::skills());
    skills
}

/// Ctrl-C: the first press raises the agent's stop flag, so the loop ends after the current
/// step with the session saved, a running command's process tree is terminated and
/// dispatched children are killed (they run in their own process groups, which the
/// terminal's signal does not reach). A second press exits at once.
fn install_ctrl_c(stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        rt.block_on(async move {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            stop.store(true, Ordering::SeqCst);
            eprintln!(
                "\n[stop] Ctrl-C: ending after the current step and saving the session (press again to quit now)"
            );
            let _ = tokio::signal::ctrl_c().await;
            std::process::exit(130);
        });
    });
}

/// Mingbird's stdout protocol: streamed text as is, harness lines verbatim.
/// Start-up engine check (`main()`): an OpenAI-compatible engine is only probed (we cannot
/// start someone else's server); Ollama is started when it is down (`ensure_ollama`); the
/// cloud provider needs neither.
fn ensure_engine(cfg: &Config) {
    if let Some(eng) = cfg.backend_openai() {
        if !openai::probe_blocking(
            &eng.base_url,
            &eng.api_key,
            std::time::Duration::from_secs(3),
        ) {
            println!(
                "⚠️ {} not ready at {} — start it first, then retry.",
                eng.label, eng.base_url
            );
        }
    } else if cfg.cloud_provider().is_none()
        && !OllamaProc::from_config(cfg).ensure(std::time::Duration::from_secs(25))
    {
        println!("⚠️ could not start ollama automatically; run ollama serve yourself, then retry.");
    }
}

fn print_event(ev: AgentEvent) {
    match ev {
        AgentEvent::Text(t) => {
            print!("{t}");
            let _ = std::io::stdout().flush();
        }
        // Thinking is dimmed so it reads apart from the answer in the terminal.
        AgentEvent::Thinking(t) => {
            print!("\x1b[2m{t}\x1b[0m");
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

/// `rufous decide`: heuristic and model verdicts per input line, for the offline comparison
/// in `docs/system-one-decider.md` section 5.
fn decide(
    cfg: &Config,
    file: Option<&std::path::Path>,
    model: Option<&str>,
    timeout_ms: u64,
) -> Result<()> {
    use rufous_kernel::decider::{intent_question, intent_state, OllamaSystemOne};
    use rufous_kernel::guards::qa::intent_heuristic;
    use rufous_kernel::ollama::{Message, SystemOneAnswer};
    use std::io::{BufRead, Write};

    let input: Box<dyn BufRead> = match file {
        Some(p) => Box::new(std::io::BufReader::new(
            std::fs::File::open(p).with_context(|| format!("opening {}", p.display()))?,
        )),
        None => Box::new(std::io::stdin().lock()),
    };
    let backend = model.map(|m| {
        OllamaSystemOne::new(
            cfg.ollama_host(),
            m,
            std::time::Duration::from_millis(timeout_ms.max(1)),
        )
    });
    let mut out = std::io::stdout().lock();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let case: serde_json::Value = serde_json::from_str(&line).context("case JSON")?;
        let text = case["text"].as_str().unwrap_or("").to_string();
        let has_progress = case["has_progress"].as_bool().unwrap_or(false);
        let h = intent_heuristic(&[Message::user(&text)], has_progress);
        let mut row = serde_json::json!({
            "text": text,
            "has_progress": has_progress,
            "heuristic": {"intent": h.intent.as_str(), "confidence": h.confidence, "rule": h.rule},
        });
        if let Some(b) = &backend {
            let mut qs = std::collections::BTreeMap::new();
            qs.insert("intent".to_string(), intent_question());
            let t = std::time::Instant::now();
            row["model"] = match b.ask(&intent_state(&h), qs) {
                Ok(a) => match a.get("intent") {
                    Some(SystemOneAnswer::Choice {
                        choice,
                        probabilities,
                        confidence,
                    }) => serde_json::json!({
                        "intent": choice,
                        "p": probabilities.get(choice).copied().unwrap_or(0.0),
                        "probabilities": probabilities,
                        "confidence": confidence,
                        "ms": t.elapsed().as_millis(),
                    }),
                    other => serde_json::json!({"error": format!("unexpected answer {other:?}")}),
                },
                Err(e) => serde_json::json!({"error": e, "ms": t.elapsed().as_millis()}),
            };
        }
        writeln!(out, "{row}")?;
    }
    Ok(())
}

async fn doctor(cfg: &Config) -> Result<()> {
    let home = agent_home();
    println!("rufous {}", rufous_kernel::VERSION);
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
    println!(
        "engine       : {}{}",
        cfg.engine_display_name(),
        match (cfg.cloud_provider(), cfg.backend_openai()) {
            (Some(cp), _) => format!(" (cloud enabled: {} @ {})", cp.model, cp.base_url),
            (None, Some(e)) => format!(" @ {}", e.base_url),
            _ => String::new(),
        }
    );
    if let Some(eng) = cfg.backend_openai() {
        let up = openai::probe(
            &eng.base_url,
            &eng.api_key,
            std::time::Duration::from_secs(3),
        )
        .await;
        println!("openai engine: {}", if up { "ok" } else { "unreachable" });
    }

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
                        let think = match model_can_think(cfg, &m.name) {
                            Some(true) => "thinking",
                            Some(false) => "-",
                            None => "?",
                        };
                        println!(
                            "  {:<38} {:>8}  {think}",
                            cfg.display_name(&m.name),
                            human_size(m.size)
                        );
                    }
                }
                Err(e) => println!("models       : error: {e}"),
            }
        }
        Err(e) => println!("unreachable ({e})"),
    }

    match cfg.decider.mode {
        rufous_kernel::decider::DeciderMode::Off => println!(
            "decider      : off (config `decider.mode`: shadow | on; model {})",
            cfg.decider.model
        ),
        mode => {
            if let Some(n) = cfg.decider.notice() {
                println!("decider      : {n}");
            }
            let b = rufous_kernel::decider::OllamaSystemOne::new(
                cfg.ollama_host(),
                cfg.decider.model.clone(),
                std::time::Duration::from_millis(cfg.decider.timeout_ms.max(1)),
            );
            let h = rufous_kernel::guards::qa::intent_heuristic(
                &[rufous_kernel::ollama::Message::user(
                    "Are you capable of software development ?",
                )],
                false,
            );
            let mut qs = std::collections::BTreeMap::new();
            qs.insert(
                "intent".to_string(),
                rufous_kernel::decider::intent_question(),
            );
            let t = std::time::Instant::now();
            match b.ask(&rufous_kernel::decider::intent_state(&h), qs) {
                Ok(a) => println!(
                    "decider      : {mode:?} · {} ok ({} ms) probe answer: {:?}",
                    cfg.decider.model,
                    t.elapsed().as_millis(),
                    a.get("intent")
                ),
                Err(e) => println!(
                    "decider      : {mode:?} · {} unavailable ({e}); heuristics stand",
                    cfg.decider.model
                ),
            }
        }
    }

    let skills = SkillSet::standard(&std::env::current_dir()?, &home, &install_dir())
        .with_builtin(builtin_skills());
    println!("skills       : {}", skills.len());

    {
        let cwd = std::env::current_dir()?;
        let project = mcp::load_project(&cwd).unwrap_or_default().len();
        let servers = mcp::load_for(&home, &install_dir(), &cwd);
        if project > 0 {
            println!(
                "mcp servers  : {} ({project} from {})",
                servers.len(),
                mcp::PROJECT_FILE
            );
        } else {
            println!("mcp servers  : {}", servers.len());
        }
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
    Ok(())
}

fn human_size(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    if bytes == 0 {
        return String::new();
    }
    format!("{:.1} GB", bytes as f64 / GB)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rufous_kernel::agent::{Gate, Gates};

    #[test]
    fn the_two_product_presets_differ_in_name_prompt_and_the_coding_gate() {
        // The CLI is the one crate that sees both products. Both presets are the kernel's
        // conversation under their own name and their own copy of the task prefill (the
        // same text, without the finish sentences since bead boubou-twe); since bead boubou-78s the coding preset alone holds a turn once when
        // a file the instruction asks to write got no write (the plan tool stood in for
        // tasks.md in the editor's tracker turns). General is not measured on that yet.
        let g = rufous_general::harness::policy();
        let c = rufous_coding::harness::policy();
        assert_eq!((g.name, c.name), ("general", "coding"));
        assert_eq!(
            TurnPolicy {
                name: c.name,
                prompt: c.prompt,
                gates: c.gates,
                ..g
            },
            c
        );
        // Since bead boubou-ear both hold a turn that ends with its own plan still open, and
        // since boubou-bcs one that claims its code works with nothing run to show it.
        assert_eq!(
            c.gates,
            Gates::NONE
                .with(Gate::NamedWrites)
                .with(Gate::PlanSync)
                .with(Gate::Claims)
        );
        assert_eq!(g.gates, Gates::NONE.with(Gate::PlanSync).with(Gate::Claims));
        assert_eq!(
            rufous_general::harness::TASK_PROMPT,
            rufous_coding::harness::TASK_PROMPT
        );
    }

    #[test]
    fn acp_policy_is_coding_unless_asked_for_general() {
        // `rufous acp` serves an editor by default; `--policy general` serves a desktop app
        // (Choreo) the web chat's preset; the batch harness is not offered over ACP.
        let parse = |args: &[&str]| match Cli::try_parse_from(args).map(|c| c.cmd) {
            Ok(Cmd::Acp { policy, .. }) => Ok(policy),
            Ok(_) => panic!("not acp"),
            Err(e) => Err(e.kind()),
        };
        assert_eq!(parse(&["rufous", "acp"]).as_deref(), Ok("coding"));
        assert_eq!(
            parse(&["rufous", "acp", "--policy", "general"]).as_deref(),
            Ok("general")
        );
        assert_eq!(
            parse(&["rufous", "acp", "--policy", "unattended"]),
            Err(clap::error::ErrorKind::InvalidValue)
        );
        assert_eq!(product("general").0, rufous_general::harness::policy());
    }

    #[test]
    fn acp_host_tools_follow_the_policy_unless_the_flag_says() {
        assert!(!acp_host_tools("coding", None));
        assert!(acp_host_tools("general", None));
        assert!(acp_host_tools("coding", Some("on")));
        assert!(!acp_host_tools("general", Some("off")));
        let parse = |args: &[&str]| match Cli::try_parse_from(args).map(|c| c.cmd) {
            Ok(Cmd::Acp { host_tools, .. }) => host_tools,
            _ => panic!("not acp"),
        };
        assert_eq!(parse(&["rufous", "acp"]), None);
        assert_eq!(
            parse(&["rufous", "acp", "--host-tools", "off"]).as_deref(),
            Some("off")
        );
    }

    #[test]
    fn run_policy_picks_the_product_s_preset_and_skills() {
        // `rufous run --policy coding` is an editor turn over the task file: the coding
        // preset and the coding skills only; `general` likewise; the default is the batch
        // harness with every skill.
        let (p, s) = product("coding");
        assert_eq!(p, rufous_coding::harness::policy());
        assert!(
            s.iter().any(|k| k.name == "bug_fix") && !s.iter().any(|k| k.name == "web_research")
        );
        let (p, s) = product("general");
        assert_eq!(p, rufous_general::harness::policy());
        assert!(
            s.iter().any(|k| k.name == "web_research") && !s.iter().any(|k| k.name == "bug_fix")
        );
        let (p, s) = product("unattended");
        assert!(p.is_unattended());
        assert_eq!(s.len(), 14);
    }

    #[test]
    fn the_batch_harness_sees_every_product_s_skills() {
        let names: Vec<String> = builtin_skills().into_iter().map(|s| s.name).collect();
        assert_eq!(names.len(), 14);
        assert!(names.contains(&"web_research".to_string()));
        assert!(names.contains(&"bug_fix".to_string()));
    }
}
