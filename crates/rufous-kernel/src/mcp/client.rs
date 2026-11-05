//! The `rmcp` side: spawn (stdio) or connect (streamable HTTP) to a server, list its
//! tools, call one. Everything is synchronous for the agent loop: each operation runs on
//! its own small current-thread runtime with a hard timeout, so a hung `npx` launcher
//! never freezes the task (Mingbird's 2026-10-06 post-mortem).

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ClientConfig, Implementation};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::Value;

use super::ServerConfig;
use crate::text::head;

/// `_probe_stdio` / `_probe_http` timeout.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole `mcp_call` budget (spawn + handshake + call).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// The call itself, inside `CALL_TIMEOUT`.
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Chars of tool output passed back to the model.
const RESULT_CHARS: usize = 4000;

/// A tool as probed: name, description (150 chars) and compacted schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedTool {
    pub name: String,
    pub desc: String,
    pub schema: Value,
}

fn client_info() -> ClientConfig {
    let mut c = ClientConfig::default();
    c.client_info = Implementation::new("rufous", crate::VERSION);
    c
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

type Running = rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>;

/// Spawn or connect and finish the `initialize` handshake. The future is dropped on
/// timeout by the caller, which kills a spawned child.
async fn connect(cfg: &ServerConfig) -> Result<Running, String> {
    if let Some(url) = &cfg.url {
        let mut config =
            rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                url.as_str(),
            );
        for (k, v) in &cfg.headers {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| format!("header {k}: {e}"))?;
            let value = reqwest::header::HeaderValue::from_str(v)
                .map_err(|e| format!("header {k}: {e}"))?;
            config.custom_headers.insert(name, value);
        }
        let transport = StreamableHttpClientTransport::with_client(
            reqwest::Client::builder()
                .build()
                .map_err(|e| e.to_string())?,
            config,
        );
        return client_info()
            .serve(transport)
            .await
            .map_err(|e| e.to_string());
    }
    let command = cfg
        .command
        .as_deref()
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "server has neither command nor url".to_string())?;
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(&cfg.args).envs(cfg.env.iter()).kill_on_drop(true);
    // Servers chatter on stderr at startup; keep it off the agent's log (Mingbird: DEVNULL).
    let (child, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {command}: {e}"))?;
    client_info().serve(child).await.map_err(|e| e.to_string())
}

async fn list_probed(client: &Running) -> Result<Vec<ProbedTool>, String> {
    let tools = client.list_all_tools().await.map_err(|e| e.to_string())?;
    Ok(tools
        .into_iter()
        .map(|t| ProbedTool {
            name: t.name.to_string(),
            desc: head(t.description.as_deref().unwrap_or(""), super::HOST_DESC_CAP).to_string(),
            schema: super::compact_schema(&Value::Object((*t.input_schema).clone())),
        })
        .collect())
}

/// Let a dropped child's kill task run before the runtime goes away.
async fn settle() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// `_probe_server`: the tool list, or `None` on failure/timeout (distinct from no tools).
pub fn probe_server(cfg: &ServerConfig, timeout: Duration) -> Option<Vec<ProbedTool>> {
    let rt = runtime().ok()?;
    rt.block_on(async {
        let work = async {
            let client = connect(cfg).await?;
            let tools = list_probed(&client).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), client.cancel()).await;
            tools
        };
        let out = match tokio::time::timeout(timeout, work).await {
            Ok(Ok(t)) => Some(t),
            Ok(Err(e)) => {
                tracing::debug!("mcp probe failed: {e}");
                None
            }
            Err(_) => {
                tracing::debug!("mcp probe timed out after {timeout:?}");
                None
            }
        };
        settle().await;
        out
    })
}

/// `mcp_call(server, tool, args)` without the dispatcher's extras: the first text block of
/// the result (4000 chars), or a `[mcp_call error: …]` line.
pub fn call_tool(
    cfg: &ServerConfig,
    tool: &str,
    args: serde_json::Map<String, Value>,
    timeout: Duration,
) -> String {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return format!("[mcp_call error: {e}]"),
    };
    let tool = tool.to_string();
    rt.block_on(async {
        let work = async {
            let client = connect(cfg).await.map_err(|e| format!("[mcp_call error: {e}]"))?;
            let params = CallToolRequestParams::new(tool.clone()).with_arguments(args);
            let res = match tokio::time::timeout(TOOL_CALL_TIMEOUT, client.call_tool(params)).await {
                Ok(Ok(r)) => Ok(render_result(&r)),
                Ok(Err(e)) => Err(format!("[mcp_call error: {e}]")),
                Err(_) => Err(format!(
                    "[mcp_call error: the {tool} call timed out (60s). The server may be stuck; use another tool or retry later.]"
                )),
            };
            let _ = tokio::time::timeout(Duration::from_secs(2), client.cancel()).await;
            res
        };
        let out = match tokio::time::timeout(timeout, work).await {
            Ok(Ok(text)) | Ok(Err(text)) => text,
            Err(_) => "[mcp_call error: server start-up or handshake timed out (120s). This MCP server \
                       may be stuck or very slow to start; complete this step with another tool.]"
                .to_string(),
        };
        settle().await;
        out
    })
}

fn render_result(res: &rmcp::model::CallToolResult) -> String {
    let text = res
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_else(|| serde_json::to_string(&res.content).unwrap_or_default());
    head(&text, RESULT_CHARS).to_string()
}
