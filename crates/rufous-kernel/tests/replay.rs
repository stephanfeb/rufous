//! Replay corpus (bead boubou-bk9.13): tool calls real models made in real sessions, run
//! through the executor as regression data. Each `tests/replay/*.jsonl` line is one call:
//! `{"id", "tool", "args", "expect", "fixture"?}`; `{workdir}` in the arguments is the test's
//! temporary directory. `expect` is `"as_written"` (the shell gets the command unchanged and
//! unblocked), `"blocked"` (the gate refuses), or an object checked against `run_tool`'s
//! output: `starts_with`, `contains`, `not_contains`, `snapshots` (relative paths whose
//! backup copy must exist under the harness directory). `bench/replay_extract.py` writes the
//! extracted entries; the hand-written ones carry a fixture. A `"scope"` string arms the
//! scope gate with that instruction before the call (bead boubou-6x3); the gate's confirmer
//! here denies, so an out-of-scope call is `"blocked"`.

use std::path::Path;

use rufous_kernel::exec::Executor;
use rufous_kernel::safety::SafetyPolicy;
use rufous_kernel::tools::ToolInvocation;
use serde_json::Value;

fn subst(v: &Value, workdir: &str) -> Value {
    match v {
        Value::String(s) => Value::String(s.replace("{workdir}", workdir)),
        Value::Array(a) => Value::Array(a.iter().map(|x| subst(x, workdir)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), subst(x, workdir)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn strs<'a>(v: &'a Value, key: &str) -> Vec<&'a str> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn check(entry: &Value) -> Result<(), String> {
    let d = tempfile::tempdir().expect("tmp");
    let h = tempfile::tempdir().expect("tmp");
    let mut ex = Executor::with_policy(d.path(), SafetyPolicy::default());
    ex.harness_dir = h.path().to_path_buf();
    if let Some(fx) = entry.get("fixture") {
        if let Some(files) = fx.get("files").and_then(Value::as_object) {
            for (rel, body) in files {
                let p = d.path().join(rel);
                std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
                std::fs::write(&p, body.as_str().unwrap_or_default()).expect("write");
            }
        }
        for rel in strs(fx, "dirs") {
            std::fs::create_dir_all(d.path().join(rel)).expect("mkdir");
        }
    }
    if let Some(instruction) = entry.get("scope").and_then(Value::as_str) {
        ex.gate.set_scope(
            Some(rufous_kernel::scope::Scope::of(instruction)),
            rufous_kernel::scope::ScopeGateMode::On,
        );
    }
    let wd = d.path().to_str().expect("utf-8 tempdir");
    let tool = entry["tool"].as_str().ok_or("no tool")?;
    let args = subst(&entry["args"], wd);
    match &entry["expect"] {
        Value::String(s) if s == "as_written" => {
            let sent = args["command"]
                .as_str()
                .ok_or("as_written needs a command")?;
            match ex.prepare(tool, &args) {
                Ok(ToolInvocation::RunBash { command }) if command == sent => Ok(()),
                Ok(ToolInvocation::RunBash { command }) => Err(format!(
                    "the shell would get a changed command\n    sent: {sent}\n    got:  {command}"
                )),
                Ok(other) => Err(format!("not a run_bash invocation: {other:?}")),
                Err(e) => Err(format!("blocked: {e}")),
            }
        }
        Value::String(s) if s == "blocked" => match ex.prepare(tool, &args) {
            Err(_) => Ok(()),
            Ok(_) => Err("the gate let it through".to_string()),
        },
        Value::Object(exp) => {
            let out = ex.run_tool(tool, &args);
            let mut problems = Vec::new();
            if let Some(p) = exp.get("starts_with").and_then(Value::as_str) {
                if !out.starts_with(p) {
                    problems.push(format!("does not start with {p:?}"));
                }
            }
            for s in strs(&entry["expect"], "contains") {
                if !out.contains(s) {
                    problems.push(format!("missing {s:?}"));
                }
            }
            for s in strs(&entry["expect"], "not_contains") {
                if out.contains(s) {
                    problems.push(format!("contains {s:?}"));
                }
            }
            for rel in strs(&entry["expect"], "snapshots") {
                let bak = h.path().join("backups").join(format!("{rel}.bak"));
                if !bak.is_file() {
                    problems.push(format!("no backup at {}", bak.display()));
                }
            }
            if problems.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "{}\n    output: {}",
                    problems.join("; "),
                    out.replace('\n', "\n            ")
                ))
            }
        }
        other => Err(format!("unknown expect {other}")),
    }
}

#[test]
fn every_replay_entry_holds() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/replay");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/replay")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    let mut n = 0;
    let mut failures = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read corpus");
        for (ln, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let entry: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{}:{}: {e}", file.display(), ln + 1));
            n += 1;
            if let Err(msg) = check(&entry) {
                failures.push(format!(
                    "{}:{} {}: {msg}",
                    file.file_name().unwrap_or_default().to_string_lossy(),
                    ln + 1,
                    entry["id"].as_str().unwrap_or("?")
                ));
            }
        }
    }
    assert!(n > 0, "no replay entries under {}", dir.display());
    assert!(
        failures.is_empty(),
        "{} of {n} replay entries failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!(
        "[replay] {n} entries from {} corpus file(s) hold",
        files.len()
    );
}
