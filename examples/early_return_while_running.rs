//! Standalone fixture, not part of the CLI: enters the same terminal state
//! `ui::app::run` does (raw mode, the alternate screen, mouse capture), then
//! returns an `Err` early rather than panicking, the way a `?` deep inside
//! the run loop would. Deliberately does *not* install the panic hook, so a
//! pty test can confirm `TerminalGuard`'s `Drop` restores the terminal on
//! its own (finding B2), independent of the panic-hook safety net that
//! `panic_while_running` already covers.

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
