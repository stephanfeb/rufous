//! `_run_test_guard`: before a `finish` is accepted in a workdir that has `test_*.py`, the
//! harness runs the tests itself. A missing interpreter is "cannot verify", not "failed".

use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::exec::{run_command, CommandRun};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestVerdict {
    Pass,
    Fail,
    /// No usable Python: not verifiable, do not block.
    Skip,
}

const TEST_ENV_ERR_MARKS: [&str; 3] = [
    "no module named pytest",
    "is not recognized as",
    "command not found",
];

/// Does the workdir contain `test_*.py` files?
pub fn has_python_tests(workdir: &Path) -> bool {
    std::fs::read_dir(workdir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with("test_") && n.ends_with(".py")
            })
        })
        .unwrap_or(false)
}

/// `_pytest_hint`: FAILED line, `File "...", line N`, and the first `E ` line.
pub fn pytest_hint(output: &str) -> String {
    static FILE_LINE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"File "([^"]+)", line (\d+)"#).expect("regex"));
    let lines: Vec<&str> = output.lines().collect();
    let mut hint = Vec::new();
    if let Some(l) = lines.iter().find(|l| l.starts_with("FAILED ")) {
        hint.push(crate::text::head(l[7..].trim(), 120).to_string());
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

/// `_test_env_broken`: exit 127/9009 or a "missing pytest/python" message.
fn env_broken(run: &CommandRun, out: &str) -> bool {
    if run.timed_out {
        return false;
    }
    if run.code == 127 || run.code == 9009 {
        return true;
    }
    let low = out.to_lowercase();
    TEST_ENV_ERR_MARKS.iter().any(|m| low.contains(m))
}

fn merged(run: &CommandRun) -> String {
    format!("{}\n{}", run.stdout, run.stderr)
}

/// Run `python -m pytest -q`, fall back to `python -m unittest discover`, skip when Python
/// itself is unavailable. Returns the verdict and the precise-failure hint.
pub fn run_test_guard(workdir: &Path) -> (TestVerdict, String) {
    let timeout = Duration::from_secs(300);
    let pr = run_command("python -m pytest -q", workdir, timeout, None);
    if pr.timed_out {
        return (
            TestVerdict::Fail,
            "pytest timed out (>300s; possibly an infinite loop or a test waiting for input)"
                .to_string(),
        );
    }
    let out = merged(&pr);
    if env_broken(&pr, &out) {
        let pr = run_command(
            "python -m unittest discover -p \"test_*.py\"",
            workdir,
            timeout,
            None,
        );
        if pr.timed_out {
            return (TestVerdict::Fail, "unittest timed out (>300s)".to_string());
        }
        let out = merged(&pr);
        if env_broken(&pr, &out) {
            return (TestVerdict::Skip, String::new());
        }
        if pr.code == 0 {
            return (TestVerdict::Pass, String::new());
        }
        return (TestVerdict::Fail, pytest_hint(&out));
    }
    if pr.code == 0 {
        return (TestVerdict::Pass, String::new());
    }
    (TestVerdict::Fail, pytest_hint(&out))
}
