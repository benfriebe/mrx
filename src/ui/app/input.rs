//! The input thread and the generation-tagged handshake that stops it
//! touching stdin while another program owns the terminal.

use crossterm::event::Event;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// How often the input thread checks whether it's allowed to touch the tty:
/// its poll timeout while reading, and its sleep while [`InputGate::park`]
/// has paused it for `$EDITOR`. Small enough that resuming feels instant,
/// large enough not to spin.
const GATE_POLL_INTERVAL: Duration = Duration::from_millis(30);

/// Lets the run loop stop the input thread from reading stdin while
/// `$EDITOR` owns the terminal, so mrx and the editor are never both blocked
/// on the same tty at once, and stop it for good on the way out so it isn't
/// still competing for keystrokes with the shell mrx just handed the tty
/// back to.
///
/// `crossterm::read()` can't be interrupted once it's blocked in a syscall,
/// so the thread never calls it unless the gate is open. Closing the gate is
/// not enough on its own: the thread may have already passed its open check
/// and be mid `poll`/`read`, and would still send that stray event. `park`
/// closes the gate and blocks until the thread reports it has reached the
/// paused branch with no read in flight.
///
/// That acknowledgment is tagged with a generation rather than a plain flag:
/// a flag set once and never cleared would let a later `park` return
/// instantly on an earlier pause's leftover. A thread whose loop has ended (a
/// read error, stdin closing) can never acknowledge again, so `park` also
/// returns once the thread reports it has exited.
#[derive(Clone)]
pub(super) struct InputGate {
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
    pub(super) fn park(&self) {
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

    pub(super) fn resume(&self) {
        self.state.lock().unwrap().open = true;
    }

    fn is_open(&self) -> bool {
        self.state.lock().unwrap().open
    }

    /// Called by the input thread from inside the paused branch, to record
    /// that it has parked for whichever pause is current right now.
    fn mark_parked(&self) {
        let mut state = self.state.lock().unwrap();
        state.parked_generation = Some(state.generation);
        drop(state);
        self.condvar.notify_all();
    }

    /// Called once by the input thread's exit path, however its loop ends,
    /// so a `park` waiting on a reader that can never acknowledge again
    /// returns instead of blocking forever.
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
/// for longer than [`GATE_POLL_INTERVAL`] at a time, so it can notice the
/// returned [`InputGate`] closing (for `$EDITOR`) or being asked to stop (on
/// quit) instead of parking in a read that could race a child process, or
/// the just-restored shell, for the same keystrokes. The join handle is how
/// [`run`] waits for the thread to stop before handing the tty back.
pub(super) fn input_thread() -> (
    mpsc::UnboundedReceiver<Event>,
    InputGate,
    std::thread::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let gate = InputGate::new();
    let thread_gate = gate.clone();
    let handle = std::thread::spawn(move || {
        // Marks the gate exited however this loop ends, a panic unwinding
        // out of it included, so a `park` isn't left waiting on a reader
        // that no longer exists to acknowledge it.
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
/// failure. An unjoined thread keeps polling stdin for up to
/// [`GATE_POLL_INTERVAL`] after the terminal is handed back, competing with
/// the shell for the user's next keystroke.
///
/// Declared after `TerminalGuard` in [`run`] so it drops first: reversed,
/// the terminal would already be back in the caller's hands while this
/// thread could still be mid `poll`/`read` racing it for input.
pub(super) struct InputThreadGuard {
    pub(super) gate: InputGate,
    pub(super) handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for InputThreadGuard {
    fn drop(&mut self) {
        self.gate.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
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

    /// `park()` must wait for the reader's own acknowledgment rather than
    /// assume a closed gate means it has stopped. Two full park/resume
    /// cycles, since a single one can't tell the generation-tagged handshake
    /// apart from a plain flag, then the reader exiting for good.
    #[test]
    fn park_blocks_until_the_reader_marks_itself_parked() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let gate = InputGate::new();

        // A fresh single-shot acker per cycle: it only has to notice one
        // gate close, so it can't miss a reopen the way a persistent polling
        // loop racing the main thread's resume/park pair could.
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

    /// A flag set once and never cleared would let the second pause return
    /// on the first's leftover acknowledgment, before the reader has parked
    /// for this one.
    #[test]
    fn a_stale_acknowledgement_from_an_earlier_pause_does_not_satisfy_a_later_one() {
        let gate = InputGate::new();

        // First pause cycle: closes, gets acknowledged, resumes, the same
        // shape a completed `$EDITOR` session leaves behind.
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

    /// `InputThreadGuard::drop` has to join the reader, not just ask it to
    /// stop. The stand-in reader loop bumps a counter every iteration (the
    /// real one needs a tty crossterm can poll, which a unit test lacks), so
    /// the counter must stop changing at the moment `drop` returns.
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

    /// A reader whose loop has already ended can never acknowledge a pause
    /// again; `park()` has to give up once it learns that, rather than
    /// leaving the UI thread stuck with the terminal in raw mode.
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
