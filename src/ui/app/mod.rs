//! The resident app: a repo table that stays on screen across runs. This
//! phase is a browsable list with cursor, selection, and filter; the probe,
//! the executor, and the detail view join the event loop in later phases.

pub mod actions;
pub mod keys;
pub mod probe;
pub mod render;
pub mod state;

use crossterm::event::Event;
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::Repo;
use probe::Probed;
use state::App;

/// crossterm's `read()` blocks, so it gets its own thread rather than the
/// `event-stream` feature and a futures dependency. The thread outlives the
/// app: it parks in `read()` until the next input, then the send fails and
/// it ends.
fn input_thread() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    rx
}

/// Begin a new probe generation over the repos `r` should re-check (the
/// selection, or everything when nothing is selected) and spawn it.
fn spawn_reprobe(app: &mut App, tx: &mpsc::UnboundedSender<Probed>) {
    let targets = app.reprobe_targets();
    let generation = app.begin_probe(&targets);
    probe::spawn_probe_generation(&app.repos, targets, app.jobs, generation, tx.clone());
}

/// Open the resident app on `repos` and block until the user quits.
pub async fn run(repos: Vec<Repo>, set_label: String, jobs: usize) -> io::Result<()> {
    super::install_panic_hook();
    let mut terminal = super::setup_terminal()?;

    let mut app = App::new(repos, set_label, jobs);
    let mut input = input_thread();
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    // The first frame paints immediately with placeholders; this probe's
    // results fill the table in as they arrive.
    {
        let targets: Vec<usize> = (0..app.repos.len()).collect();
        let generation = app.begin_probe(&targets);
        probe::spawn_probe_generation(&app.repos, targets, app.jobs, generation, probe_tx.clone());
    }

    terminal.draw(|frame| render::draw(frame, &app))?;

    loop {
        tokio::select! {
            Some(ev) = input.recv() => {
                if keys::on_input(&mut app, ev) {
                    break;
                }
                if app.take_probe_request() {
                    spawn_reprobe(&mut app, &probe_tx);
                }
            }
            Some(probed) = probe_rx.recv() => {
                app.on_probe(probed.generation, probed.state);
            }
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(1);
            }
        }
        terminal.draw(|frame| render::draw(frame, &app))?;
    }

    super::teardown_terminal()?;
    Ok(())
}
