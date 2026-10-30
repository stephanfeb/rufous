//! Finish gate 2 (`agent::testguard`): decisions from the turn's own record, never from a
//! runner the harness picked. Design in the module doc; the bench case that drove it is bd
//! boubou-j5s (a green plain-function suite refused three times per task).

use boubou_kernel::agent::testguard::{
    finish_check, has_tests, pytest_hint, rerun_outcome, FinishCheck, RerunOutcome,
};
use boubou_kernel::guards::integrity::{rerunnable_verification, CommandRecord};

fn shell(cmd: &str, exit: i32) -> CommandRecord {
    CommandRecord::from_result(cmd, &format!("[exit {exit}]\nSTDOUT:\n..."))
}

fn edit(path: &str) -> CommandRecord {
    CommandRecord::file_write("edit_file", path, "[edited]")
}

#[test]
fn a_passing_check_followed_by_file_changes_is_rerun_and_nothing_else_is() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    // Fresh pass: the record is the evidence; no rerun.
    assert_eq!(
        finish_check(wd, &[edit("slug.py"), shell("python test_slug.py", 0)]),
        FinishCheck::Nothing
    );
    // Reads after a pass do not stale it.
    assert_eq!(
        finish_check(
            wd,
            &[shell("python test_slug.py", 0), shell("cat slug.py", 0)]
        ),
        FinishCheck::Nothing
    );
    // A write after a pass does: the model's own command is what gets re-executed.
    assert_eq!(
        finish_check(wd, &[shell("python test_slug.py", 0), edit("slug.py")]),
        FinishCheck::Rerun("python test_slug.py".to_string())
    );
    // The last check failed: the claims gate owns that (honest failure must pass).
    assert_eq!(
        finish_check(wd, &[shell("python test_slug.py", 1), edit("slug.py")]),
        FinishCheck::Nothing
    );
    // A stale pass whose command also writes is not re-executed by the harness.
    assert_eq!(
        finish_check(
            wd,
            &[shell("pytest -q && git commit -am wip", 0), edit("a.py")]
        ),
        FinishCheck::NotRerunnable("pytest -q && git commit -am wip".to_string())
    );
}

#[test]
fn changing_files_in_a_tested_workdir_without_any_check_is_asked_for_one() {
    let d = tempfile::tempdir().expect("tmp");
    let wd = d.path();
    // No tests here: nothing to ask for.
    assert_eq!(finish_check(wd, &[edit("notes.md")]), FinishCheck::Nothing);
    std::fs::write(wd.join("test_notes.py"), "def test_x():\n    pass\n").expect("w");
    assert_eq!(
        finish_check(wd, &[edit("notes.md")]),
        FinishCheck::NoCheckRan
    );
    // Only reads happened: nothing to verify.
    assert_eq!(
        finish_check(wd, &[shell("cat notes.md", 0)]),
        FinishCheck::Nothing
    );
    // A check that never ran (refused) is not a check.
    assert_eq!(
        finish_check(
            wd,
            &[
                edit("notes.md"),
                CommandRecord::from_result("pytest", "[safety gate] refused")
            ]
        ),
        FinishCheck::Nothing,
        "a refused check is a failed last check: the claims gate's case"
    );
}

#[test]
fn tests_are_recognised_by_name_not_by_runner() {
    let d = tempfile::tempdir().expect("tmp");
    assert!(!has_tests(d.path()));
    std::fs::write(d.path().join("app_test.go"), "").expect("w");
    assert!(has_tests(d.path()));
    let d2 = tempfile::tempdir().expect("tmp");
    std::fs::create_dir(d2.path().join("tests")).expect("mkdir");
    assert!(has_tests(d2.path()));
    let d3 = tempfile::tempdir().expect("tmp");
    std::fs::write(d3.path().join("testing_notes.md"), "").expect("w");
    assert!(
        !has_tests(d3.path()),
        "a name that merely starts with test is not a test file"
    );
}

#[test]
fn only_checks_that_cannot_change_anything_are_rerun_by_the_harness() {
    assert!(rerunnable_verification("python test_slug.py"));
    assert!(rerunnable_verification("python -m pytest -q 2>&1"));
    assert!(rerunnable_verification("cd src && cargo test"));
    assert!(!rerunnable_verification("pytest -q && git commit -am wip"));
    assert!(!rerunnable_verification("pytest -q > out.txt"));
    assert!(!rerunnable_verification("python test_x.py; rm -rf build"));
}

#[test]
fn the_rerun_s_outcome_is_read_from_the_tool_result() {
    assert_eq!(
        rerun_outcome("[exit 0]\nSTDOUT:\n6 passed, 0 failed"),
        RerunOutcome::Passed
    );
    assert_eq!(
        rerun_outcome("[exit 1]\nSTDOUT:\nFAIL test_bad: AssertionError()\n3 passed, 1 failed"),
        RerunOutcome::Failed {
            exit: 1,
            hint: "test_bad: AssertionError()".to_string()
        }
    );
    assert_eq!(
        rerun_outcome("[exit 127]\nSTDERR:\npython: command not found"),
        RerunOutcome::CouldNotRun
    );
    assert_eq!(
        rerun_outcome("[tool error: not scripted]"),
        RerunOutcome::CouldNotRun
    );
}

#[test]
fn the_hint_reads_pytest_and_plain_runner_output() {
    assert_eq!(
        pytest_hint("FAILED test_slug.py::test_bad - AssertionError\n1 failed, 1 passed\n"),
        "test_slug.py::test_bad - AssertionError"
    );
    assert_eq!(
        pytest_hint(
            "FAIL test_title_case_minor_words_middle: AssertionError()\n7 passed, 1 failed\n"
        ),
        "test_title_case_minor_words_middle: AssertionError()"
    );
    assert_eq!(
        pytest_hint("Traceback\n  File \"x.py\", line 3\nSyntaxError: bad"),
        "at x.py:3"
    );
}
