pub mod event;
pub mod render;
pub mod spinner;
pub mod state;

use crossterm::{
    event::KeyCode,
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use state::{AppState, RepoStatus};
use std::io::{self, stdout};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::cli::Command;
use crate::config::Repo;
use crate::executor::TaskEvent;
use crate::summarize;

pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));
}

pub fn run(
    repos: Vec<Repo>,
    command: &Command,
    mut rx: mpsc::UnboundedReceiver<TaskEvent>,
) -> io::Result<bool> {
    install_panic_hook();

    terminal::enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState::new(repos, command.display_name());
    let mut all_succeeded = true;

    loop {
        // Drain pending events from executor
        loop {
            match rx.try_recv() {
                Ok(evt) => apply_event(&mut state, &evt, command),
                Err(_) => break,
            }
        }

        // Check if all done
        state.all_done = state.done_count() == state.total();

        // Render
        terminal.draw(|frame| render::draw(frame, &state))?;

        // Handle input
        if let Some(app_event) = event::poll(Duration::from_millis(80)) {
            match app_event {
                event::AppEvent::Key(code, modifiers) => {
                    // Ctrl+C always quits
                    if modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                        && code == KeyCode::Char('c')
                    {
                        break;
                    }

                    if state.expanded.is_some() {
                        // Expanded mode keys
                        match code {
                            KeyCode::Esc | KeyCode::Enter => state.collapse(),
                            KeyCode::Up | KeyCode::Char('k') => state.scroll_up(),
                            KeyCode::Down | KeyCode::Char('j') => {
                                let max = state
                                    .expanded_content()
                                    .map(|c| c.lines().count())
                                    .unwrap_or(0);
                                state.scroll_down(max);
                            }
                            KeyCode::Char('q') => break,
                            _ => {}
                        }
                    } else {
                        // Normal mode keys
                        match code {
                            KeyCode::Char('q') => break,
                            KeyCode::Up | KeyCode::Char('k') => state.move_up(),
                            KeyCode::Down | KeyCode::Char('j') => state.move_down(),
                            KeyCode::Enter => state.toggle_expand(),
                            KeyCode::Home | KeyCode::Char('g') => state.selected = 0,
                            KeyCode::End | KeyCode::Char('G') => {
                                state.selected = state.total().saturating_sub(1)
                            }
                            _ => {}
                        }
                    }
                }
                event::AppEvent::Tick => {
                    state.tick += 1;
                }
            }
        }
    }

    // Cleanup
    terminal::disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;

    // Print final summary
    let failed = state.failed_count();
    let done = state.done_count();
    let total = state.total();
    if failed > 0 {
        eprintln!(
            "mrx {}: {}/{} done, {} failed",
            state.command_name, done, total, failed
        );
        all_succeeded = false;
    } else {
        eprintln!("mrx {}: {}/{} done", state.command_name, done, total);
    }

    Ok(all_succeeded)
}

fn apply_event(state: &mut AppState, event: &TaskEvent, command: &Command) {
    match event {
        TaskEvent::Started { index } => {
            state.statuses[*index] = RepoStatus::Running;
        }
        TaskEvent::Finished {
            index,
            stdout,
            stderr,
            exit_code,
        } => {
            let summary = summarize::summarize(command, stdout, stderr, *exit_code);
            state.statuses[*index] = RepoStatus::Done {
                summary,
                stdout: stdout.clone(),
                stderr: stderr.clone(),
                exit_code: *exit_code,
            };
        }
        TaskEvent::Skipped { index, reason } => {
            state.statuses[*index] = RepoStatus::Skipped {
                reason: reason.clone(),
            };
        }
    }
}
