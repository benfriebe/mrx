//! Standalone fixture, not part of the CLI: enters every terminal mode the
//! resident app can be in (raw mode, the alternate screen, mouse capture)
//! and then panics, so a pty test can drive it and confirm the installed
//! panic hook actually restores all three before the app's own teardown
//! code would ever run.
use crossterm::event::EnableMouseCapture;
use crossterm::execute;

fn main() {
    mrx::ui::install_panic_hook();
    mrx::ui::setup_terminal().expect("enter raw mode and the alternate screen");
    execute!(std::io::stdout(), EnableMouseCapture).expect("enable mouse capture");
    panic!("deliberate panic to exercise the installed teardown hook");
}
