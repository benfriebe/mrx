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
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::Repo;
use crate::executor::{self, RunEvent};
use crate::operations;
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

fn apply_mouse_capture(enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        execute!(io::stdout(), DisableMouseCapture)
    }
}

/// `o`: suspend the alternate screen, raw mode, and (if it was on) mouse
/// capture, run `$EDITOR` (falling back to `vi`) on `path` to completion,
/// then restore all three exactly as they were. A blocking wait is the
/// point: there is nothing useful for the app to do while the editor has
/// the terminal, and any probe or run events that arrive in the meantime
/// just sit in their channels until the next draw picks them up, the same
/// eventually-consistent handling every other background result gets.
fn open_editor(terminal: &mut super::Term, path: &Path, mouse_captured: bool) -> io::Result<()> {
    if mouse_captured {
        apply_mouse_capture(false)?;
    }
    super::teardown_terminal()?;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("vi");
    let result = std::process::Command::new(bin)
        .args(parts)
        .arg(path)
        .status();

    *terminal = super::setup_terminal()?;
    if mouse_captured {
        apply_mouse_capture(true)?;
    }
    terminal.clear()?;

    result.map(|_| ())
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
    let mut input = input_thread();
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
            Some(ev) = input.recv() => {
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
                    if let Err(e) = open_editor(&mut terminal, &path, app.mouse_captured) {
                        app.status_message = Some(format!("could not open $EDITOR: {e}"));
                    }
                }
            }
            Some(probed) = probe_rx.recv() => {
                app.on_probe(probed.generation, probed.state);
                if let Some(targets) = app.take_auto_update_requested() {
                    poll::spawn_auto_update(&app.repos, targets, app.jobs, auto_tx.clone());
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

    apply_mouse_capture(false)?;
    super::teardown_terminal()?;
    Ok(())
}
