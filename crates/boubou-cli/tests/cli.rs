//! The `boubou` binary end to end, without a model: help, tool schemas, skills, the
//! capacity plan, direct tool calls, voice/run error paths. Every run points `BOUBOU_HOME`
//! at a scratch directory and `OLLAMA_HOST` at a closed port so nothing on the machine is
//! touched or started.

use std::path::Path;
use std::process::{Command, Output};

fn boubou(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_boubou"))
        .args(args)
        .current_dir(cwd)
        .env("BOUBOU_HOME", home)
        .env("OLLAMA_HOST", "http://127.0.0.1:9")
        .env("OLLAMA_BIN", "/nonexistent/ollama")
        .env("AGENT_PARALLEL", "0")
        .env_remove("AGENT_OFFLINE")
        .output()
        .expect("run boubou")
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
    let o = boubou(d.path(), d.path(), &["--help"]);
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
    let o = boubou(d.path(), d.path(), &["--version"]);
    assert!(text(&o).contains(boubou_core::VERSION));
}

#[test]
fn tools_prints_core_schemas_and_named_advanced_ones() {
    let d = tempfile::tempdir().expect("tmp");
    let o = boubou(d.path(), d.path(), &["tools"]);
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
    let o = boubou(d.path(), d.path(), &["tools", "--enable", "web_fetch"]);
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
    let o = boubou(d.path(), &ws, &["skills", ws.to_str().expect("utf8")]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("my_skill"), "{}", text(&o));
}

#[test]
fn plan_reports_capacity_without_spawning() {
    let d = tempfile::tempdir().expect("tmp");
    let o = boubou(d.path(), d.path(), &["plan"]);
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
    let o = boubou(
        d.path(),
        &ws,
        &["tool", "read_file", r#"{"path":"hello.txt"}"#],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("hi there"), "{}", text(&o));
    let o = boubou(d.path(), &ws, &["tool", "list_dir", r#"{"path":"."}"#]);
    assert!(text(&o).contains("hello.txt"));
}

#[test]
fn run_with_missing_task_file_fails_before_touching_the_engine() {
    let d = tempfile::tempdir().expect("tmp");
    let ws = d.path().join("ws");
    let o = boubou(
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
    let silent = boubou_voice::encode_wav16(&boubou_voice::Pcm {
        samples: vec![0.3; 16_000],
        sample_rate: 16_000,
    });
    let wav = d.path().join("a.wav");
    std::fs::write(&wav, silent).expect("write");
    let o = Command::new(env!("CARGO_BIN_EXE_boubou"))
        .args(["voice", wav.to_str().expect("utf8"), "--lang", "en"])
        .env("BOUBOU_HOME", d.path())
        .env("BOUBOU_STT_DIR", d.path().join("nothing-here"))
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
    let o = boubou(d.path(), d.path(), &["voice", "/nonexistent.wav"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("nonexistent.wav"));
}

#[test]
fn doctor_runs_with_everything_unreachable() {
    let d = tempfile::tempdir().expect("tmp");
    let o = boubou(d.path(), d.path(), &["doctor"]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("engine       : Ollama"), "{t}");
    assert!(t.contains("unreachable"), "{t}");
    assert!(t.contains("skills"));
}
