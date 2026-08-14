//! The one-shot progress view: plan, execute, watch, quit. This is `mrx`'s
//! existing TUI, unchanged in behaviour after moving out of `tui/`.

use crossterm::event::KeyCode;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

use super::app::probe::{self, Probed};
use super::event;
use super::render;
use super::state::{AppState, RepoStatus};
use crate::cli::Command;
use crate::config::Repo;
use crate::executor::{self, TaskEvent};
use crate::operations;
use crate::summarize;

pub fn run(
    repos: Vec<Repo>,
    command: &Command,
    mut rx: mpsc::UnboundedReceiver<TaskEvent>,
    jobs: usize,
    defaults: &BTreeMap<String, String>,
    config_path: PathBuf,
    exit_on_done: bool,
) -> io::Result<bool> {
    super::install_panic_hook();
    let mut terminal = super::setup_terminal()?;

    let action = command.display_name().to_string();
    let mut state = AppState::new(repos, &action);
    let mut all_succeeded = true;

    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
    spawn_probe_for_all(&mut state, jobs, &probe_tx);

    loop {
        // Drain pending events from executor
        while let Ok(evt) = rx.try_recv() {
            apply_event(&mut state, &evt);
        }
        while let Ok(probed) = probe_rx.try_recv() {
            state.on_probe(probed.generation, probed.state);
        }

        // Check if all done
        state.all_done = state.done_count() == state.total();

        // Render
        terminal.draw(|frame| render::draw(frame, &state))?;

        // Sticking around after the work finishes is the point of the TUI, so this
        // is opt-in; without the flag the loop still waits for `q`.
        if state.all_done && exit_on_done {
            break;
        }

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
                            // Re-run the current command once the previous run finished.
                            KeyCode::Char('r') if state.all_done => {
                                let ops = state
                                    .repos
                                    .iter()
                                    .map(|r| operations::plan(command, r, defaults))
                                    .collect();
                                rx = executor::execute_all(
                                    &state.repos,
                                    ops,
                                    jobs,
                                    config_path.clone(),
                                );
                                state.reset_for_rerun();
                                spawn_probe_for_all(&mut state, jobs, &probe_tx);
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
    super::teardown_terminal()?;

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

/// Probe every repo, tagged with a freshly begun generation so an older
/// rerun's results can't land on top of a newer rerun's.
fn spawn_probe_for_all(state: &mut AppState, jobs: usize, tx: &mpsc::UnboundedSender<Probed>) {
    let targets: Vec<usize> = (0..state.repos.len()).collect();
    let generation = state.begin_probe();
    probe::spawn_probe_generation(&state.repos, targets, jobs, generation, tx.clone());
}

fn apply_event(state: &mut AppState, event: &TaskEvent) {
    match event {
        TaskEvent::Started { index } => {
            state.statuses[*index] = RepoStatus::Running;
        }
        // Which step is live doesn't have anywhere to show yet in this view;
        // the row keeps its fixed "pulling..."-style text until Finished.
        TaskEvent::Step { .. } => {}
        TaskEvent::Finished {
            index,
            steps,
            exit_code,
        } => {
            let summary = summarize::summarize_steps(steps, *exit_code);
            let stdout: String = steps.iter().map(|s| s.stdout.as_str()).collect();
            let stderr: String = steps.iter().map(|s| s.stderr.as_str()).collect();
            state.statuses[*index] = RepoStatus::Done {
                summary,
                stdout,
                stderr,
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
