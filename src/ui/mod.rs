//! Terminal front ends. `run` is the existing one-shot progress view; a
//! resident app is expected to join it here later, sharing the setup below
//! and the row renderer in `widgets`.

pub mod event;
pub mod render;
pub mod run;
pub mod state;
pub mod widgets;

use crossterm::{
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::io::{self, stdout, Stdout};

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Restore the terminal (raw mode off, alternate screen closed) before the
/// default panic handler prints, so a panic mid-render doesn't leave the
/// user's terminal wrecked.
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
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
