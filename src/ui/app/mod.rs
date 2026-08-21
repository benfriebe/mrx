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

use crate::cli::Command;
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
    probe::spawn_cycle(
        &app.repos,
        targets,
        app.jobs,
        generation,
        tx,
        probe::Cycle::Probe,
    );
}

/// Start a freshness cycle if one is due, the single path both the interval
/// tick and the opening fetch take. `on_poll_due` is what decides whether the
/// cycle actually runs, so calling this when it should not is a no-op.
fn spawn_poll_cycle(app: &mut App, tx: &mpsc::UnboundedSender<Probed>) {
    app.on_poll_due();
    if let Some(targets) = app.take_poll_requested() {
        // The same job limit a probe uses, so a poll cannot compete with a
        // live run for the network.
        probe::spawn_cycle(
            &app.repos,
            targets,
            app.jobs,
            app.probe_generation,
            tx,
            probe::Cycle::Poll,
        );
    }
}

/// Plan and spawn a run over `req`'s targets, tagging the app with the run
/// id incoming `RunEvent`s are attributed by. Returns the handle so the
/// caller can cancel it later.
fn spawn_action_run(
    app: &mut App,
    tx: &mpsc::UnboundedSender<RunEvent>,
    req: state::RunRequest,
) -> executor::RunHandle {
    // A body typed at the run-command prompt is planned as `run`, which hands
    // it to `sh` whole; only a named action has a definition to look up.
    let command = match &req.body {
        Some(body) => Command::Run {
            cmd: vec![body.clone()],
        },
        None => actions::command_for(&req.action),
    };
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

/// How long the poll ticker's next tick should be pushed out to, given the
/// poll's state on the previous loop iteration and its state now, or `None`
/// to leave the running ticker alone.
///
/// Switching on re-phases, so the first cycle after `F` is prompt. So does a
/// new interval, which a set switch can hand over: the running ticker holds
/// the period it was built with and would otherwise keep the old set's
/// cadence for the rest of the session. A poll that stays on unchanged is
/// left alone, so the startup delay [`run`] builds it with survives.
fn poll_ticker_restart_delay(was: (bool, Duration), now: (bool, Duration)) -> Option<Duration> {
    let ((was_enabled, was_interval), (is_enabled, interval)) = (was, now);
    if !is_enabled {
        return None;
    }
    if !was_enabled {
        return Some(POLL_RESET_GRACE);
    }
    (was_interval != interval).then_some(interval)
}

/// Everything [`run`] needs to open ui mode, bundled to satisfy clippy's
/// argument-count limit.
pub struct RunOptions {
    pub repos: Vec<Repo>,
    pub set_label: String,
    pub jobs: usize,
    /// `-j`, if it was given, so a reload can re-resolve `jobs` without
    /// losing it.
    pub jobs_flag: Option<usize>,
    pub defaults: BTreeMap<String, String>,
    pub config_path: PathBuf,
    pub force: bool,
    pub dir_override: Option<PathBuf>,
    /// Loaded by `main.rs` before the repo list exists, since the stored set
    /// decides which repos to resolve; applied once, after `App::new`.
    pub session: session::Session,
    /// `[DEFAULT] auto_fetch` from the set being opened, if it sets one.
    /// Seeds the poll before the session gets its say.
    pub auto_fetch: Option<Duration>,
    /// `--result-ttl`: how long a run's result stays on its row. `None` was
    /// asked for explicitly (`off`); an absent flag arrives here as the
    /// default, resolved by `main.rs`.
    pub result_ttl: Option<Duration>,
}

/// Open ui mode and block until the user quits.
// Setup, loop, and teardown stay in one body: the guards here drop in reverse
// declaration order, so splitting it would hide that sequencing, not shorten it.
#[expect(clippy::too_many_lines)]
pub async fn run(options: RunOptions) -> io::Result<()> {
    let RunOptions {
        repos,
        set_label,
        jobs,
        jobs_flag,
        defaults,
        config_path,
        force,
        dir_override,
        session,
        auto_fetch,
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
    app.jobs_flag = jobs_flag;
    // Before the session, which is the more specific record: the config says
    // what this set does by default, the session what was last chosen in it.
    app.apply_auto_fetch(auto_fetch);
    app.restore_session(&session);
    app.arm_boot_fetch();
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
    // The poll's settings as of the previous iteration, so the loop can spot
    // `F` or a set switch changing them without either recording anything.
    let mut poll_was = (app.poll_enabled, app.poll_interval);
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
    let (run_tx, mut run_rx) = mpsc::unbounded_channel::<RunEvent>();

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
                let should_quit = keys::on_input(&mut app, &ev);
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
            }
            Some(evt) = run_rx.recv() => {
                app.on_task(evt.run_id, evt.kind);
                if let Some(targets) = app.take_post_run_targets() {
                    spawn_probe_over(&mut app, &probe_tx, targets);
                }
            }
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(1);
                app.expire_results();
            }
            _ = poll_ticker.tick() => {
                spawn_poll_cycle(&mut app, &probe_tx);
            }
        }
        // Outside the input arm: auto-update asks for a run from the probe
        // arm, once a poll cycle's last result lands.
        if let Some(req) = app.take_run_requested() {
            current_run = Some(spawn_action_run(&mut app, &run_tx, req));
        }
        // Owed from startup, and held until the opening probe has landed so
        // the table is filled in before a cycle of fetches replaces it.
        if app.take_boot_fetch() {
            spawn_poll_cycle(&mut app, &probe_tx);
        }
        let poll_now = (app.poll_enabled, poll::clamp_interval(app.poll_interval));
        if let Some(delay) = poll_ticker_restart_delay(poll_was, poll_now) {
            poll_ticker = tokio::time::interval_at(tokio::time::Instant::now() + delay, poll_now.1);
        }
        poll_was = poll_now;

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

    /// Stands in for `app.poll_interval` in the ticker tests, which run on a
    /// paused clock: nothing here waits in real time, so the interval only has
    /// to stay an order of magnitude clear of [`POLL_RESET_GRACE`].
    ///
    /// Both ticker tests rebuild the ticker themselves, so they pin
    /// [`poll_ticker_restart_delay`], not its call site in [`run`]: deleting
    /// that call leaves them green.
    const TEST_INTERVAL: Duration = Duration::from_secs(4);

    #[test]
    fn only_a_change_to_the_polls_settings_restarts_its_ticker() {
        let off = (false, TEST_INTERVAL);
        let on = (true, TEST_INTERVAL);
        assert_eq!(poll_ticker_restart_delay(off, on), Some(POLL_RESET_GRACE));
        assert_eq!(poll_ticker_restart_delay(on, on), None);
        assert_eq!(poll_ticker_restart_delay(on, off), None);
        assert_eq!(poll_ticker_restart_delay(off, off), None);
    }

    /// A set switch can hand over a different interval with the poll on
    /// either side of it, and the ticker holds the period it was built with.
    #[test]
    fn a_new_interval_rephases_the_ticker_onto_it() {
        let slower = Duration::from_mins(10);
        assert_eq!(
            poll_ticker_restart_delay((true, TEST_INTERVAL), (true, slower)),
            Some(slower),
            "the next cycle is a full interval of the new length away"
        );
        assert_eq!(
            poll_ticker_restart_delay((false, TEST_INTERVAL), (true, slower)),
            Some(POLL_RESET_GRACE),
            "switching on is still prompt, whatever the interval changed to"
        );
        assert_eq!(
            poll_ticker_restart_delay((true, TEST_INTERVAL), (false, slower)),
            None,
            "an interval nothing is going to fire on needs no ticker"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn switching_the_poll_on_pulls_its_next_cycle_off_the_interval_boundary() {
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + TEST_INTERVAL, TEST_INTERVAL);
        // The poll starts off, so the ticker is running but every tick is
        // declined; `F` arrives partway through the current interval.
        let pressed = tokio::time::Instant::now();
        if let Some(delay) =
            poll_ticker_restart_delay((false, TEST_INTERVAL), (true, TEST_INTERVAL))
        {
            ticker = tokio::time::interval_at(tokio::time::Instant::now() + delay, TEST_INTERVAL);
        }

        ticker.tick().await;
        let waited = pressed.elapsed();
        assert!(
            waited < TEST_INTERVAL / 2,
            "first poll cycle after F waited {waited:?}, wanted well under {TEST_INTERVAL:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_poll_already_on_keeps_the_ticker_it_has() {
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + TEST_INTERVAL, TEST_INTERVAL);
        // Iterations of the run loop with the poll on throughout: restarting
        // on any of them would push the next cycle out indefinitely.
        for _ in 0..20 {
            if let Some(delay) =
                poll_ticker_restart_delay((true, TEST_INTERVAL), (true, TEST_INTERVAL))
            {
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
