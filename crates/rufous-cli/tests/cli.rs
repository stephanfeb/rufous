//! The `rufous` binary end to end, without a model: help, tool schemas, skills, the
//! capacity plan, direct tool calls, voice/run error paths. Every run points `RUFOUS_HOME`
//! at a scratch directory and `OLLAMA_HOST` at a closed port so nothing on the machine is
//! touched or started.

use std::path::Path;
use std::process::{Command, Output};

fn rufous(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rufous"))
        .args(args)
        .current_dir(cwd)
        .env("RUFOUS_HOME", home)
        .env("OLLAMA_HOST", "http://127.0.0.1:9")
        .env("OLLAMA_BIN", "/nonexistent/ollama")
        .env("AGENT_PARALLEL", "0")
        .env_remove("AGENT_OFFLINE")
        .output()
        .expect("run rufous")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn help_lists_every_subcommand() {
    let d = tempfile::tempdir().expect("tmp");
    let o = rufous(d.path(), d.path(), &["--help"]);
    assert!(o.status.success());
    let t = text(&o);
    for cmd in [
        "run", "chat", "models", "skills", "tools", "doctor", "plan", "voice", "serve", "tool",
    ] {
        assert!(
            t.contains(&format!("\n  {cmd} ")),
            "missing `{cmd}` in help:\n{t}"
        );
    }
    let o = rufous(d.path(), d.path(), &["--version"]);
    assert!(text(&o).contains(rufous_kernel::VERSION));
}

#[test]
fn tools_prints_core_schemas_and_named_advanced_ones() {
    let d = tempfile::tempdir().expect("tmp");
    let o = rufous(d.path(), d.path(), &["tools"]);
    assert!(o.status.success(), "{}", text(&o));
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    for core in [
        "create_file",
        "read_file",
        "edit_file",
        "run_bash",
        "todo",
        "skills",
        "enable_tools",
        "finish",
    ] {
        assert!(t.contains(&format!("\"{core}\"")), "{core} missing");
    }
    assert!(
        !t.contains("\"web_fetch\""),
        "advanced tools are off by default"
    );
    let o = rufous(d.path(), d.path(), &["tools", "--enable", "web_fetch"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("\"web_fetch\""));
}

#[test]
fn skills_lists_a_workspace_skill() {
    let d = tempfile::tempdir().expect("tmp");
    let ws = d.path().join("ws");
    std::fs::create_dir_all(ws.join("skills")).expect("mkdir");
    std::fs::write(
        ws.join("skills").join("my_skill.md"),
        "---\nname: my_skill\ndescription: a test skill\n---\n# body\n",
    )
    .expect("write");
    let o = rufous(d.path(), &ws, &["skills", ws.to_str().expect("utf8")]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("my_skill"), "{t}");
    // The skills built into the products are listed too: the binary carries them, with no
    // `skills/` directory beside it.
    assert!(t.contains("web_research:") && t.contains("bug_fix:"), "{t}");
}

#[test]
fn run_policy_takes_the_three_names_only() {
    // `--policy` is the bench's switch between the batch harness and a product's turn
    // policy; a name that is not one of the three is refused before anything starts.
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("task.txt"), "say hi").expect("write");
    let o = rufous(
        d.path(),
        d.path(),
        &["run", "m", "task.txt", ".", "--policy", "editor"],
    );
    assert!(!o.status.success());
    let t = text(&o);
    assert!(
        t.contains("invalid value 'editor'") && t.contains("coding"),
        "{t}"
    );
    let o = rufous(d.path(), d.path(), &["run", "--help"]);
    assert!(text(&o).contains("--policy <POLICY>"), "{}", text(&o));
}

#[test]
fn plan_reports_capacity_without_spawning() {
    let d = tempfile::tempdir().expect("tmp");
    let o = rufous(d.path(), d.path(), &["plan"]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("capacity plan"), "{t}");
    assert!(
        t.contains("max_parallel = 0"),
        "no child model configured → 0:\n{t}"
    );
    assert!(t.contains("enabled=false"));
}

#[test]
fn tool_runs_a_builtin_in_the_cwd() {
    let d = tempfile::tempdir().expect("tmp");
    let ws = d.path().join("ws");
    std::fs::create_dir_all(&ws).expect("mkdir");
    std::fs::write(ws.join("hello.txt"), "hi there\n").expect("write");
    let o = rufous(
        d.path(),
        &ws,
        &["tool", "read_file", r#"{"path":"hello.txt"}"#],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("hi there"), "{}", text(&o));
    let o = rufous(d.path(), &ws, &["tool", "list_dir", r#"{"path":"."}"#]);
    assert!(text(&o).contains("hello.txt"));
}

#[test]
fn run_with_missing_task_file_fails_before_touching_the_engine() {
    let d = tempfile::tempdir().expect("tmp");
    let ws = d.path().join("ws");
    let o = rufous(
        d.path(),
        d.path(),
        &[
            "run",
            "m",
            "/nonexistent/task.md",
            ws.to_str().expect("utf8"),
        ],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("task.md"), "{}", text(&o));
    assert!(
        !text(&o).contains("[engine]"),
        "must fail before the engine check"
    );
}

#[test]
fn voice_without_model_explains_how_to_install() {
    let d = tempfile::tempdir().expect("tmp");
    let silent = rufous_voice::encode_wav16(&rufous_voice::Pcm {
        samples: vec![0.3; 16_000],
        sample_rate: 16_000,
    });
    let wav = d.path().join("a.wav");
    std::fs::write(&wav, silent).expect("write");
    let o = Command::new(env!("CARGO_BIN_EXE_rufous"))
        .args(["voice", wav.to_str().expect("utf8"), "--lang", "en"])
        .env("RUFOUS_HOME", d.path())
        .env("RUFOUS_STT_DIR", d.path().join("nothing-here"))
        .env("OLLAMA_HOST", "http://127.0.0.1:9")
        .output()
        .expect("run");
    assert!(!o.status.success());
    let t = text(&o);
    assert!(
        t.contains("sherpa-onnx-streaming-zipformer-en")
            || t.contains("built_without_voice_feature"),
        "{t}"
    );
    let o = rufous(d.path(), d.path(), &["voice", "/nonexistent.wav"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("nonexistent.wav"));
}

#[test]
fn doctor_runs_with_everything_unreachable() {
    let d = tempfile::tempdir().expect("tmp");
    let o = rufous(d.path(), d.path(), &["doctor"]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("engine       : Ollama"), "{t}");
    assert!(t.contains("unreachable"), "{t}");
    assert!(t.contains("skills"));
}

#[test]
fn mcp_add_list_get_remove_round_trip() {
    // `rufous mcp add framit -- node …/cli.js serve`, as `claude mcp add` (bead boubou-r5f).
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let o = rufous(
        home.path(),
        cwd.path(),
        &[
            "mcp",
            "add",
            "-e",
            "TOKEN=x",
            "framit",
            "--",
            "node",
            "/opt/framit/dist/cli.js",
            "serve",
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    let user: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join("mcp.json")).expect("user file"),
    )
    .expect("json");
    assert_eq!(user["framit"]["command"], "node");
    assert_eq!(
        user["framit"]["args"],
        serde_json::json!(["/opt/framit/dist/cli.js", "serve"])
    );
    assert_eq!(user["framit"]["env"]["TOKEN"], "x");

    // Arguments that look like flags belong to the server's command.
    let o = rufous(
        home.path(),
        cwd.path(),
        &[
            "mcp",
            "add",
            "-s",
            "project",
            "-t",
            "http",
            "-H",
            "Authorization: Bearer k",
            "remote",
            "https://mcp.example/mcp",
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    let o = rufous(
        home.path(),
        cwd.path(),
        &["mcp", "add", "flags", "--", "srv", "--port", "-v"],
    );
    assert!(o.status.success(), "{}", text(&o));
    let o = rufous(home.path(), cwd.path(), &["mcp", "list"]);
    let out = text(&o);
    assert!(
        out.contains("framit: node /opt/framit/dist/cli.js serve"),
        "{out}"
    );
    assert!(out.contains("flags: srv --port -v"), "{out}");
    assert!(
        out.contains("remote: https://mcp.example/mcp (http)"),
        "{out}"
    );

    let o = rufous(home.path(), cwd.path(), &["mcp", "get", "remote"]);
    assert!(text(&o).contains("Bearer k"), "{}", text(&o));

    // A name in use is refused; a mismatched flag is refused before anything is written.
    let o = rufous(
        home.path(),
        cwd.path(),
        &["mcp", "add", "framit", "--", "other"],
    );
    assert!(
        !o.status.success() && text(&o).contains("--force"),
        "{}",
        text(&o)
    );
    let o = rufous(
        home.path(),
        cwd.path(),
        &["mcp", "add", "-H", "A: b", "x", "--", "cmd"],
    );
    assert!(!o.status.success(), "{}", text(&o));

    let o = rufous(home.path(), cwd.path(), &["mcp", "remove", "framit"]);
    assert!(o.status.success(), "{}", text(&o));
    let o = rufous(home.path(), cwd.path(), &["mcp", "get", "framit"]);
    assert!(!o.status.success(), "{}", text(&o));
}
