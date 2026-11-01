//! Finish gate 2: the turn's own verification must hold at finish.
//!
//! Design (2026-10-30, replacing Mingbird's `_run_test_guard`). The finish gate verifies
//! what the turn itself verified. The loop records every shell command and file write with
//! its exit code (`guards::integrity::CommandRecord`), and the claims gate already reads that
//! record to refuse a summary that claims a result nothing backs or that contradicts a
//! failed check, and to let an honest failure report through. The one case the record
//! cannot settle is a check that passed before later file changes. For that case the gate
//! re-executes the model's own command through the same tool path and takes the new exit
//! code: a pass becomes a fresh record and the finish stands, a failure is refused with the
//! output, quoting the command. When the turn changed files in a workdir that has tests and
//! ran no check at all, the gate asks once for one and names no runner. It never chooses a
//! test command, never falls back, and does nothing when the last check failed, because
//! honesty about failure is the claims gate's business.
//!
//! The ported guard guessed a runner from the filesystem (`python -m pytest -q`, then
//! `unittest discover`) and read any non-zero exit as a failing suite; on test files of plain
//! functions with their own `__main__` runner it refused a green suite three times per task
//! (bench 2026-10-30, bd boubou-j5s).

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::guards::integrity::{self, CommandRecord};

/// What the gate does at this finish, from the turn's record alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishCheck {
    /// Nothing to do: no verification is stale, and either nothing changed, there are no
    /// tests, or the last check failed (the claims gate's case).
    Nothing,
    /// Files changed in a workdir that has tests, and no check command ran this turn.
    NoCheckRan,
    /// This check passed and files changed after it: re-execute it.
    Rerun(String),
    /// This check passed and files changed after it, but it is not safe to re-execute
    /// (compound or writing command); the claims gate's stale rule stands.
    NotRerunnable(String),
}

/// Decide the gate's action from the record.
pub fn finish_check(workdir: &Path, cmds: &[CommandRecord]) -> FinishCheck {
    let Some(i) = cmds.iter().rposition(CommandRecord::is_verification) else {
        let changed = cmds.iter().any(CommandRecord::mutates);
        return if changed && has_tests(workdir) {
            FinishCheck::NoCheckRan
        } else {
            FinishCheck::Nothing
        };
    };
    let last = &cmds[i];
    if !last.succeeded() {
        return FinishCheck::Nothing;
    }
    if !cmds[i + 1..].iter().any(CommandRecord::mutates) {
        return FinishCheck::Nothing;
    }
    if integrity::rerunnable_verification(&last.command) {
        FinishCheck::Rerun(last.command.clone())
    } else {
        FinishCheck::NotRerunnable(last.command.clone())
    }
}

/// The re-executed check's result, read from the tool's output the way the model reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RerunOutcome {
    Passed,
    Failed {
        exit: i32,
        hint: String,
    },
    /// No exit code, or the program was not found: the harness learnt nothing.
    CouldNotRun,
}

pub fn rerun_outcome(result: &str) -> RerunOutcome {
    match integrity::exit_code_of(result) {
        Some(0) => RerunOutcome::Passed,
        Some(127) | Some(9009) | None => RerunOutcome::CouldNotRun,
        Some(exit) => RerunOutcome::Failed {
            exit,
            hint: pytest_hint(result),
        },
    }
}

/// Does the workdir carry tests? Top-level `test_*`, `*_test.*`, `*.test.*`, `*.spec.*`
/// files or a `test`/`tests`/`spec`/`__tests__` directory. Which runner they need is not the
/// gate's business.
pub fn has_tests(workdir: &Path) -> bool {
    std::fs::read_dir(workdir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                let n = e.file_name().to_string_lossy().to_lowercase();
                if e.path().is_dir() {
                    return matches!(n.as_str(), "test" | "tests" | "spec" | "__tests__");
                }
                n.starts_with("test_")
                    || n.contains("_test.")
                    || n.contains(".test.")
                    || n.contains(".spec.")
            })
        })
        .unwrap_or(false)
}

/// `_pytest_hint`: the FAILED (pytest) or FAIL (a plain runner) line, `File "...", line N`,
/// and the first `E ` line; else the output's last line.
pub fn pytest_hint(output: &str) -> String {
    static FILE_LINE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"File "([^"]+)", line (\d+)"#).expect("regex"));
    let lines: Vec<&str> = output.lines().collect();
    let mut hint = Vec::new();
    if let Some(l) = lines
        .iter()
        .find(|l| l.starts_with("FAILED ") || l.starts_with("FAIL "))
    {
        let rest = l.split_once(' ').map(|(_, r)| r).unwrap_or("");
        hint.push(crate::text::head(rest.trim(), 120).to_string());
    }
    if let Some(c) = lines.iter().find_map(|l| FILE_LINE.captures(l)) {
        let base = Path::new(&c[1])
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        hint.push(format!("at {}:{}", base, &c[2]));
    }
    if let Some(l) = lines.iter().find(|l| l.starts_with("E ")) {
        hint.push(crate::text::head(l[2..].trim(), 160).to_string());
    }
    if !hint.is_empty() {
        return hint.join(" | ");
    }
    crate::text::head(lines.last().copied().unwrap_or("").trim(), 120).to_string()
}
