//! The one-shot progress view: plan, execute, watch, quit.

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
    // `teardown_terminal()` below covers the normal quit path; this guard
    // covers every early `?` return from the loop, most notably a failed
    // `terminal.draw`.
    let _terminal_guard = super::TerminalGuard;

    let action = command.display_name().to_string();
    let mut state = AppState::new(repos, &action);
    let mut all_succeeded = true;

    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
    spawn_probe_for_all(&mut state, jobs, &probe_tx);

    loop {
        while let Ok(evt) = rx.try_recv() {
            apply_event(&mut state, &evt);
        }
        while let Ok(probed) = probe_rx.try_recv() {
            state.on_probe(probed.generation, probed.state);
        }

        state.all_done = state.done_count() == state.total();

        terminal.draw(|frame| render::draw(frame, &state))?;

        // Sticking around after the work finishes is the point of the TUI, so
        // leaving early is opt-in; otherwise the loop still waits for `q`.
        if state.all_done && exit_on_done {
            break;
        }

        if let Some(app_event) = event::poll(Duration::from_millis(80)) {
            match app_event {
                event::AppEvent::Key(code, modifiers) => {
                    if modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                        && code == KeyCode::Char('c')
                    {
                        break;
                    }

                    if state.expanded.is_some() {
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
                        match code {
                            KeyCode::Char('q') => break,
                            KeyCode::Up | KeyCode::Char('k') => state.move_up(),
                            KeyCode::Down | KeyCode::Char('j') => state.move_down(),
                            KeyCode::Enter => state.toggle_expand(),
                            KeyCode::Home | KeyCode::Char('g') => state.selected = 0,
                            KeyCode::End | KeyCode::Char('G') => {
                                state.selected = state.total().saturating_sub(1)
                            }
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

    super::teardown_terminal()?;

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
        // Never emitted here: the one-shot view opts out of streaming.
        TaskEvent::Output { .. } => {}
        TaskEvent::Finished {
            index,
            steps,
            exit_code,
        } => {
            let summary = summarize::summarize_steps(steps, *exit_code);
            // Stripped once here: `RepoStatus::Done` holds plain text.
            let stdout: String = steps
                .iter()
                .map(|s| crate::ansi::strip(&s.stdout))
                .collect();
            let stderr: String = steps
                .iter()
                .map(|s| crate::ansi::strip(&s.stderr))
                .collect();
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
