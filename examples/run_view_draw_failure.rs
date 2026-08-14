//! Standalone fixture, not part of the CLI: runs `ui::run::run` (the
//! one-shot progress view) against one repo left permanently "running", so
//! its spinner keeps every frame different and each `terminal.draw` keeps
//! actually writing, then repoints stdout at a pipe with its read end
//! already closed, from a background thread, once the terminal is known to
//! be up. The next `terminal.draw` inside the loop then fails with a broken
//! pipe, well after `setup_terminal` itself succeeded (a separate fixture
//! covers that failure). Exercises `run`'s own terminal guard, not
//! `setup_terminal`'s rollback: without the guard, this `Err` return would
//! leave raw mode and the alternate screen still active.
//!
//! A pipe with no reader, rather than simply closing fd 1: see
//! `setup_terminal_partial_failure.rs` for why a bare `close` isn't
//! dependable here.

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use mrx::config::Repo;
use mrx::executor::TaskEvent;

// FFI straight to the platform's C library, already linked by std on every
// Unix target; not a new crate dependency.
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

// `ui::run::run` is itself synchronous, but spawns the background repo
// probe onto a Tokio runtime internally; it needs one already running in
// the background, the same as it has under the real `mrx` binary.
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
    // Left "running" and never finished, so the row's spinner keeps
    // changing every tick and `terminal.draw` has something new to write
    // on every frame, including the one after stdout breaks below.
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
