//! Boubou (布布) agent core.
//!
//! Rust successor to Mingbird (鸣鸟, `ollama_agent.py`). Module map against the
//! Python original lives in `docs/PORTING.md`. Everything here is on-disk and
//! wire compatible with Mingbird: same `~/.ollama_agent` data directory, same
//! `config.json` / `mcp.json` formats, same skill files, same tool schemas.

pub mod agent;
pub mod config;
pub mod error;
pub mod exec;
pub mod guards;
pub mod mcp;
pub mod ollama;
pub mod safety;
pub mod skills;
pub mod text;
pub mod todo;
pub mod tools;

pub use error::{Error, Result};

/// Crate version, surfaced by `boubou --version` and the GUI about box.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
