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
use std::sync::{Arc, Condvar, Mutex};
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
/// next notices. `park` closes the gate and then blocks until the thread
/// itself reports, via a generation counter, that it has actually reached
/// the paused branch with no read in flight, turning "probably stopped by
/// now" into an explicit handshake.
///
/// The acknowledgment is tagged with a generation, bumped on every `park`
/// call, rather than a plain flag: a plain flag set once and never cleared
/// would let a later `park` return instantly on an acknowledgment left over
/// from an earlier pause, before the thread has actually parked for *this*
/// one. And because a thread whose own loop has already ended (a read
/// error, stdin closing) can never acknowledge anything again, `park` also
/// gives up and returns once the thread reports it has exited, rather than
/// waiting on an acknowledgment that will never come.
#[derive(Clone)]
struct InputGate {
    state: Arc<Mutex<GateState>>,
    /// Wakes a blocked `park` when the reader acknowledges (at any
    /// generation) or reports it has exited.
    condvar: Arc<Condvar>,
}

struct GateState {
    open: bool,
    stop: bool,
    /// Bumped by every `park` call, so an acknowledgment can be checked
    /// against the specific pause it answers.
    generation: u64,
    /// The generation the reader last confirmed it has actually parked
    /// for, if any.
    parked_generation: Option<u64>,
    /// Set once, by the reader's own exit path, when its loop ends for any
    /// reason.
    exited: bool,
}

impl InputGate {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(GateState {
                open: true,
                stop: false,
                generation: 0,
                parked_generation: None,
                exited: false,
            })),
            condvar: Arc::new(Condvar::new()),
        }
    }

    /// Close the gate and block until the reader acknowledges this specific
    /// pause, or reports it has exited.
    fn park(&self) {
        let my_generation = {
            let mut state = self.state.lock().unwrap();
            state.generation += 1;
            state.open = false;
            state.generation
        };
        let mut state = self.state.lock().unwrap();
        while state.parked_generation != Some(my_generation) && !state.exited {
            state = self.condvar.wait(state).unwrap();
        }
    }

    fn resume(&self) {
        self.state.lock().unwrap().open = true;
    }

    fn is_open(&self) -> bool {
        self.state.lock().unwrap().open
    }

    /// Called by the input thread itself, from inside the paused branch, to
    /// record that it has actually parked for whichever pause is current
    /// right now.
    fn mark_parked(&self) {
        let mut state = self.state.lock().unwrap();
        state.parked_generation = Some(state.generation);
        drop(state);
        self.condvar.notify_all();
    }

    /// Called once by the input thread's own exit path, however its loop
    /// ends, so a `park` call waiting on a reader that will never
    /// acknowledge again returns instead of spinning forever.
    fn mark_exited(&self) {
        let mut state = self.state.lock().unwrap();
        state.exited = true;
        drop(state);
        self.condvar.notify_all();
    }

    /// Ask the input thread to end its loop for good; [`run`] joins the
    /// thread's handle afterward to wait it out.
    fn stop(&self) {
        self.state.lock().unwrap().stop = true;
    }

    fn should_stop(&self) -> bool {
        self.state.lock().unwrap().stop
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
    let handle = std::thread::spawn(move || {
        // Marks the gate exited when this loop ends for any reason,
        // including a panic unwinding out of it, so a `park` call left
        // waiting on this reader isn't stuck waiting on one that no longer
        // exists to acknowledge it.
        struct MarkExitedOnDrop(InputGate);
        impl Drop for MarkExitedOnDrop {
            fn drop(&mut self) {
                self.0.mark_exited();
            }
        }
        let _exit_guard = MarkExitedOnDrop(thread_gate.clone());

        loop {
            if thread_gate.should_stop() {
                break;
            }
            if !thread_gate.is_open() {
                thread_gate.mark_parked();
                std::thread::sleep(GATE_POLL_INTERVAL);
                continue;
            }
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
        }
    });
    (rx, gate, handle)
}

/// Stops and joins the input thread on every way out of [`run`]: the normal
/// quit, and every early `?` return from a draw, mouse-capture, or editor
/// failure. Without this, only the happy path joined the thread, so an
/// error return left it detached and still polling stdin for up to
/// [`GATE_POLL_INTERVAL`] after the terminal was handed back, competing
/// with the shell for the user's next keystroke.
///
/// Declared after `TerminalGuard` in [`run`] so it drops first: the input
/// thread must stop touching stdin *before* raw mode and the alternate
/// screen are torn down, for the same reason the join has to happen at
/// all. Reversed, the terminal would already be back in the caller's hands
/// while this thread could still be mid `poll`/`read` racing it for input.
struct InputThreadGuard {
    gate: InputGate,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for InputThreadGuard {
    fn drop(&mut self) {
        self.gate.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
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

    #[test]
    fn a_new_input_gate_starts_open_and_not_parked() {
        let gate = InputGate::new();
        assert!(gate.is_open());
        assert!(gate.state.lock().unwrap().parked_generation.is_none());
        assert!(!gate.should_stop());
    }

    #[test]
    fn resume_reopens_a_gate_the_reader_has_already_acknowledged() {
        let gate = InputGate::new();
        let reader = gate.clone();
        let acker = std::thread::spawn(move || {
            // Stands in for the input thread noticing the gate closed and
            // acknowledging it, so `park()` below doesn't block forever.
            while reader.is_open() {
                std::thread::sleep(Duration::from_millis(1));
            }
            reader.mark_parked();
        });

        gate.park();
        acker.join().unwrap();
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
        let acker = clone.clone();
        let acker_thread = std::thread::spawn(move || {
            while acker.is_open() {
                std::thread::sleep(Duration::from_millis(1));
            }
            acker.mark_parked();
        });

        gate.park();
        acker_thread.join().unwrap();
        assert!(!clone.is_open(), "a clone must observe the same state");
    }

    /// Closing the gate alone isn't enough: `park()` must actually wait for
    /// the reader's own acknowledgment rather than assuming a closed gate
    /// means the reader has stopped, and that has to keep holding across
    /// more than one pause: a lone single-cycle check can't tell a real
    /// generation-tagged handshake apart from the old plain flag it
    /// replaced, since a flag set once and never cleared still blocks
    /// correctly the first time. Runs the reader through two full
    /// park/resume cycles, each timed to confirm `park()` genuinely waited
    /// rather than merely completing, then through the reader exiting for
    /// good, confirming `park()` gives up rather than hanging on an
    /// acknowledgment that will never come.
    #[test]
    fn park_blocks_until_the_reader_marks_itself_parked() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let gate = InputGate::new();

        // A fresh single-shot acker per cycle, the same idiom the other
        // multi-cycle tests in this module use: it only has to notice the
        // gate close once and acknowledge, so there is no window between
        // cycles where it could miss a reopen that a persistent polling
        // loop racing the main thread's own resume/park pair could.
        fn spawn_timed_acker(gate: &InputGate) -> (std::thread::JoinHandle<()>, Arc<AtomicBool>) {
            let acknowledged_after_delay = Arc::new(AtomicBool::new(false));
            let flag = acknowledged_after_delay.clone();
            let reader = gate.clone();
            let handle = std::thread::spawn(move || {
                while reader.is_open() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                // Stands in for the input thread finishing whatever
                // poll/read cycle was already in flight when the gate
                // closed.
                std::thread::sleep(Duration::from_millis(30));
                flag.store(true, Ordering::SeqCst);
                reader.mark_parked();
            });
            (handle, acknowledged_after_delay)
        }

        for cycle in 1..=2u32 {
            let (acker, acknowledged) = spawn_timed_acker(&gate);
            gate.park();
            assert!(
                acknowledged.load(Ordering::SeqCst),
                "park() must not return before the reader acknowledges it has parked (cycle {cycle})"
            );
            acker.join().unwrap();
            gate.resume();
        }

        // The reader's own loop has now ended for good; a further park()
        // must give up instead of waiting on an acknowledgment that will
        // never come.
        gate.mark_exited();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = gate.clone();
        std::thread::spawn(move || {
            waiter.park();
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("park() must return once the reader has exited, rather than spin forever");
    }

    /// A flag set once and never cleared would let this second pause return
    /// immediately on the first pause's leftover acknowledgment, before the
    /// reader has actually parked for this one; this is why the
    /// acknowledgment is tagged with a generation instead.
    #[test]
    fn a_stale_acknowledgement_from_an_earlier_pause_does_not_satisfy_a_later_one() {
        let gate = InputGate::new();

        // First pause cycle: closes, gets acknowledged, and resumes, the
        // same shape a completed `$EDITOR` session leaves behind.
        let first_acker = gate.clone();
        let first_ack_thread = std::thread::spawn(move || {
            while first_acker.is_open() {
                std::thread::sleep(Duration::from_millis(1));
            }
            first_acker.mark_parked();
        });
        gate.park();
        first_ack_thread.join().unwrap();
        gate.resume();

        // Second pause cycle: nobody acknowledges it. If the first cycle's
        // acknowledgment could satisfy this one, `park()` returns almost
        // immediately instead of blocking.
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = gate.clone();
        let waiter_thread = std::thread::spawn(move || {
            waiter.park();
            let _ = done_tx.send(());
        });

        let returned_on_stale_ack = done_rx.recv_timeout(Duration::from_millis(200)).is_ok();
        assert!(
            !returned_on_stale_ack,
            "park() must not return on an acknowledgment left over from an earlier pause"
        );

        // Satisfy the pending pause so the waiter thread doesn't leak
        // blocked forever, and confirm it actually was still waiting on it.
        gate.mark_parked();
        waiter_thread.join().unwrap();
    }

    /// `InputThreadGuard::drop` has to actually stop and join the reader
    /// thread, not just ask it to stop: a reader still mid `poll`/`read`
    /// when a caller drops the guard and moves on would keep competing for
    /// stdin with whatever the tty is handed to next. Spawns a stand-in
    /// reader loop (the real one needs a tty crossterm can poll, which a
    /// unit test doesn't have) that proves it is still alive by bumping a
    /// counter every iteration, then confirms the counter stops changing at
    /// the moment `drop` returns rather than sometime after.
    #[test]
    fn dropping_the_input_thread_guard_stops_and_joins_the_reader() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let gate = InputGate::new();
        let thread_gate = gate.clone();
        let iterations = Arc::new(AtomicU64::new(0));
        let counter = iterations.clone();
        let handle = std::thread::spawn(move || {
            while !thread_gate.should_stop() {
                counter.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        // Let the stand-in reader actually get into its loop before the
        // guard is dropped, so the assertion below isn't just measuring a
        // thread that hadn't started yet.
        std::thread::sleep(Duration::from_millis(20));

        let guard = InputThreadGuard {
            gate,
            handle: Some(handle),
        };
        drop(guard);

        let at_drop = iterations.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        let after_drop = iterations.load(Ordering::SeqCst);
        assert_eq!(
            at_drop, after_drop,
            "the reader thread must already be stopped by the time drop() returns"
        );
    }

    /// A reader whose own loop has already ended (a read error, stdin
    /// closing) can never acknowledge a pause again; `park()` has to give
    /// up once it learns that, rather than waiting on stdin forever with
    /// the UI thread stuck and the terminal left in raw mode.
    #[test]
    fn park_returns_once_the_reader_reports_it_has_exited_instead_of_hanging_forever() {
        let gate = InputGate::new();
        // Simulates the input thread's own loop having already ended,
        // without ever acknowledging the pause that is about to be
        // requested below.
        gate.mark_exited();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = gate.clone();
        std::thread::spawn(move || {
            waiter.park();
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("park() must return once the reader has exited, rather than spin forever");
    }
}
