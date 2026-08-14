//! The resident app: a repo table that stays on screen across runs. Browse a
//! set, watch the background probe fill in branch and dirty state, select
//! repos, and run any action from `.mrconfig` against the selection without
//! leaving the screen.

pub mod actions;
pub mod detail;
pub mod keys;
pub mod poll;
pub mod probe;
pub mod render;
pub mod session;
pub mod state;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::Repo;
use crate::executor::{self, RunEvent};
use crate::operations;
use probe::Probed;
use state::App;

/// How often the input thread checks whether it's allowed to touch the tty
/// while [`InputGate::park`] has paused it for `$EDITOR`, and how often
/// [`InputGate::park`] itself checks for the thread's acknowledgment.
/// Small enough that resuming feels instant, large enough not to spin.
const GATE_POLL_INTERVAL: Duration = Duration::from_millis(30);

/// Lets the run loop stop the input thread from reading stdin while
/// `$EDITOR` owns the terminal, so mrx and the editor are never both
/// blocked on the same tty at once (`o`, section 03), and lets it stop the
/// thread for good on the way out so it isn't still competing for
/// keystrokes with the shell mrx just handed the tty back to.
///
/// `crossterm::read()` itself can't be interrupted once it's blocked in a
/// syscall, so the thread never calls it unless the gate is open. But
/// merely closing the gate isn't enough on its own: the thread could have
/// already passed its own open check and be mid `poll`/`read` when the gate
/// closes, and nothing stops it from sending that stray event before it
/// next notices. `park` closes the gate and then blocks until the thread itself
/// reports, via `parked`, that it has actually reached the paused branch
/// with no read in flight, turning "probably stopped by now" into an
/// explicit handshake.
#[derive(Clone)]
struct InputGate {
    open: Arc<AtomicBool>,
    /// Set by the input thread itself, only from inside the paused branch:
    /// true exactly when it is safe to assume no `poll`/`read` is in flight.
    parked: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl InputGate {
    fn new() -> Self {
        Self {
            open: Arc::new(AtomicBool::new(true)),
            parked: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Close the gate and block until the input thread acknowledges it has
    /// actually parked, not just observed the flag. Bounded to roughly
    /// [`GATE_POLL_INTERVAL`] past whatever poll/read cycle was already in
    /// flight when this was called.
    fn park(&self) {
        self.open.store(false, Ordering::SeqCst);
        while !self.parked.load(Ordering::SeqCst) {
            std::thread::sleep(GATE_POLL_INTERVAL);
        }
    }

    fn resume(&self) {
        self.open.store(true, Ordering::SeqCst);
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    /// Called by the input thread itself, between poll/read cycles, to
    /// record whether one could currently be in flight.
    fn mark_parked(&self, parked: bool) {
        self.parked.store(parked, Ordering::SeqCst);
    }

    /// Ask the input thread to end its loop for good; [`run`] joins the
    /// thread's handle afterward to wait it out.
    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    fn should_stop(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// crossterm's `read()` blocks, so input gets its own thread rather than the
/// `event-stream` feature and a futures dependency. The thread never blocks
/// for longer than [`GATE_POLL_INTERVAL`] at a time: it polls with that
/// timeout and only reads when something is actually ready, so it can
/// notice the returned [`InputGate`] closing (for `$EDITOR`) or being asked
/// to stop (on quit) instead of parking in a read that could otherwise race
/// a child process, or the just-restored shell, for the same keystrokes.
/// The join handle is how [`run`] waits for the thread to actually stop
/// before handing the tty back.
fn input_thread() -> (
    mpsc::UnboundedReceiver<Event>,
    InputGate,
    std::thread::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let gate = InputGate::new();
    let thread_gate = gate.clone();
    let handle = std::thread::spawn(move || loop {
        if thread_gate.should_stop() {
            break;
        }
        if !thread_gate.is_open() {
            thread_gate.mark_parked(true);
            std::thread::sleep(GATE_POLL_INTERVAL);
            continue;
        }
        thread_gate.mark_parked(false);
        match crossterm::event::poll(GATE_POLL_INTERVAL) {
            Ok(true) => match crossterm::event::read() {
                Ok(ev) => {
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            },
            Ok(false) => continue,
            Err(_) => break,
        }
    });
    (rx, gate, handle)
}

fn apply_mouse_capture(enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        execute!(io::stdout(), DisableMouseCapture)
    }
}

/// What happened when `$EDITOR` was run, once the terminal itself is known
/// to be back in a good state. Kept separate from `open_editor`'s `Err`
/// case, which means the terminal restoration itself failed and the
/// caller can no longer trust `terminal` at all.
enum EditorOutcome {
    Ok,
    /// The editor process couldn't run (a bad `$EDITOR`, typically); the
    /// terminal was still fully restored before this is returned.
    EditorFailed(io::Error),
}

/// `o`: suspend the alternate screen, raw mode, mouse capture (if it was
/// on), and the input thread (via `gate`, so it stops competing with the
/// editor for stdin), run `$EDITOR` (falling back to `vi`) on `path` to
/// completion, then restore all of it exactly as it was. A blocking wait is
/// the point: there is nothing useful for the app to do while the editor
/// has the terminal, and any probe or run events that arrive in the
/// meantime just sit in their channels until the next draw picks them up,
/// the same eventually-consistent handling every other background result
/// gets.
///
/// `gate.park()` blocks until the input thread confirms it has actually
/// stopped touching stdin before this goes on to tear down the terminal and
/// launch the editor: closing the gate and immediately proceeding isn't
/// enough, since the thread can already be mid `poll` or `read` when the
/// gate closes, and a keystroke meant for the editor would otherwise be won
/// by mrx's own reader instead.
///
/// An `Err` return means re-entering raw mode or the alternate screen
/// failed and the real terminal is left in whatever state that partial
/// attempt produced; callers must not keep drawing against `terminal` as
/// though nothing happened; see [`run`]'s call site.
fn open_editor(
    terminal: &mut super::Term,
    path: &Path,
    mouse_captured: bool,
    gate: &InputGate,
) -> io::Result<EditorOutcome> {
    gate.park();
    let outcome = (|| {
        if mouse_captured {
            apply_mouse_capture(false)?;
        }
        super::teardown_terminal()?;

        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let mut parts = editor.split_whitespace();
        let bin = parts.next().unwrap_or("vi");
        let spawn_result = std::process::Command::new(bin)
            .args(parts)
            .arg(path)
            .status();

        *terminal = super::setup_terminal()?;
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
    )
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
    app.restore_session(&session);
    // A corrupted or hostile `ui.json` is caught at the point it's parsed
    // (`session::from_fields`); this is the last line of defense so that no
    // path into `poll_interval`, present or future, can build an `Instant`
    // that overflows below.
    app.poll_interval = poll::clamp_interval(app.poll_interval);
    let (mut input, input_gate, input_handle) = input_thread();
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
                if let Some(path) = app.take_open_editor_requested() {
                    match open_editor(&mut terminal, &path, app.mouse_captured, &input_gate) {
                        Ok(EditorOutcome::Ok) => {}
                        Ok(EditorOutcome::EditorFailed(e)) => {
                            app.status_message = Some(format!("could not open $EDITOR: {e}"));
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
            }
            _ = poll_ticker.tick() => {
                app.on_poll_due();
                if let Some(targets) = app.take_poll_requested() {
                    poll::spawn_poll_generation(&app.repos, targets, app.jobs, app.probe_generation, probe_tx.clone());
                }
            }
        }
        let completed = terminal.draw(|frame| render::draw(frame, &app))?;
        app.terminal_width = completed.area.width;
        app.terminal_height = completed.area.height;
    }

    // Ask the input thread to end its loop and wait for it to actually do
    // so before handing the tty back: otherwise it's still polling stdin
    // for up to GATE_POLL_INTERVAL after this returns, competing with the
    // shell the terminal is about to be restored to for the first key the
    // user types.
    input_gate.stop();
    let _ = input_handle.join();

    apply_mouse_capture(false)?;
    super::teardown_terminal()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_input_gate_starts_open_and_not_parked() {
        let gate = InputGate::new();
        assert!(gate.is_open());
        assert!(!gate.parked.load(Ordering::SeqCst));
        assert!(!gate.should_stop());
    }

    #[test]
    fn resume_reopens_a_gate_the_reader_has_already_acknowledged() {
        // Simulates the reader thread having caught up and parked, so
        // `park()` itself doesn't block this test.
        let gate = InputGate::new();
        gate.mark_parked(true);
        gate.park();
        assert!(!gate.is_open());
        gate.resume();
        assert!(gate.is_open());
    }

    #[test]
    fn a_cloned_gate_shares_state_with_the_original() {
        // The input thread holds a clone; closing the gate from the run
        // loop's copy must be visible to the thread's.
        let gate = InputGate::new();
        let clone = gate.clone();
        clone.mark_parked(true); // so this gate's own park() below doesn't block
        gate.park();
        assert!(!clone.is_open(), "a clone must observe the same state");
    }

    #[test]
    fn stop_is_observed_by_a_clone() {
        // The input thread checks its own clone's `should_stop`; the run
        // loop signals the original on the way out.
        let gate = InputGate::new();
        let clone = gate.clone();
        gate.stop();
        assert!(clone.should_stop());
    }

    /// Closing the gate alone isn't enough: `park()` must actually wait for
    /// the reader's own acknowledgment rather than assuming a closed gate
    /// means the reader has stopped.
    #[test]
    fn park_blocks_until_the_reader_marks_itself_parked() {
        let gate = InputGate::new();
        let acknowledged_before_park_returned = Arc::new(AtomicBool::new(false));
        let flag = acknowledged_before_park_returned.clone();
        let reader = gate.clone();
        let simulated_reader = std::thread::spawn(move || {
            // Stands in for the input thread finishing whatever poll/read
            // cycle was already in flight when the gate closed.
            std::thread::sleep(Duration::from_millis(50));
            flag.store(true, Ordering::SeqCst);
            reader.mark_parked(true);
        });

        gate.park();
        assert!(
            acknowledged_before_park_returned.load(Ordering::SeqCst),
            "park() must not return before the reader acknowledges it has parked"
        );
        simulated_reader.join().unwrap();
    }

    /// The other half of the input-channel-close fix: `run`'s select arm
    /// used to read `Some(ev) = input.recv() =>`, which just disables that arm forever
    /// once the channel closes (the input thread's read failed and it
    /// ended) rather than ending the loop, so the app would spin on ticks
    /// with no way left to quit. This mirrors that arm's exact shape,
    /// swapped between the old pattern-match guard and the new explicit
    /// `let-else`, against a real channel closed the same way the input
    /// thread closes it (dropping the sender).
    #[tokio::test]
    async fn a_closed_input_channel_ends_the_loop_instead_of_spinning_on_ticks() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
        drop(tx);

        let mut ticks = 0u32;
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let ended = loop {
            tokio::select! {
                ev = rx.recv() => {
                    let Some(_ev) = ev else {
                        break true;
                    };
                    unreachable!("no events were ever sent");
                }
                _ = ticker.tick() => {
                    ticks += 1;
                    if ticks > 50 {
                        break false;
                    }
                }
            }
        };

        assert!(
            ended,
            "a closed input channel must end the loop rather than let it spin on ticks forever"
        );
    }
}
