//! Fixture for `tests/ui_pty.rs`: runs the real `ui::app::run` (ui mode end
//! to end, input thread included), then breaks stdout from a background
//! thread once the app is up. `run`'s 200ms ticker keeps `terminal.draw`
//! firing regardless of repo state, so the next one fails with a broken pipe
//! and `run` returns `Err` from inside its select loop.
//!
//! A pipe with no reader rather than a bare `close` on fd 1: see
//! `setup_terminal_partial_failure.rs`.

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use mrx::config::Repo;
use mrx::ui::app::{session, RunOptions};

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

#[tokio::main]
async fn main() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(500));
        break_stdout();
    });

    let repos = vec![Repo {
        name: "fixture-repo".into(),
        path: std::env::temp_dir().join("app_run_draw_failure_repo"),
        clone_url: None,
        keys: BTreeMap::new(),
    }];

    let result = mrx::ui::app::run(RunOptions {
        repos,
        set_label: "fixture".into(),
        jobs: 1,
        defaults: BTreeMap::new(),
        config_path: std::env::temp_dir().join("app_run_draw_failure.mrconfig"),
        force: false,
        dir_override: None,
        session: session::Session::default(),
        result_ttl: None,
    })
    .await;

    match result {
        Ok(()) => eprintln!("run() unexpectedly returned Ok after stdout was closed"),
        Err(e) => eprintln!("run() returned Err as expected: {e}"),
    }
}
