//! `rufous mcp add/remove` (bead boubou-r5f): edits to a server file, made on the file's JSON
//! rather than through [`ServerConfig`], so keys rufous does not read and the file's shape
//! (a bare map, or the shared `{"mcpServers": …}` wrapper) survive the edit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::{McpConfig, CACHE_FILE, PROJECT_FILE};
use crate::{Error, Result};

/// Which file an edit goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `<agent home>/mcp.json`: every session on this machine.
    User,
    /// `<workdir>/.mcp.json`: this project, in the format the other harnesses share.
    Project,
}

impl Scope {
    pub fn path(self, agent_home: &Path, workdir: &Path) -> PathBuf {
        match self {
            Scope::User => agent_home.join("mcp.json"),
            Scope::Project => workdir.join(PROJECT_FILE),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
        }
    }
}

/// How the server is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

/// A server name the rest of rufous can address: tools are named `server.tool` and split at
/// the first dot, so letters, digits, `-` and `_` only.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Config("an MCP server needs a name".into()));
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(Error::Config(format!(
            "MCP server name {name:?}: {c:?} is not allowed (letters, digits, '-' and '_' only)"
        )));
    }
    Ok(())
}

/// The file's JSON, `{}` when it does not exist yet.
fn read_doc(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => {
            let v: Value = serde_json::from_str(&text)
                .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
            if v.is_object() {
                Ok(v)
            } else {
                Err(Error::Config(format!(
                    "{}: not a JSON object",
                    path.display()
                )))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// The server map inside a document: the `mcpServers` object when the file has one, else the
/// document itself.
fn servers_mut(doc: &mut Value) -> &mut Map<String, Value> {
    let wrapped = doc.get("mcpServers").is_some_and(Value::is_object);
    let target = if wrapped {
        doc.get_mut("mcpServers").expect("checked above")
    } else {
        doc
    };
    target.as_object_mut().expect("read_doc returns objects")
}

fn entry(transport: &Transport, wrapped: bool) -> Value {
    let mut e = Map::new();
    match transport {
        Transport::Stdio { command, args, env } => {
            if wrapped {
                e.insert("type".into(), json!("stdio"));
            }
            e.insert("command".into(), json!(command));
            if !args.is_empty() {
                e.insert("args".into(), json!(args));
            }
            if !env.is_empty() {
                e.insert("env".into(), json!(env));
            }
        }
        Transport::Http { url, headers } => {
            if wrapped {
                e.insert("type".into(), json!("http"));
            }
            e.insert("url".into(), json!(url));
            if !headers.is_empty() {
                e.insert("headers".into(), json!(headers));
            }
        }
    }
    Value::Object(e)
}

/// Adds `name` to `path`. A new user file starts from `seed` (the install directory's
/// `mcp.json`, which `load` reads only while the user file is absent) so those servers stay;
/// a new project file uses the `mcpServers` wrapper. An existing name is an error unless
/// `force`. Returns whether an entry was replaced.
pub fn add_server(
    path: &Path,
    seed: Option<&Path>,
    name: &str,
    transport: &Transport,
    force: bool,
) -> Result<bool> {
    check_name(name)?;
    let mut doc = if path.exists() {
        read_doc(path)?
    } else if let Some(seed) = seed.filter(|s| s.exists()) {
        read_doc(seed)?
    } else if path.file_name().and_then(|n| n.to_str()) == Some(PROJECT_FILE) {
        json!({ "mcpServers": {} })
    } else {
        json!({})
    };
    let wrapped = doc.get("mcpServers").is_some_and(Value::is_object);
    let servers = servers_mut(&mut doc);
    let replaced = servers.contains_key(name);
    if replaced && !force {
        return Err(Error::Config(format!(
            "an MCP server named {name:?} is already in {} (use --force to replace it)",
            path.display()
        )));
    }
    servers.insert(name.to_string(), entry(transport, wrapped));
    crate::todo::atomic_write_json(path, &doc)?;
    Ok(replaced)
}

/// Removes `name` from `path`; `false` when the file or the name is not there.
pub fn remove_server(path: &Path, name: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut doc = read_doc(path)?;
    if servers_mut(&mut doc).remove(name).is_none() {
        return Ok(false);
    }
    crate::todo::atomic_write_json(path, &doc)?;
    Ok(true)
}

/// The servers configured in one file, `{}` when it is absent.
pub fn servers_in(path: &Path) -> Result<McpConfig> {
    super::load_from(path)
}

/// One server's entry exactly as the file has it.
pub fn raw_entry(path: &Path, name: &str) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut doc = read_doc(path)?;
    Ok(servers_mut(&mut doc).get(name).cloned())
}

/// Drops the probed tool manifest so the next session probes the servers as configured now;
/// the cache does not record which servers it was built from and would hide a new one for
/// up to [`super::MANIFEST_TTL`].
pub fn invalidate_manifest(agent_home: &Path) {
    let _ = std::fs::remove_file(agent_home.join(CACHE_FILE));
}
