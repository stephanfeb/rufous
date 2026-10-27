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
    #[error("ollama: {0}")]
    Ollama(String),
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
