//! The general-purpose harness at a terminal (`rufous chat`): a read-eval-print loop over
//! the kernel's agent under the `general` policy, the same turn semantics as the web UI's
//! chat. The CLI builds the agent (engine, sink, Ctrl-C) and hands it here; this module
//! owns what the product does with it.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::atomic::Ordering;

use rufous_kernel::agent::session::{load_session, SessionLog};
use rufous_kernel::agent::{Agent, ChatBackend, RunParams, ToolRunner};
use rufous_kernel::ollama::Message;

use crate::harness;

/// The loop parameters for a terminal chat: the environment's defaults as for `rufous run`
/// (`AGENT_CTX` and the rest) under the `general` policy, so a text reply ends the turn,
/// `finish` is accepted as is, every turn takes the task prompt and the full tool set, and
/// nothing is dispatched.
pub fn params(model: String, workdir: &Path) -> rufous_kernel::Result<RunParams> {
    let mut params = RunParams::from_env(model, workdir)?;
    params.policy = harness::policy();
    Ok(params)
}

/// Read lines from stdin until `exit`, `quit` or end of input, running one turn per line.
/// `session` names the saved conversation to continue and to save to; a Ctrl-C that raised
/// the agent's stop flag ends the turn, not the conversation.
pub fn repl<B: ChatBackend, T: ToolRunner>(
    agent: &mut Agent<B, T>,
    sessions_dir: &Path,
    session: Option<&str>,
    budget_sec: Option<f64>,
) -> anyhow::Result<()> {
    let stop = agent.stop_flag();
    if let Some(s) = session {
        agent.set_session_log(SessionLog::open(sessions_dir, s));
    }
    let mut msgs = session
        .and_then(|s| load_session(sessions_dir, s))
        .unwrap_or_else(|| {
            let mut m = agent.task_messages("");
            m.pop();
            m
        });
    println!("[chat] type exit to quit.");
    let stdin = std::io::stdin();
    loop {
        print!("you> ");
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
        // A Ctrl-C ends the turn, not the conversation: the next prompt starts clean.
        stop.store(false, Ordering::SeqCst);
        msgs = agent.run(msgs, session, budget_sec)?;
        println!();
    }
    Ok(())
}
