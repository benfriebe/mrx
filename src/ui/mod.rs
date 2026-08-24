//! Terminal front ends. `run` is the one-shot progress view; `app` is ui
//! mode. Both share the terminal setup below, and `output` for drawing
//! captured process output.

pub mod app;
pub mod event;
pub mod output;
pub mod render;
pub mod run;
pub mod state;
pub mod textarea;
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
/// mid-render doesn't leave the user's terminal wrecked.
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        original_hook(panic_info);
    }));
}

/// Enter raw mode and the alternate screen, returning a ready-to-draw
/// terminal. Rolls back whatever it already entered if a later step fails, so
/// an `Err` never leaves raw mode or the alternate screen engaged.
pub fn setup_terminal() -> io::Result<Term> {
    terminal::enable_raw_mode()?;
    if let Err(e) = execute!(stdout(), EnterAlternateScreen) {
        let _ = terminal::disable_raw_mode();
        return Err(e);
    }
    match Terminal::new(CrosstermBackend::new(stdout())) {
        Ok(term) => Ok(term),
        Err(e) => {
            let _ = execute!(stdout(), LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
            Err(e)
        }
    }
}

/// Leave the alternate screen and restore normal terminal input.
pub fn teardown_terminal() -> io::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// RAII guard over the terminal state ui mode enters: raw mode, the alternate
/// screen, and mouse capture. Without it any `?` between entering that state and
/// teardown at the end of `run` skips cleanup and leaves the terminal wrecked.
/// The panic hook remains the belt to this guard's braces.
///
/// Undoing a step that was never entered is harmless, so this does not track
/// which ones ran.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Three independent `let _ =`, not one `execute!` with several
        // commands: a macro call stops at its first failing write, so one bad
        // step would skip the others.
        let _ = execute!(stdout(), DisableMouseCapture);
        let _ = execute!(stdout(), LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}
