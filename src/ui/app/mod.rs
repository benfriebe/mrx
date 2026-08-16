//! The resident app: a repo table that stays on screen across runs. Browse a
//! set, watch the background probe fill in branch and dirty state, select
//! repos, and run any action from `.mrconfig` against the selection without
//! leaving the screen.

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
/// id it needs to attribute incoming `RunEvent`s to and drive the header
/// with. Returns the handle so the caller can cancel it later.
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
        // The detail view shows output while it arrives, so the app is the
        // one caller that wants a line at a time.
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
/// Only the off-to-on transition re-phases the ticker: on every other
/// iteration the running one is left to its own interval, so the startup
/// delay [`run`] builds it with survives and a poll that stays on keeps
/// firing on schedule.
fn poll_ticker_restart_delay(was_enabled: bool, is_enabled: bool) -> Option<Duration> {
    (!was_enabled && is_enabled).then_some(POLL_RESET_GRACE)
}

/// Everything [`run`] needs to open the resident app, bundled to satisfy
/// clippy's argument-count limit (the same shape `widgets::RepoRow` used
/// for this in phase 0).
pub struct RunOptions {
    pub repos: Vec<Repo>,
    pub set_label: String,
    pub jobs: usize,
    pub defaults: BTreeMap<String, String>,
    pub config_path: PathBuf,
    pub force: bool,
    pub dir_override: Option<PathBuf>,
    /// Whatever `session::load()` returned before the repo list was even
    /// resolved (`main.rs` needed it earlier, to decide which set to open
    /// on the stored one's own terms); applied once, after `App::new`, to
    /// restore the filter, selection, cursor, and poll settings.
    pub session: session::Session,
    /// `--result-ttl`: how long a run's result stays on its row. `None` was
    /// asked for explicitly (`off`); an absent flag arrives here as the
    /// default, resolved by `main.rs`.
    pub result_ttl: Option<Duration>,
}

/// Open the resident app and block until the user quits.
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
    // From here on, every `?` in this function (including the ones inside
    // the run loop below) is a way to return early with raw mode and the
    // alternate screen still active; `_terminal_guard`'s `Drop` is what
    // restores the terminal on any of those paths, not just the explicit
    // teardown at the bottom of this function.
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
    // A corrupted or hostile `ui.json` is caught at the point it's parsed
    // (`session::from_fields`); this is the last line of defense so that no
    // path into `poll_interval`, present or future, can build an `Instant`
    // that overflows below.
    app.poll_interval = poll::clamp_interval(app.poll_interval);
    let (mut input, input_gate, input_handle) = input_thread();
    // Declared after `_terminal_guard` so it drops first; see the type's
    // own doc comment for why the order matters. Dropped explicitly, ahead
    // of the terminal teardown below, on the normal quit path; left to
    // `Drop` on every early `?` return instead.
    let input_guard = InputThreadGuard {
        gate: input_gate.clone(),
        handle: Some(input_handle),
    };
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    // The interval arm always ticks, whether or not the poll is on
    // (section 05); `on_poll_due` is what actually decides. Delayed a full
    // interval past startup rather than firing immediately, so a restored
    // "poll on" session doesn't race the very first, unconditional probe
    // below.
    let mut poll_ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + app.poll_interval,
        app.poll_interval,
    );
    // `poll_enabled` as of the end of the previous iteration, so the loop
    // can spot `F` turning the poll on without `toggle_poll` having to
    // record anything for it.
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
    // Stale once its run is superseded or finished, but flipping its flag
    // then is harmless: nothing is left to check it.
    let mut current_run: Option<executor::RunHandle> = None;

    loop {
        tokio::select! {
            ev = input.recv() => {
                // `None` means the input thread's own read failed and it
                // ended: pattern-matching on `Some(ev)` in the
                // select arm above would just leave this arm permanently
                // disabled instead, since the receiver stays instantly
                // ready-with-None from here on, and the app would spin
                // forever redrawing on ticks with no way left to quit.
                // Treat that the same as an explicit quit.
                let Some(ev) = ev else {
                    break;
                };
                let should_quit = keys::on_input(&mut app, ev);
                // Best-effort: a session write failing (a full disk, a
                // read-only home) shouldn't take the app down over it.
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
                        // The terminal itself could not be restored; drawing
                        // another frame against `terminal` would just paint
                        // over a real shell that mrx no longer controls.
                        // Propagate so `main.rs`'s `.expect()` panics and the
                        // installed panic hook gets one last attempt at
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
        // Turning the poll on asks for a cycle now, not at whatever tick
        // boundary the ticker was already heading for.
        if let Some(delay) = poll_ticker_restart_delay(poll_was_enabled, app.poll_enabled) {
            poll_ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + delay, app.poll_interval);
        }
        poll_was_enabled = app.poll_enabled;

        let completed = terminal.draw(|frame| render::draw(frame, &app))?;
        app.terminal_width = completed.area.width;
        app.terminal_height = completed.area.height;
    }

    // Stop and join the input thread before handing the tty back: otherwise
    // it's still polling stdin for up to GATE_POLL_INTERVAL after this
    // returns, competing with the shell the terminal is about to be
    // restored to for the first key the user types. An explicit drop here
    // rather than waiting for scope exit, so this happens before the
    // teardown below rather than after it.
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
    /// The two ticker tests below rebuild the ticker themselves, so they
    /// pin [`poll_ticker_restart_delay`] and tokio's `interval_at`, not the
    /// call site in [`run`]: deleting that call leaves them green. Only
    /// driving the app covers it.
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
