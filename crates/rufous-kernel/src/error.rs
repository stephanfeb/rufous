use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("config: {0}")]
    Config(String),
    #[error("model server: {0}")]
    Ollama(String),
    /// The model server sent nothing for the read timeout (`AGENT_APITIMEOUT`): before the
    /// first chunk (`mid_reply` false: loading or processing the prompt) or after some
    /// (bead boubou-yqu).
    #[error("no reply from the model server within the read timeout ({})", if *mid_reply { "mid-reply" } else { "before the first token" })]
    Timeout { mid_reply: bool },
    /// Non-2xx reply from the model server, body kept for the loop's 4xx/5xx triage.
    #[error("HTTP {status}: {body}")]
    Status { status: u16, body: String },
    /// The agent loop stopped on purpose after saving its checkpoint (Mingbird `sys.exit(1)`).
    #[error("agent stopped: {0}")]
    Aborted(String),
    /// A child agent tripped the sandbox fuse; the process should exit with code 53
    /// (`parallel::SEVERE_EXIT_CODE`) so the dispatcher falls back without retrying.
    #[error("child sandbox fuse: {0}")]
    ChildTerminated(String),
    #[error("skill `{0}` not found")]
    SkillNotFound(String),
    #[error("skill file {path}: {reason}")]
    SkillFormat { path: PathBuf, reason: String },
    #[error("tool `{tool}`: {reason}")]
    ToolArgs { tool: String, reason: String },
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    #[error("not yet ported from Mingbird: {0}")]
    Unimplemented(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
