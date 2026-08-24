//! Handing the terminal to another program (`$EDITOR`, `$SHELL`) and getting
//! it back in the state it was in.

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use std::io;
use std::path::Path;

use super::super::{setup_terminal, teardown_terminal, Term};
use super::input::InputGate;
use super::state;

pub(super) fn apply_mouse_capture(enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        execute!(io::stdout(), DisableMouseCapture)
    }
}

/// What happened when the foreground program was run, once the terminal
/// itself is known to be back in a good state. Kept separate from
/// [`suspend_for`]'s `Err` case, which means the terminal restoration itself
/// failed and the caller can no longer trust `terminal` at all.
pub(super) enum EditorOutcome {
    Ok,
    /// The process couldn't run (a bad `$EDITOR` or `$SHELL`, typically);
    /// the terminal was still fully restored before this is returned.
    EditorFailed(io::Error),
}

/// `o` and `!`: suspend the alternate screen, raw mode, mouse capture and the
/// input thread (via [`InputGate::park`]), run the program to completion, then
/// restore all of it. The wait blocks, so probe and run events arriving
/// meanwhile sit in their channels until the next draw.
///
/// An `Err` means re-entering raw mode or the alternate screen failed and the
/// terminal is in whatever state that partial attempt produced; callers must
/// not keep drawing against `terminal`. See [`run`]'s call site.
pub(super) fn suspend_for(
    terminal: &mut Term,
    what: &state::Suspend,
    mouse_captured: bool,
    gate: &InputGate,
) -> io::Result<EditorOutcome> {
    gate.park();
    let outcome = (|| {
        if mouse_captured {
            apply_mouse_capture(false)?;
        }
        teardown_terminal()?;

        let spawn_result = match what {
            state::Suspend::Editor(path) => spawn_editor(path),
            state::Suspend::Shell(dir) => spawn_shell(dir),
        };

        *terminal = setup_terminal()?;
        if mouse_captured {
            apply_mouse_capture(true)?;
        }
        terminal.clear()?;

        Ok(match spawn_result {
            Ok(_) => EditorOutcome::Ok,
            Err(e) => EditorOutcome::EditorFailed(e),
        })
    })();
    gate.resume();
    outcome
}

/// `$EDITOR` (falling back to `vi`) on a path. The variable may carry flags
/// (`code -w`, `nvim -p`), so it is split rather than taken as one binary.
fn spawn_editor(path: &Path) -> io::Result<std::process::ExitStatus> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("vi");
    std::process::Command::new(bin)
        .args(parts)
        .arg(path)
        .status()
}

/// An interactive `$SHELL` (falling back to `sh`) in `dir`. `MR_REPO` is
/// exported the same way it is for an action's body, so a one-off command
/// typed here can refer to the repo the same way `.mrconfig` does.
fn spawn_shell(dir: &Path) -> io::Result<std::process::ExitStatus> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
    std::process::Command::new(shell)
        .current_dir(dir)
        .env("MR_REPO", dir)
        .status()
}
