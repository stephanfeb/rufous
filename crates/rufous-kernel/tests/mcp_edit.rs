//! `rufous mcp add/remove` edits (bead boubou-r5f): what is written is what the loader reads,
//! the file keeps its shape and the keys rufous does not read, and a new server is not hidden
//! by the manifest cache.

use std::collections::BTreeMap;

use rufous_kernel::mcp::edit::{
    add_server, check_name, invalidate_manifest, raw_entry, remove_server, Scope, Transport,
};
use rufous_kernel::mcp::{load_from, CACHE_FILE};
use serde_json::{json, Value};

fn stdio(command: &str, args: &[&str]) -> Transport {
    Transport::Stdio {
        command: command.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        env: BTreeMap::new(),
    }
}

fn read(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

#[test]
fn an_added_stdio_server_is_what_the_loader_reads() {
    let d = tempfile::tempdir().expect("tmp");
    let path = Scope::User.path(d.path(), d.path());
    let mut env = BTreeMap::new();
    env.insert("TOKEN".to_string(), "x".to_string());
    let t = Transport::Stdio {
        command: "node".into(),
        args: vec!["/opt/framit/dist/cli.js".into(), "serve".into()],
        env,
    };
    assert!(!add_server(&path, None, "framit", &t, false).expect("add"));
    let servers = load_from(&path).expect("load");
    let s = &servers["framit"];
    assert_eq!(s.command.as_deref(), Some("node"));
    assert_eq!(s.args, ["/opt/framit/dist/cli.js", "serve"]);
    assert_eq!(s.env["TOKEN"], "x");
    // The user file is rufous's own bare map.
    assert!(read(&path).get("mcpServers").is_none());
}

#[test]
fn an_http_server_and_its_headers() {
    let d = tempfile::tempdir().expect("tmp");
    let path = d.path().join("mcp.json");
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_string(), "Bearer k".to_string());
    let t = Transport::Http {
        url: "https://mcp.example/mcp".into(),
        headers,
    };
    add_server(&path, None, "remote", &t, false).expect("add");
    let s = &load_from(&path).expect("load")["remote"];
    assert!(s.is_http());
    assert_eq!(s.headers["Authorization"], "Bearer k");
}

#[test]
fn a_new_project_file_uses_the_shared_wrapper_and_an_existing_shape_is_kept() {
    let d = tempfile::tempdir().expect("tmp");
    let project = Scope::Project.path(d.path(), d.path());
    add_server(&project, None, "framit", &stdio("node", &["cli.js"]), false).expect("add");
    let doc = read(&project);
    assert_eq!(doc["mcpServers"]["framit"]["type"], "stdio");
    assert_eq!(
        load_from(&project).expect("load")["framit"].args,
        ["cli.js"]
    );

    // A user file someone wrote in the wrapper shape, with keys rufous does not read.
    let user = d.path().join("mcp.json");
    std::fs::write(
        &user,
        json!({"mcpServers": {"old": {"command": "x", "disabled": true}}, "note": "mine"})
            .to_string(),
    )
    .expect("write");
    add_server(&user, None, "new", &stdio("y", &[]), false).expect("add");
    let doc = read(&user);
    assert_eq!(doc["note"], "mine");
    assert_eq!(doc["mcpServers"]["old"]["disabled"], true);
    assert_eq!(doc["mcpServers"]["new"]["command"], "y");
}

#[test]
fn a_name_in_use_is_refused_unless_forced() {
    let d = tempfile::tempdir().expect("tmp");
    let path = d.path().join("mcp.json");
    add_server(&path, None, "a", &stdio("one", &[]), false).expect("add");
    let err = add_server(&path, None, "a", &stdio("two", &[]), false).expect_err("duplicate");
    assert!(err.to_string().contains("--force"), "{err}");
    assert!(add_server(&path, None, "a", &stdio("two", &[]), true).expect("force"));
    assert_eq!(
        raw_entry(&path, "a").expect("get").expect("there")["command"],
        "two"
    );
}

#[test]
fn remove_takes_out_one_server_and_reports_a_missing_one() {
    let d = tempfile::tempdir().expect("tmp");
    let path = d.path().join("mcp.json");
    add_server(&path, None, "a", &stdio("one", &[]), false).expect("add");
    add_server(&path, None, "b", &stdio("two", &[]), false).expect("add");
    assert!(remove_server(&path, "a").expect("remove"));
    assert!(!remove_server(&path, "a").expect("again"));
    assert!(!remove_server(&d.path().join("absent.json"), "a").expect("no file"));
    let servers = load_from(&path).expect("load");
    assert!(!servers.contains_key("a") && servers.contains_key("b"));
}

#[test]
fn names_that_could_not_be_addressed_are_refused() {
    for ok in ["framit", "my-server", "s_2"] {
        assert!(check_name(ok).is_ok(), "{ok}");
    }
    for bad in ["", "a.b", "has space", "x/y"] {
        assert!(check_name(bad).is_err(), "{bad:?}");
    }
    let d = tempfile::tempdir().expect("tmp");
    let path = d.path().join("mcp.json");
    assert!(add_server(&path, None, "a.b", &stdio("x", &[]), false).is_err());
    assert!(!path.exists(), "nothing written for a refused name");
}

#[test]
fn a_new_user_file_keeps_the_install_directory_s_servers() {
    // `load` reads <install>/mcp.json only while the user file is absent; creating the user
    // file must not make those servers disappear.
    let home = tempfile::tempdir().expect("home");
    let install = tempfile::tempdir().expect("install");
    let seed = install.path().join("mcp.json");
    std::fs::write(&seed, json!({"shipped": {"command": "s"}}).to_string()).expect("seed");
    let path = Scope::User.path(home.path(), home.path());
    add_server(&path, Some(&seed), "mine", &stdio("m", &[]), false).expect("add");
    let servers = load_from(&path).expect("load");
    assert!(servers.contains_key("shipped") && servers.contains_key("mine"));
}

#[test]
fn the_manifest_cache_is_dropped() {
    let home = tempfile::tempdir().expect("home");
    let cache = home.path().join(CACHE_FILE);
    std::fs::write(&cache, "{}").expect("cache");
    invalidate_manifest(home.path());
    assert!(!cache.exists());
    invalidate_manifest(home.path()); // nothing to drop is fine
}
