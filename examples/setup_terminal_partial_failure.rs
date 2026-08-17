//! Fixture for `tests/ui_pty.rs`: breaks stdout before calling
//! `mrx::ui::setup_terminal`, so raw mode still enables (crossterm goes
//! through stdin's tty) but entering the alternate screen fails with a broken
//! pipe. Prints the outcome and exits.
//!
//! A pipe with no reader rather than a bare `close` on fd 1, because a closed
//! fd number is up for grabs: anything else opening a file in the meantime
//! (crossterm's own terminal-size query included) can reclaim it, and the
//! write meant to fail would land on that file instead.

use std::io;

// Declared here rather than taking a libc dependency: std already links the
// system C library on every Unix target.
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

fn main() {
    break_stdout();

    match mrx::ui::setup_terminal() {
        Ok(_) => eprintln!("setup_terminal unexpectedly succeeded with stdout broken"),
        Err(e) => eprintln!("setup_terminal failed as expected: {e}"),
    }
}
