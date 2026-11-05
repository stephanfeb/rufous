//! `kill_live_groups` kills every group registered in the process (docs/sandboxed-helper.md),
//! so its test has a binary of its own: run beside the other exec tests it killed their
//! running commands too.

#[cfg(unix)]
#[test]
fn live_groups_are_killed_with_everything_they_started() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let mut child = std::process::Command::new("sh")
        .args(["-c", "sleep 30 & wait"])
        .process_group(0)
        .spawn()
        .expect("spawn");
    let live = rufous_kernel::exec::LiveGroup::register(child.id());
    rufous_kernel::exec::kill_live_groups();
    let st = child.wait().expect("wait");
    assert_eq!(st.signal(), Some(9), "{st:?}");
    drop(live);
}
