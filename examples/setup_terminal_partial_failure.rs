//! Standalone fixture, not part of the CLI: repoints stdout at a pipe with
//! its read end already closed before calling `mrx::ui::setup_terminal`, so
//! raw mode enables fine (crossterm reads and writes terminal attributes
//! through stdin's tty, not stdout) but entering the alternate screen fails
//! with a broken pipe, since that writes an escape sequence to stdout.
//! Exercises `setup_terminal`'s own rollback of the raw mode it already
//! enabled before that later step failed; a pty test drives this and
//! confirms raw mode did not leak past the `Err` return.
//!
//! A pipe with no reader, rather than simply closing fd 1, because a closed
//! fd number is up for grabs: something else opening any file in the
//! meantime (crossterm's own terminal-size query included) can silently
//! reclaim it, and the write that was meant to fail would then succeed
//! against a whole different file instead. Holding fd 1 open against a
//! guaranteed-broken destination rules that out.

use std::io;

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

fn main() {
    break_stdout();

    match mrx::ui::setup_terminal() {
        Ok(_) => eprintln!("setup_terminal unexpectedly succeeded with stdout broken"),
        Err(e) => eprintln!("setup_terminal failed as expected: {e}"),
    }
}
