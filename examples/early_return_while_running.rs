//! Fixture for `tests/ui_pty.rs`: enters the terminal state `ui::app::run`
//! does (raw mode, the alternate screen, mouse capture), then returns `Err`
//! from `main` rather than panicking. Deliberately installs no panic hook, so
//! only `TerminalGuard`'s `Drop` is left to restore the terminal.

use crossterm::event::EnableMouseCapture;
use crossterm::execute;

fn main() -> std::io::Result<()> {
    mrx::ui::setup_terminal().expect("enter raw mode and the alternate screen");
    let _guard = mrx::ui::TerminalGuard;
    execute!(std::io::stdout(), EnableMouseCapture).expect("enable mouse capture");
    Err(std::io::Error::other(
        "deliberate early return to exercise the terminal guard",
    ))
}
