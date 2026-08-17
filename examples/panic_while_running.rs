//! Fixture for `tests/ui_pty.rs`: enters every terminal mode ui mode can be
//! in (raw mode, the alternate screen, mouse capture), then panics with the
//! panic hook installed and no other teardown in the way.
use crossterm::event::EnableMouseCapture;
use crossterm::execute;

fn main() {
    mrx::ui::install_panic_hook();
    mrx::ui::setup_terminal().expect("enter raw mode and the alternate screen");
    execute!(std::io::stdout(), EnableMouseCapture).expect("enable mouse capture");
    panic!("deliberate panic to exercise the installed teardown hook");
}
