//! Fixture for `tests/ui_pty.rs`: runs `ui::run::run` (the one-shot progress
//! view), then breaks stdout from a background thread once the terminal is
//! up, so a `terminal.draw` well after `setup_terminal` succeeded fails with
//! a broken pipe and `run` returns `Err` from inside its draw loop.
//!
//! A pipe with no reader rather than a bare `close` on fd 1: see
//! `setup_terminal_partial_failure.rs`.

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use mrx::config::Repo;
use mrx::executor::TaskEvent;

extern "C" {
    fn pipe(fds: *mut i32) -> i32;
    fn close(fd: i32) -> i32;
    fn dup2(oldfd: i32, newfd: i32) -> i32;
}

/// Makes writes to stdout (fd 1) fail with a broken pipe from here on.
fn break_stdout() {
    let mut fds = [0i32; 2];
    // SAFETY: three plain libc calls building and installing a pipe with no
    // reader onto fd 1, checked at each step; nothing here touches memory
    // outside `fds`.
    unsafe {
        assert_eq!(
            pipe(fds.as_mut_ptr()),
            0,
            "pipe() failed: {}",
            io::Error::last_os_error()
        );
        assert_eq!(
            close(fds[0]),
            0,
            "closing the pipe's read end failed: {}",
            io::Error::last_os_error()
        );
        assert_eq!(
            dup2(fds[1], 1),
            1,
            "dup2 onto stdout failed: {}",
            io::Error::last_os_error()
        );
        assert_eq!(
            close(fds[1]),
            0,
            "closing the original pipe write end failed: {}",
            io::Error::last_os_error()
        );
    }
}

// `ui::run::run` is synchronous but spawns its repo probe onto an ambient
// Tokio runtime, so one has to already be running.
#[tokio::main]
async fn main() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(300));
        break_stdout();
    });

    let repos = vec![Repo {
        name: "fixture-repo".into(),
        path: std::env::temp_dir().join("run_view_draw_failure_repo"),
        clone_url: None,
        keys: BTreeMap::new(),
    }];
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // Never finished, so the spinner keeps every frame different and
    // `terminal.draw` still has something to write once stdout breaks.
    tx.send(TaskEvent::Started { index: 0 })
        .expect("send on a freshly created channel");

    let result = mrx::ui::run::run(
        repos,
        &mrx::cli::Command::Status,
        rx,
        1,
        &BTreeMap::new(),
        std::env::temp_dir().join("run_view_draw_failure.mrconfig"),
        false,
    );

    match result {
        Ok(_) => eprintln!("run() unexpectedly returned Ok after stdout was closed"),
        Err(e) => eprintln!("run() returned Err as expected: {e}"),
    }
}
