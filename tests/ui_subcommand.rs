//! Shell-level coverage for `mrx ui`'s invocation errors. These run the
//! built binary directly because they depend on real stdout tty-ness, which
//! `Cli::parse_from` in the unit tests can't exercise.

use std::process::Command;

#[test]
fn ui_with_stdout_not_a_tty_exits_2_with_a_usable_message() {
    // `Command::output` captures stdout/stderr, so the child never sees a tty,
    // the same condition as `mrx ui > /dev/null`.
    let output = Command::new(env!("CARGO_BIN_EXE_mrx"))
        .arg("ui")
        .output()
        .expect("failed to run mrx");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mrx status") || stderr.contains("mrx list"),
        "message should point at a non-interactive subcommand, got: {stderr}"
    );
}

#[test]
fn ui_plain_exits_2_with_a_usable_message() {
    let output = Command::new(env!("CARGO_BIN_EXE_mrx"))
        .args(["ui", "--plain"])
        .output()
        .expect("failed to run mrx");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--plain"),
        "message should mention --plain, got: {stderr}"
    );
}
