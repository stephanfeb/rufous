//! `boubou`: command-line entry point (counterpart of `ollama_agent.py`'s CLI).

use std::path::PathBuf;

use anyhow::{Context, Result};
use boubou_core::agent::{Agent, AgentEvent, RunParams};
use boubou_core::config::{agent_home, config_path, Config};
use boubou_core::ollama::Client;
use boubou_core::skills::SkillSet;
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
    /// Run a task file to completion.
    Run {
        model: String,
        taskfile: PathBuf,
        workdir: PathBuf,
        /// Session name (resume by default, like Mingbird).
        #[arg(long)]
        session: Option<String>,
        /// Start fresh instead of resuming a checkpoint.
        #[arg(long)]
        new: bool,
        /// Append the task to an existing conversation.
        #[arg(long)]
        append: bool,
    },
    /// Interactive chat in a workspace.
    Chat { model: String, workdir: PathBuf },
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
}

fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[tokio::main]
async fn main() -> Result<()> {
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
            let client = Client::new(cfg.ollama_host());
            for m in client.list_models().await.context("GET /api/tags")? {
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
        Cmd::Doctor => doctor(&cfg).await?,
        Cmd::Run {
            model,
            taskfile,
            workdir,
            ..
        } => {
            let task = std::fs::read_to_string(&taskfile)
                .with_context(|| format!("reading {}", taskfile.display()))?;
            let params = RunParams::from_env(model, workdir)?;
            let mut agent = Agent::new(params);
            let summary = agent.run(&task, &mut print_event).await?;
            println!("{summary}");
        }
        Cmd::Chat { .. } => {
            anyhow::bail!("interactive chat is not yet ported (see docs/PORTING.md)");
        }
    }
    Ok(())
}

fn print_event(ev: AgentEvent) {
    match ev {
        AgentEvent::Text(t) | AgentEvent::Thinking(t) => print!("{t}"),
        AgentEvent::ToolCall(c) => println!("\n> {}({})", c.function.name, c.function.arguments),
        AgentEvent::ToolResult {
            name,
            output,
            blocked,
        } => {
            println!(
                "< {name}{}: {output}",
                if blocked { " [blocked]" } else { "" }
            )
        }
        AgentEvent::Finished { summary } => println!("\n[finished] {summary}"),
        AgentEvent::Error(e) => eprintln!("[error] {e}"),
        AgentEvent::TodoUpdated(_)
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::Compacted { .. } => {}
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
            for (name, s) in &servers {
                let kind = if s.is_http() { "http" } else { "stdio" };
                let bad = s.unknown_categories();
                let warn = if bad.is_empty() {
                    String::new()
                } else {
                    format!("  ⚠ unrouted categories: {}", bad.join(", "))
                };
                println!("  {name:<20} {kind}{warn}");
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
