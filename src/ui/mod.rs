//! Terminal front ends. `run` is the one-shot progress view; `app` is the
//! resident app. Both share the terminal setup below and the row renderer in
//! `widgets`.

pub mod app;
pub mod event;
pub mod render;
pub mod run;
pub mod state;
pub mod widgets;

use crossterm::{
    event::DisableMouseCapture,
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::io::{self, stdout, Stdout};

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Restore the terminal (raw mode off, alternate screen closed, mouse
/// capture released) before the default panic handler prints, so a panic
/// mid-render doesn't leave the user's terminal wrecked. Releasing mouse
/// capture is a no-op for the one-shot view, which never enables it.
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        original_hook(panic_info);
    }));
}

/// Enter raw mode and the alternate screen, returning a ready-to-draw terminal.
pub fn setup_terminal() -> io::Result<Term> {
    terminal::enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout()))
}

/// Leave the alternate screen and restore normal terminal input.
pub fn teardown_terminal() -> io::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// RAII guard over the terminal state the resident app enters: raw mode,
/// the alternate screen, and mouse capture. Without it, any `?` between
/// entering that state and the app's own teardown at the end of `run`
/// (including a library caller's own `ui::app::run(...).await?`) skips
/// cleanup and leaves the terminal wrecked (finding B2). `Drop` attempts
/// every restoration step regardless of whether an earlier one failed, so
/// one bad step can't skip the others; the installed panic hook remains
/// the belt to this guard's braces for an actual panic.
///
/// Disabling mouse capture or leaving the alternate screen when they were
/// never entered is harmless, so this deliberately doesn't track whether
/// each step actually ran before attempting to undo it.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Three independent `let _ =`, not one `execute!` with several
        // commands: a macro call stops at its first failing write, which is
        // exactly the "one failing step skips the others" this guard exists
        // to rule out.
        let _ = execute!(stdout(), DisableMouseCapture);
        let _ = execute!(stdout(), LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}
