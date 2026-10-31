//! Boubou kernel: the small-model agent loop and the product-neutral parts around it.
//!
//! The two products, `boubou-general` and `boubou-coding`, depend on this crate and set a
//! [`agent::TurnPolicy`]; this crate depends on neither (`docs/refactor.md`).
//! Rust successor to Mingbird (`ollama_agent.py`). Module map against the
//! Python original lives in `docs/PORTING.md`. Everything here is on-disk and
//! wire compatible with Mingbird: same `~/.ollama_agent` data directory, same
//! `config.json` / `mcp.json` formats, same skill files, same tool schemas.

pub mod agent;
pub mod config;
pub mod decider;
pub mod edit_match;
pub mod error;
pub mod exec;
pub mod guards;
pub mod hooks;
pub mod mcp;
pub mod memory;
pub mod ollama;
pub mod ollama_proc;
pub mod parallel;
pub mod profile;
pub mod safety;
pub mod skills;
pub mod text;
pub mod todo;
pub mod tools;
pub mod web;

pub use error::{Error, Result};

/// Crate version, surfaced by `boubou --version` and the GUI about box.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
