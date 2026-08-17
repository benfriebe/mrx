//! ui mode: a repo table that stays on screen across runs. Browse a set,
//! watch the background probe fill in branch and dirty state, select repos,
//! and run any action from `.mrconfig` against the selection without leaving
//! the screen.

pub mod actions;
pub mod detail;
mod input;
pub mod keymap;
pub mod keys;
pub mod poll;
pub mod probe;
pub mod render;
pub mod session;
pub mod state;
mod suspend;

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::Repo;
use crate::executor::{self, RunEvent};
use crate::operations;
use input::{input_thread, InputThreadGuard};
use probe::Probed;
use state::App;
use suspend::{apply_mouse_capture, suspend_for, EditorOutcome};

/// Begin a new probe generation over `targets` and spawn it.
fn spawn_probe_over(app: &mut App, tx: &mpsc::UnboundedSender<Probed>, targets: Vec<usize>) {
    let generation = app.begin_probe(&targets);
    probe::spawn_probe_generation(&app.repos, targets, app.jobs, generation, tx.clone());
}

/// Plan and spawn a run over `req`'s targets, tagging the app with the run
/// id incoming `RunEvent`s are attributed by. Returns the handle so the
/// caller can cancel it later.
fn spawn_action_run(
    app: &mut App,
    tx: &mpsc::UnboundedSender<RunEvent>,
    req: state::RunRequest,
) -> executor::RunHandle {
    let command = actions::command_for(&req.action);
    let targets: Vec<(usize, operations::Operation)> = req
        .targets
        .iter()
        .map(|&i| (i, operations::plan(&command, &app.repos[i], &app.defaults)))
        .collect();
    let run_id = app.begin_named_run(req.action, req.targets);
    executor::spawn_run(
        &app.repos,
        targets,
        app.jobs,
        app.config_path.clone(),
        tx.clone(),
        run_id,
        // The detail view shows output as it arrives, so this is the one
        // caller that wants a line at a time.
        true,
    )
}

/// How long after the poll is switched on its first cycle fires. Short
/// enough to read as immediate, long enough that mashing `F` on and off
/// collapses to one cycle instead of a fetch per press.
const POLL_RESET_GRACE: Duration = Duration::from_millis(250);

/// How long the poll ticker's next tick should be pushed out to, given
/// `poll_enabled` on the previous loop iteration and its value now, or
/// `None` to leave the running ticker alone.
///
/// Only the off-to-on transition re-phases the ticker, so the startup delay
/// [`run`] builds it with survives and a poll that stays on keeps firing on
/// schedule.
fn poll_ticker_restart_delay(was_enabled: bool, is_enabled: bool) -> Option<Duration> {
    (!was_enabled && is_enabled).then_some(POLL_RESET_GRACE)
}

/// Everything [`run`] needs to open ui mode, bundled to satisfy clippy's
/// argument-count limit.
pub struct RunOptions {
    pub repos: Vec<Repo>,
    pub set_label: String,
    pub jobs: usize,
    pub defaults: BTreeMap<String, String>,
    pub config_path: PathBuf,
    pub force: bool,
    pub dir_override: Option<PathBuf>,
    /// Loaded by `main.rs` before the repo list exists, since the stored set
    /// decides which repos to resolve; applied once, after `App::new`.
    pub session: session::Session,
    /// `--result-ttl`: how long a run's result stays on its row. `None` was
    /// asked for explicitly (`off`); an absent flag arrives here as the
    /// default, resolved by `main.rs`.
    pub result_ttl: Option<Duration>,
}

/// Open ui mode and block until the user quits.
pub async fn run(options: RunOptions) -> io::Result<()> {
    let RunOptions {
        repos,
        set_label,
        jobs,
        defaults,
        config_path,
        force,
        dir_override,
        session,
        result_ttl,
    } = options;

    super::install_panic_hook();
    let mut terminal = super::setup_terminal()?;
    // Every `?` from here on, the run loop's included, returns with raw mode
    // and the alternate screen still active; this guard's `Drop` is what
    // restores the terminal on those paths.
    let _terminal_guard = super::TerminalGuard;
    apply_mouse_capture(true)?;

    let mut app = App::new(
        repos,
        set_label,
        jobs,
        defaults,
        config_path,
        force,
        dir_override,
    );
    app.result_ttl = result_ttl;
    app.restore_session(&session);
    // `session::from_fields` already rejects an out-of-range interval; this
    // is belt and braces, so no path into `poll_interval` can reach the
    // `Instant` arithmetic below unclamped.
    app.poll_interval = poll::clamp_interval(app.poll_interval);
    let (mut input, input_gate, input_handle) = input_thread();
    // Declared after `_terminal_guard` so it drops first; see
    // [`InputThreadGuard`] for why the order matters. Dropped explicitly on
    // the quit path below, left to `Drop` on every early `?` return.
    let input_guard = InputThreadGuard {
        gate: input_gate.clone(),
        handle: Some(input_handle),
    };
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    // The interval arm always ticks; `on_poll_due` decides whether a cycle
    // runs. Delayed a full interval past startup so a restored "poll on"
    // session doesn't race the unconditional probe below.
    let mut poll_ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + app.poll_interval,
        app.poll_interval,
    );
    // `poll_enabled` as of the previous iteration, so the loop can spot `F`
    // turning the poll on without `toggle_poll` recording anything for it.
    let mut poll_was_enabled = app.poll_enabled;
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
    let (run_tx, mut run_rx) = mpsc::unbounded_channel::<RunEvent>();
    let (auto_tx, mut auto_rx) = mpsc::unbounded_channel::<poll::AutoUpdateResult>();

    // The first frame paints immediately with placeholders; this probe's
    // results fill the table in as they arrive.
    {
        let targets: Vec<usize> = (0..app.repos.len()).collect();
        spawn_probe_over(&mut app, &probe_tx, targets);
    }

    let completed = terminal.draw(|frame| render::draw(frame, &app))?;
    app.terminal_width = completed.area.width;
    app.terminal_height = completed.area.height;

    // The live run's handle, held here so `Esc` has something to cancel.
    // Stale once its run is superseded or finished, but flipping a stale
    // handle's flag is harmless: nothing is left to check it.
    let mut current_run: Option<executor::RunHandle> = None;

    loop {
        tokio::select! {
            ev = input.recv() => {
                // `None` means the input thread's own read failed and it
                // ended. Matching `Some(ev)` in the select arm instead would
                // disable this arm for good, leaving the app spinning on
                // ticks with no way left to quit, so treat it as a quit.
                let Some(ev) = ev else {
                    break;
                };
                let should_quit = keys::on_input(&mut app, ev);
                // Best-effort: a failed session write (a full disk, a
                // read-only home) shouldn't take the app down.
                let _ = session::save(&app);
                if should_quit {
                    break;
                }
                if app.take_full_reprobe_request() {
                    let targets: Vec<usize> = (0..app.repos.len()).collect();
                    spawn_probe_over(&mut app, &probe_tx, targets);
                }
                if app.take_probe_request() {
                    let targets = app.reprobe_targets();
                    spawn_probe_over(&mut app, &probe_tx, targets);
                }
                if let Some(req) = app.take_run_requested() {
                    current_run = Some(spawn_action_run(&mut app, &run_tx, req));
                }
                if app.take_cancel_requested() {
                    if let Some(handle) = &current_run {
                        handle.request_cancel();
                    }
                }
                if app.take_mouse_capture_dirty() {
                    apply_mouse_capture(app.mouse_captured)?;
                }
                if let Some(what) = app.take_foreground() {
                    match suspend_for(&mut terminal, &what, app.mouse_captured, &input_gate) {
                        Ok(EditorOutcome::Ok) => {}
                        Ok(EditorOutcome::EditorFailed(e)) => {
                            app.status_message = Some(match what {
                                state::Suspend::Shell(_) => format!("could not open $SHELL: {e}"),
                                state::Suspend::Editor(_) => {
                                    format!("could not open $EDITOR: {e}")
                                }
                            });
                        }
                        // The terminal itself could not be restored, so
                        // drawing again would paint over a real shell mrx no
                        // longer controls. Propagate, and let `main.rs`'s
                        // `.expect()` hand the panic hook one last attempt at
                        // putting the terminal back.
                        Err(e) => return Err(e),
                    }
                }
            }
            Some(probed) = probe_rx.recv() => {
                app.on_probe(probed.generation, probed.state);
                if let Some(targets) = app.take_auto_update_requested() {
                    poll::spawn_auto_update(
                        &app.repos,
                        targets,
                        app.jobs,
                        app.auto_update_generation(),
                        auto_tx.clone(),
                    );
                }
            }
            Some(evt) = run_rx.recv() => {
                app.on_task(evt.run_id, evt.kind);
                if let Some(targets) = app.take_post_run_targets() {
                    spawn_probe_over(&mut app, &probe_tx, targets);
                }
            }
            Some(result) = auto_rx.recv() => {
                app.on_auto_update_result(result);
                if let Some(targets) = app.take_auto_update_reprobe_targets() {
                    spawn_probe_over(&mut app, &probe_tx, targets);
                }
            }
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(1);
                app.expire_results();
            }
            _ = poll_ticker.tick() => {
                app.on_poll_due();
                if let Some(targets) = app.take_poll_requested() {
                    poll::spawn_poll_generation(&app.repos, targets, app.jobs, app.probe_generation, probe_tx.clone());
                }
            }
        }
        if let Some(delay) = poll_ticker_restart_delay(poll_was_enabled, app.poll_enabled) {
            poll_ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + delay, app.poll_interval);
        }
        poll_was_enabled = app.poll_enabled;

        let completed = terminal.draw(|frame| render::draw(frame, &app))?;
        app.terminal_width = completed.area.width;
        app.terminal_height = completed.area.height;
    }

    // Explicit rather than waiting for scope exit, so the reader is stopped
    // and joined before the teardown below hands the tty back rather than
    // after it; see [`InputThreadGuard`].
    drop(input_guard);

    apply_mouse_capture(false)?;
    super::teardown_terminal()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for `app.poll_interval` in the ticker tests. The real
    /// default is 300s; these wait on wall-clock ticks, so they use an
    /// interval that is still an order of magnitude clear of the grace.
    ///
    /// Both ticker tests rebuild the ticker themselves, so they pin
    /// [`poll_ticker_restart_delay`], not its call site in [`run`]: deleting
    /// that call leaves them green.
    const TEST_INTERVAL: Duration = Duration::from_secs(4);

    #[test]
    fn only_switching_the_poll_on_restarts_its_ticker() {
        assert_eq!(
            poll_ticker_restart_delay(false, true),
            Some(POLL_RESET_GRACE)
        );
        assert_eq!(poll_ticker_restart_delay(true, true), None);
        assert_eq!(poll_ticker_restart_delay(true, false), None);
        assert_eq!(poll_ticker_restart_delay(false, false), None);
    }

    #[tokio::test]
    async fn switching_the_poll_on_pulls_its_next_cycle_off_the_interval_boundary() {
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + TEST_INTERVAL, TEST_INTERVAL);
        // The poll starts off, so the ticker is running but every tick is
        // declined; `F` arrives partway through the current interval.
        let pressed = std::time::Instant::now();
        if let Some(delay) = poll_ticker_restart_delay(false, true) {
            ticker = tokio::time::interval_at(tokio::time::Instant::now() + delay, TEST_INTERVAL);
        }

        ticker.tick().await;
        let waited = pressed.elapsed();
        assert!(
            waited < TEST_INTERVAL / 2,
            "first poll cycle after F waited {waited:?}, wanted well under {TEST_INTERVAL:?}"
        );
    }

    #[tokio::test]
    async fn a_poll_already_on_keeps_the_ticker_it_has() {
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + TEST_INTERVAL, TEST_INTERVAL);
        // Iterations of the run loop with the poll on throughout: restarting
        // on any of them would push the next cycle out indefinitely.
        for _ in 0..20 {
            if let Some(delay) = poll_ticker_restart_delay(true, true) {
                ticker =
                    tokio::time::interval_at(tokio::time::Instant::now() + delay, TEST_INTERVAL);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        assert!(
            tokio::time::timeout(TEST_INTERVAL, ticker.tick())
                .await
                .is_ok(),
            "the ticker never fired within its own interval"
        );
    }
}
