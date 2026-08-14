//! State for the resident app: the repo list, cursor, selection, and filter.
//! Every decision worth testing lives here as a method that returns data;
//! `render.rs` only turns that data into widgets.

use super::actions::{self, Action};
use super::detail;
use super::poll::{self, AutoUpdateOutcome, AutoUpdateResult};
use super::probe::{self, RepoState};
use super::session::Session;
use crate::config::{self, Repo};
use crate::executor::{StepResult, TaskEvent};
use crate::sets;
use crate::summarize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// Shown for a repo that has never taken part in a run this session, per
/// section 02: "a repo that has never been run shows `·` rather than a fake
/// 'pending'".
const NEVER_RUN: &str = "·";

/// How long a result stays on its row before the column goes back to
/// [`NEVER_RUN`]. Long enough to run something, go and read it, and come
/// back; short enough that a table left open all afternoon isn't still
/// reporting this morning.
pub const DEFAULT_RESULT_TTL: Duration = Duration::from_secs(6 * 60);

pub struct App {
    pub repos: Vec<Repo>,
    /// Active set's display name, or `(unnamed)` for a bare config file.
    pub set_label: String,
    /// Default parallelism for runs launched from inside the app. Unused
    /// until the executor work lands in a later phase.
    pub jobs: usize,
    /// Global index into `repos`, always pointing at a visible row.
    pub cursor: usize,
    /// First visible-list position drawn at the top of the table. Kept as
    /// state rather than derived from the cursor so the window only moves
    /// when the cursor would otherwise leave it: a window pinned to the
    /// cursor scrolls on every keypress, which reads as the cursor being
    /// stuck to an edge.
    pub list_scroll: usize,
    pub selected: BTreeSet<usize>,
    pub filter: String,
    /// Whether `/` is currently capturing keystrokes into `filter`.
    pub filtering: bool,
    pub tick: usize,
    /// Latest known probe result per repo, `None` until the first one for
    /// that repo arrives.
    pub probes: Vec<Option<RepoState>>,
    /// Repos with an in-flight probe in the current generation, so a row
    /// shows a spinner instead of stale or blank data.
    pub probing: BTreeSet<usize>,
    /// Bumped every time a probe run starts; a result tagged with an older
    /// generation is dropped rather than painted over newer data (section
    /// 07, "superseded, not queued").
    pub probe_generation: u64,
    /// Global indices of repos known to have fetched this session, either
    /// because a poll cycle's own `git fetch` succeeded for them or because
    /// their `FETCH_HEAD` moved since [`fetch_baseline`](Self::fetch_baseline)
    /// was taken. Until a given repo is in here, its behind column reads
    /// unknown rather than claiming to be current; a repo whose own fetch
    /// keeps failing (offline, VPN, auth) must not borrow another repo's
    /// success just because they polled in the same cycle. Sticky per repo:
    /// a later fetch-less reprobe doesn't downgrade a repo that has
    /// genuinely fetched before.
    pub fetched_repos: BTreeSet<usize>,
    /// Each repo's `FETCH_HEAD` timestamp as of the first probe that saw it,
    /// so a later probe can tell a fetch that happened since from one that
    /// happened days ago. Absent means never probed; `None` means probed and
    /// it had never fetched. This is what credits a fetch mrx did not
    /// perform: an `update` action pulls, and the re-probe that follows sees
    /// the newer timestamp.
    pub fetch_baseline: BTreeMap<usize, Option<SystemTime>>,
    /// Set by the `r` key; the run loop owns actually spawning the probe
    /// task, since `on_key` has no runtime handle to spawn one with.
    pub probe_requested: bool,
    /// Bumped every time a run starts; an executor event tagged with an
    /// older id belongs to a run that's since been cancelled and superseded,
    /// and is dropped by [`on_task`](Self::on_task) rather than painted over
    /// a newer run's results.
    pub run_id: u64,
    /// Each repo's outcome from the most recent run it took part in, `None`
    /// for a repo that has never run this session.
    pub run_results: Vec<Option<RunStatus>>,
    /// Output arriving from runs still in flight, keyed by repo. Dropped as
    /// soon as that repo's `Finished` lands, which carries the same text.
    pub live: BTreeMap<usize, LiveRun>,
    /// When each repo's result last changed, so
    /// [`expire_results`](Self::expire_results) can clear a stale one.
    pub result_at: BTreeMap<usize, Instant>,
    /// How long a finished result stays on the row before the column goes
    /// back to `·`. `None` keeps every result until the next run replaces
    /// it. A result is a statement about a moment, and an hour later it is
    /// mostly a claim about the past dressed as the present.
    pub result_ttl: Option<Duration>,
    /// `[DEFAULT]` keys for the active config, so a run started from inside
    /// the app can plan an operation the same way the CLI does.
    pub defaults: BTreeMap<String, String>,
    /// The active config's path, passed through to the executor for
    /// `MR_CONFIG` and unrelated to which set is on screen.
    pub config_path: PathBuf,
    /// `-d`, carried along so a config reload or set switch resolves repo
    /// paths the same way the initial load did.
    pub dir_override: Option<PathBuf>,
    /// Skip the dirty-selection confirmation (section 11); mirrors the
    /// CLI's `--force`, since both mean "I already know what I'm doing".
    pub force: bool,
    /// Every runnable action for the active set, discovered once at
    /// startup so the palette has something to filter (section 08).
    pub actions: Vec<Action>,

    /// Whether the action palette (`:`) is capturing keystrokes.
    pub palette_open: bool,
    pub palette_filter: String,
    pub palette_cursor: usize,

    /// A run waiting on the dirty-selection confirmation; `None` once it's
    /// been answered either way.
    pub pending_run: Option<PendingRun>,
    /// Set once a run should actually start; the run loop owns spawning it,
    /// since state has no runtime handle of its own (mirrors
    /// `probe_requested`).
    pub run_requested: Option<RunRequest>,

    /// The live run's action name, `None` once it finishes so the header
    /// falls back to the plain selection count.
    pub run_action: Option<String>,
    /// Global indices the live (or just-finished) run covers.
    pub run_targets: Vec<usize>,
    pub run_total: usize,
    pub run_completed: usize,
    pub run_failed: usize,
    /// Set once a run finishes, to the repos it covered; the run loop owns
    /// spawning the re-probe over them (section 06, "post-run re-probe").
    pub post_run_targets: Option<Vec<usize>>,

    /// Whether Enter has opened the detail view for the cursor row. The
    /// view always follows `cursor`, so no separate "which repo" field.
    pub detail_open: bool,
    /// Each repo's scroll position in its own detail view, kept per repo
    /// (keyed by global index) so paging through rows with j/k doesn't
    /// lose your place when you come back.
    pub detail_scroll: BTreeMap<usize, usize>,

    /// Whether the mouse is currently captured; starts `true` (section 03).
    pub mouse_captured: bool,
    /// Set by `m`; the run loop applies the actual capture toggle, since
    /// that's a terminal write and state has no I/O of its own.
    pub mouse_capture_dirty: bool,
    /// Whether the modifier-drag escape hatch has already been mentioned
    /// this session, so it's shown once rather than on every drag.
    pub drag_hint_shown: bool,

    /// A one-line transient message shown in the status bar in place of the
    /// keymap hint, e.g. what `y` just did.
    pub status_message: Option<String>,
    /// The frame size as of the last draw, so half-page scrolling in the
    /// detail view approximates the page actually on screen.
    pub terminal_width: u16,
    pub terminal_height: u16,

    /// Whether the set picker (`tab`) is capturing keystrokes.
    pub set_picker_open: bool,
    /// Entries offered by the open picker: every set `sets::discover()`
    /// finds, plus the active config appended as `(unnamed)` when it isn't
    /// one of them (section 11, "Unnamed active config"). Rebuilt each time
    /// the picker opens, so a set created on disk since startup shows up.
    pub set_entries: Vec<SetEntry>,
    pub set_picker_cursor: usize,

    /// Set by a config reload or a set switch, both of which replace the
    /// repo list out from under the probe; the run loop owns spawning the
    /// resulting full re-probe, since state has no runtime handle of its own.
    pub full_reprobe_requested: bool,

    /// Set by `Esc` while a run is live; the run loop owns actually flipping
    /// the executor's cancel flag, since only it holds the `RunHandle`.
    pub cancel_requested: bool,

    /// Whether `q`/`Ctrl-C` is waiting on confirmation because a run is live
    /// (section 03, "prompts if a run is live").
    pub quit_pending: bool,

    /// `?`: whether the keymap overlay is showing. Purely a view concern, so
    /// it gates nothing and blocks no operation.
    pub help_open: bool,
    /// Which half of the split `j`/`k` drive. Meaningless with the detail
    /// view closed, and reset every time it opens, so `tab` never leaves
    /// focus somewhere the next open would inherit.
    pub focus: Pane,

    /// `o` and `!`: what to run in the foreground once the app has stepped
    /// out of the way. The run loop owns actually suspending the terminal,
    /// since state has no I/O of its own (mirrors `probe_requested`,
    /// `mouse_capture_dirty`).
    pub foreground: Option<Foreground>,

    /// `F`: whether the freshness poll is currently on. Off by default,
    /// since freshness is an opt-in loop (section 07).
    pub poll_enabled: bool,
    /// How often the poll fires when it's on; a value on `App` rather than
    /// a hardcoded constant, so a persisted session can change it (section
    /// 07: "the interval is a config value").
    pub poll_interval: Duration,
    /// `Ctrl-A`: whether a completed poll cycle is allowed to fast-forward
    /// what it finds behind. Off by default; never true while `poll_enabled`
    /// is false, since it has nothing to act on without one.
    pub auto_update: bool,
    /// Set by [`on_poll_due`](Self::on_poll_due) when a tick is actually
    /// allowed to start a cycle; the run loop owns spawning it, since state
    /// has no runtime handle of its own (mirrors `probe_requested`).
    poll_targets_requested: Option<Vec<usize>>,
    /// The probe generation the current in-flight probe belongs to, when it
    /// was started as a poll cycle rather than a plain probe or reprobe.
    /// Lets [`on_probe`](Self::on_probe) tell "a poll cycle just finished"
    /// from "the user pressed r", without a second copy of the generation
    /// machinery (section 02: "the existing generation counter").
    poll_generation: Option<u64>,
    /// Set once a poll cycle's results are all in and some of them passed
    /// [`poll::can_fast_forward`]; the run loop owns spawning the actual
    /// merges.
    auto_update_requested: Option<Vec<usize>>,
    /// Bumped every time an auto-update cycle actually starts (mirrors
    /// `probe_generation`); a result tagged with a different generation
    /// belongs to a cycle that has since completed or been superseded and
    /// is dropped rather than corrupting the current cycle's counters.
    auto_update_generation: u64,
    auto_update_total: usize,
    auto_update_done: usize,
    auto_update_ok: usize,
    /// Repos an auto-update pass actually fast-forwarded, so the run loop
    /// can re-probe just those once the pass finishes and pick up their new
    /// ahead/behind and branch state.
    auto_update_reprobe_targets: Option<Vec<usize>>,
}

/// One row in the set picker: a discovered set's name and the config path
/// it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetEntry {
    pub name: String,
    pub path: PathBuf,
}

/// A run waiting on user confirmation because part of its target selection
/// is dirty, or because part of it has no probe result yet and dirtiness is
/// simply unknown (section 11, "destructive actions one keystroke away").
pub struct PendingRun {
    pub action: String,
    pub targets: Vec<usize>,
    pub dirty_count: usize,
    pub unknown_count: usize,
}

/// A run that's been decided on (no confirmation needed, or confirmed) and
/// is ready for the run loop to plan and spawn.
pub struct RunRequest {
    pub action: String,
    pub targets: Vec<usize>,
}

/// Which pane of the detail split has the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The repo list. `j`/`k` move the cursor and the output follows.
    List,
    /// The output. `j`/`k` scroll it and the cursor stays put.
    Output,
}

impl Pane {
    fn other(self) -> Self {
        match self {
            Pane::List => Pane::Output,
            Pane::Output => Pane::List,
        }
    }
}

/// A pending request to hand the terminal to something else. Resolved
/// against the cursor when the run loop takes it, not when it is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Foreground {
    /// `$EDITOR` on the cursor row's repo.
    Repo,
    /// `$EDITOR` on a file already written, such as a run transcript.
    Path(PathBuf),
    /// `$SHELL` in the cursor row's repo.
    Shell,
}

/// A resolved [`Foreground`]: the same request with the cursor's repo
/// already looked up, which is all the run loop needs.
pub enum Suspend {
    Editor(PathBuf),
    Shell(PathBuf),
}

/// A run's output as it arrives, before `Finished` delivers the whole
/// thing. Each entry is one step, in the order the chain runs them, holding
/// only the lines seen so far.
#[derive(Debug, Default, Clone)]
pub struct LiveRun {
    pub steps: Vec<StepResult>,
}

impl LiveRun {
    /// Open a section for a step that has just started.
    fn begin(&mut self, label: &str) {
        self.steps.push(StepResult {
            label: label.to_string(),
            shape: summarize::Shape::Generic,
            stdout: String::new(),
            stderr: String::new(),
            // Nothing to report yet; the finished result carries the real
            // code, and [`crate::ui::app::detail`] draws a live step's
            // heading without one.
            code: 0,
        });
    }

    /// Append one line to `step`. A line for a step that never announced
    /// itself is dropped rather than inventing a section for it.
    fn push(&mut self, step: usize, stderr: bool, line: &str) {
        let Some(slot) = self.steps.get_mut(step) else {
            return;
        };
        let text = if stderr {
            &mut slot.stderr
        } else {
            &mut slot.stdout
        };
        text.push_str(line);
        text.push('\n');
    }
}

/// A repo's outcome from the most recent run it took part in.
#[derive(Debug, Clone)]
pub enum RunStatus {
    Running,
    /// The step currently in flight, named so a row reads "post_update"
    /// instead of a fixed "running..." (section 06).
    Step {
        label: String,
    },
    Finished {
        steps: Vec<StepResult>,
        exit_code: i32,
    },
    Skipped {
        reason: String,
    },
}

impl App {
    pub fn new(
        repos: Vec<Repo>,
        set_label: String,
        jobs: usize,
        defaults: BTreeMap<String, String>,
        config_path: PathBuf,
        force: bool,
        dir_override: Option<PathBuf>,
    ) -> Self {
        let n = repos.len();
        let actions = actions::discover(&repos, &defaults);
        Self {
            repos,
            set_label,
            jobs,
            cursor: 0,
            list_scroll: 0,
            selected: BTreeSet::new(),
            filter: String::new(),
            filtering: false,
            tick: 0,
            probes: vec![None; n],
            probing: BTreeSet::new(),
            probe_generation: 0,
            fetched_repos: BTreeSet::new(),
            fetch_baseline: BTreeMap::new(),
            probe_requested: false,
            run_id: 0,
            run_results: vec![None; n],
            live: BTreeMap::new(),
            result_at: BTreeMap::new(),
            result_ttl: Some(DEFAULT_RESULT_TTL),
            defaults,
            config_path,
            dir_override,
            force,
            actions,
            palette_open: false,
            palette_filter: String::new(),
            palette_cursor: 0,
            pending_run: None,
            run_requested: None,
            run_action: None,
            run_targets: Vec::new(),
            run_total: 0,
            run_completed: 0,
            run_failed: 0,
            post_run_targets: None,
            detail_open: false,
            detail_scroll: BTreeMap::new(),
            mouse_captured: true,
            mouse_capture_dirty: false,
            drag_hint_shown: false,
            status_message: None,
            terminal_width: 80,
            terminal_height: 24,
            set_picker_open: false,
            set_entries: Vec::new(),
            set_picker_cursor: 0,
            full_reprobe_requested: false,
            cancel_requested: false,
            quit_pending: false,
            help_open: false,
            foreground: None,
            focus: Pane::List,
            poll_enabled: false,
            poll_interval: poll::DEFAULT_POLL_INTERVAL,
            auto_update: false,
            poll_targets_requested: None,
            poll_generation: None,
            auto_update_requested: None,
            auto_update_generation: 0,
            auto_update_total: 0,
            auto_update_done: 0,
            auto_update_ok: 0,
            auto_update_reprobe_targets: None,
        }
    }

    /// Apply a persisted session on top of a freshly built app (section 09).
    /// Filter and selection are matched by repo name so a config edit
    /// doesn't misdirect them onto the wrong row, and any name the current
    /// repo list doesn't have is dropped silently, a config edit is not an
    /// error. `set_label` is left untouched: `main.rs` already decided which
    /// config to load, `-s` beating the stored set if it named one, before
    /// this ever runs.
    pub fn restore_session(&mut self, session: &Session) {
        self.filter = session.filter.clone();

        self.selected = self
            .repos
            .iter()
            .enumerate()
            .filter(|(_, r)| session.selected.contains(&r.name))
            .map(|(i, _)| i)
            .collect();

        if let Some(name) = &session.cursor {
            if let Some(pos) = self.repos.iter().position(|r| &r.name == name) {
                self.cursor = pos;
            }
        }
        self.clamp_cursor_to_visible();

        if let Some(interval) = session.poll_interval {
            self.poll_enabled = true;
            self.poll_interval = interval;
        }
        // Never restore auto-update without the poll it depends on, even if
        // the file somehow has that combination.
        self.auto_update = session.auto_update && self.poll_enabled;
    }

    /// Start a new run generation and return its id, so the caller can tag
    /// the `spawn_run` call it is about to make with it.
    pub fn begin_run(&mut self) -> u64 {
        self.run_id += 1;
        self.run_id
    }

    /// Begin a named run over `targets`: bumps the run id and resets the
    /// live counters the header reads, so the caller only has to plan and
    /// spawn the operations themselves.
    pub fn begin_named_run(&mut self, action: String, targets: Vec<usize>) -> u64 {
        let run_id = self.begin_run();
        self.run_action = Some(action);
        self.run_total = targets.len();
        self.run_completed = 0;
        self.run_failed = 0;
        self.run_targets = targets;
        run_id
    }

    /// Apply one executor event, unless it belongs to a run a later one has
    /// since superseded. Once every target has reported in, clears the live
    /// run and records its targets in `post_run_targets` for a re-probe.
    pub fn on_task(&mut self, run_id: u64, event: TaskEvent) {
        if run_id != self.run_id {
            return;
        }
        let (index, status) = match event {
            TaskEvent::Started { index } => {
                self.live.remove(&index);
                (index, RunStatus::Running)
            }
            TaskEvent::Step { index, label } => {
                self.live.entry(index).or_default().begin(&label);
                (index, RunStatus::Step { label })
            }
            TaskEvent::Output {
                index,
                step,
                stderr,
                line,
            } => {
                if let Some(live) = self.live.get_mut(&index) {
                    live.push(step, stderr, &line);
                }
                return;
            }
            TaskEvent::Finished {
                index,
                steps,
                exit_code,
            } => {
                // The finished steps supersede the partial ones, which are
                // the same text either way; keeping both would only be a
                // second answer to the same question.
                self.live.remove(&index);
                (index, RunStatus::Finished { steps, exit_code })
            }
            TaskEvent::Skipped { index, reason } => {
                self.live.remove(&index);
                (index, RunStatus::Skipped { reason })
            }
        };
        self.result_at.insert(index, Instant::now());

        let counts_toward_completion = matches!(
            status,
            RunStatus::Finished { .. } | RunStatus::Skipped { .. }
        );
        let failed = matches!(&status, RunStatus::Finished { exit_code, .. } if *exit_code != 0);

        if let Some(slot) = self.run_results.get_mut(index) {
            *slot = Some(status);
        }

        if counts_toward_completion {
            self.run_completed += 1;
            if failed {
                self.run_failed += 1;
            }
            if self.run_total > 0 && self.run_completed == self.run_total {
                self.post_run_targets = Some(std::mem::take(&mut self.run_targets));
                self.run_action = None;
            }
        }
    }

    /// Drop results older than [`result_ttl`](Self::result_ttl), so a row
    /// stops reporting an outcome from long enough ago that it says nothing
    /// about the repo now. Called on the tick; a run still in flight is
    /// never touched, since it has not finished saying anything yet.
    pub fn expire_results(&mut self) {
        let Some(ttl) = self.result_ttl else {
            return;
        };
        if self.run_action.is_some() {
            return;
        }
        let now = Instant::now();
        let stale: Vec<usize> = self
            .result_at
            .iter()
            .filter(|(_, &at)| now.duration_since(at) >= ttl)
            .map(|(&index, _)| index)
            .collect();
        for index in stale {
            self.result_at.remove(&index);
            self.detail_scroll.remove(&index);
            if let Some(slot) = self.run_results.get_mut(index) {
                *slot = None;
            }
        }
    }

    /// Set by `on_task` once a run's last target reports in; consumed by
    /// the run loop, the only thing with a runtime handle to spawn the
    /// resulting probe with.
    pub fn take_post_run_targets(&mut self) -> Option<Vec<usize>> {
        self.post_run_targets.take()
    }

    /// `Esc`: ask the live run to stop queueing new work, and say honestly
    /// what that will and won't do (section 06). A no-op when nothing is
    /// running, so it's safe to bind unconditionally.
    ///
    /// `Command::output().await` has no kill, so a repo already past its
    /// semaphore permit keeps running to completion; only a repo still
    /// waiting behind it turns into a skip. Both counts are a snapshot of
    /// `run_results` as of the keypress, not a promise that stays accurate
    /// as more events arrive.
    pub fn request_cancel(&mut self) {
        if self.run_action.is_none() {
            return;
        }
        let (queued, finishing) = self.cancel_counts();
        self.status_message = Some(format!(
            "cancelled, {queued} queued skipped, {finishing} still finishing"
        ));
        self.cancel_requested = true;
    }

    /// How many of the live run's targets haven't reported in yet, split
    /// into those with no result at all (still queued, about to be skipped)
    /// and those already `Running`/`Step` (already past their permit, will
    /// run to completion regardless of the cancel flag).
    fn cancel_counts(&self) -> (usize, usize) {
        let mut queued = 0;
        let mut finishing = 0;
        for &i in &self.run_targets {
            match self.run_results.get(i).and_then(|r| r.as_ref()) {
                None => queued += 1,
                Some(RunStatus::Running) | Some(RunStatus::Step { .. }) => finishing += 1,
                _ => {}
            }
        }
        (queued, finishing)
    }

    /// Set by [`request_cancel`](Self::request_cancel); consumed by the run
    /// loop, the only thing holding the `RunHandle` that can actually flip
    /// the executor's cancel flag.
    pub fn take_cancel_requested(&mut self) -> bool {
        std::mem::take(&mut self.cancel_requested)
    }

    /// `q`/`Ctrl-C`: quits immediately, unless a run is live, in which case
    /// it opens a confirmation first rather than losing sight of whether
    /// anything was left running (section 03, "prompts if a run is live").
    /// Returns whether the caller should quit right now.
    pub fn request_quit(&mut self) -> bool {
        if self.run_action.is_some() {
            self.quit_pending = true;
            false
        } else {
            true
        }
    }

    /// Confirmed: the caller should quit now.
    pub fn confirm_quit(&mut self) -> bool {
        self.quit_pending = false;
        true
    }

    /// Declined: stay open.
    pub fn cancel_quit(&mut self) {
        self.quit_pending = false;
    }

    /// Whether an auto-update pass has merges in flight: an auto-update
    /// result carries a repo index, and anything that replaces `repos` or
    /// starts a second `git` invocation against the same working tree would
    /// race it.
    fn auto_update_in_flight(&self) -> bool {
        self.auto_update_total > 0
    }

    /// Whichever mutating operation currently owns the repos on disk, if
    /// any. A live run, an in-flight auto-update pass, a set switch, a
    /// config reload, and an editor suspension all either write to a
    /// working tree or replace the repo list out from under indices another
    /// of them is still using, so at most one may be underway at a time.
    /// The single source of truth for that rule: every entry point that
    /// commits one of these actions calls this at the moment it actually
    /// commits, not only when it opens a modal, since another one can start
    /// in the gap between the two.
    fn mutation_blocker(&self) -> Option<&'static str> {
        if self.run_action.is_some() {
            Some("another run is already live")
        } else if self.auto_update_in_flight() {
            Some("auto-update is running")
        } else {
            None
        }
    }

    /// Refuses `verb` with a status message naming what's blocking it, if
    /// anything is; returns whether it refused.
    fn refuse_if_mutation_blocked(&mut self, verb: &str) -> bool {
        if let Some(reason) = self.mutation_blocker() {
            self.status_message = Some(format!("can't {verb} while {reason}"));
            true
        } else {
            false
        }
    }

    /// `tab`: open the set picker. Blocked by [`mutation_blocker`](Self::mutation_blocker),
    /// since switching the repo list out from under a live run's or
    /// auto-update's indices would attribute its results to the wrong rows.
    pub fn open_set_picker(&mut self) {
        if self.refuse_if_mutation_blocked("switch sets") {
            return;
        }
        let mut entries: Vec<SetEntry> = sets::discover()
            .into_iter()
            .map(|(name, path)| SetEntry { name, path })
            .collect();
        // `discover` only returns sets that exist on disk under a name; the
        // active config may be neither, e.g. an implicit default or `-c`.
        // Matches `print_sets`' handling of the same case.
        if !entries.iter().any(|e| e.path == self.config_path) {
            entries.push(SetEntry {
                name: "(unnamed)".into(),
                path: self.config_path.clone(),
            });
        }
        self.set_picker_cursor = entries
            .iter()
            .position(|e| e.path == self.config_path)
            .unwrap_or(0);
        self.set_entries = entries;
        self.set_picker_open = true;
    }

    pub fn close_set_picker(&mut self) {
        self.set_picker_open = false;
    }

    pub fn set_picker_move(&mut self, delta: isize) {
        let n = self.set_entries.len();
        if n == 0 {
            return;
        }
        let next = (self.set_picker_cursor as isize + delta).clamp(0, n as isize - 1) as usize;
        self.set_picker_cursor = next;
    }

    /// Confirm the highlighted set: load its config and switch to it, with
    /// a full re-probe (section 03, "switching reloads the config and
    /// restarts the probe").
    ///
    /// Re-checks [`mutation_blocker`](Self::mutation_blocker) here rather
    /// than trusting the check [`open_set_picker`](Self::open_set_picker)
    /// made when the picker opened: a run or an auto-update pass can start
    /// in the time the picker sat open waiting for Enter, and switching sets
    /// out from under either would hand their in-flight results indices
    /// belonging to the wrong repo list.
    ///
    /// Uses [`config::try_load`] rather than `config::load`: this runs with
    /// raw mode, the alternate screen, and mouse capture all
    /// active, so a `std::process::exit` here would skip teardown and the
    /// panic hook and leave the terminal wrecked. An unreadable or
    /// unparseable config keeps the set currently open and reports the
    /// error instead.
    pub fn confirm_set_picker(&mut self) {
        let Some(entry) = self.set_entries.get(self.set_picker_cursor).cloned() else {
            self.close_set_picker();
            return;
        };
        self.close_set_picker();
        if self.refuse_if_mutation_blocked("switch sets") {
            return;
        }
        match config::try_load(&entry.path, self.dir_override.as_deref()) {
            Ok(config::Config {
                repos, defaults, ..
            }) => {
                self.set_label = entry.name;
                self.reconcile_repos(repos, defaults, entry.path);
            }
            Err(e) => {
                self.status_message = Some(format!("could not switch sets: {e}"));
            }
        }
    }

    /// `Ctrl-R`: re-read the active config from disk without changing which
    /// config is active. Blocked by
    /// [`mutation_blocker`](Self::mutation_blocker), the same guard
    /// [`open_set_picker`](Self::open_set_picker) uses.
    ///
    /// Uses [`config::try_load`] rather than `config::load` for the same
    /// reason [`confirm_set_picker`](Self::confirm_set_picker) does: this runs
    /// with the terminal in raw mode, so exiting the
    /// process here bypasses teardown. A bad edit mid-save keeps the current
    /// config loaded and reports the error rather than killing the app.
    pub fn reload_config(&mut self) {
        if self.refuse_if_mutation_blocked("reload") {
            return;
        }
        match config::try_load(&self.config_path, self.dir_override.as_deref()) {
            Ok(config::Config {
                repos, defaults, ..
            }) => {
                let config_path = self.config_path.clone();
                self.reconcile_repos(repos, defaults, config_path);
            }
            Err(e) => {
                self.status_message = Some(format!("could not reload config: {e}"));
            }
        }
    }

    /// Replace the repo list after a config reload or set switch, carrying
    /// the cursor and selection across by repo NAME rather than index: a
    /// repo added above the one you're on must not silently redirect a
    /// selection onto its neighbour, and a name the edit removed just drops
    /// out. Every index-keyed piece of state (probes, run results, detail
    /// scroll) starts over, since none of it means anything against a
    /// different repo list.
    fn reconcile_repos(
        &mut self,
        repos: Vec<Repo>,
        defaults: BTreeMap<String, String>,
        config_path: PathBuf,
    ) {
        let cursor_name = self.repos.get(self.cursor).map(|r| r.name.clone());
        let selected_names: BTreeSet<String> = self
            .selected
            .iter()
            .filter_map(|&i| self.repos.get(i).map(|r| r.name.clone()))
            .collect();

        let n = repos.len();
        self.actions = actions::discover(&repos, &defaults);
        self.repos = repos;
        self.defaults = defaults;
        self.config_path = config_path;

        self.probes = vec![None; n];
        self.probing.clear();
        self.fetched_repos.clear();
        self.fetch_baseline.clear();
        self.run_results = vec![None; n];
        self.detail_scroll.clear();

        self.selected = self
            .repos
            .iter()
            .enumerate()
            .filter(|(_, r)| selected_names.contains(&r.name))
            .map(|(i, _)| i)
            .collect();

        self.cursor = cursor_name
            .and_then(|name| self.repos.iter().position(|r| r.name == name))
            .unwrap_or(0);
        self.clamp_cursor_to_visible();

        self.full_reprobe_requested = true;
    }

    /// Set by a config reload or set switch; consumed by the run loop,
    /// which is the only thing with a runtime handle to spawn the resulting
    /// probe with.
    pub fn take_full_reprobe_request(&mut self) -> bool {
        std::mem::take(&mut self.full_reprobe_requested)
    }

    /// How many of `targets` the last probe found dirty. A repo with no
    /// probe result yet is not counted here, it is unknown rather than
    /// clean; see [`unprobed_count`](Self::unprobed_count) for that half of
    /// the confirmation decision.
    pub fn dirty_count(&self, targets: &[usize]) -> usize {
        targets
            .iter()
            .filter(|&&i| {
                self.probes
                    .get(i)
                    .and_then(|p| p.as_ref())
                    .is_some_and(|s| s.changed > 0)
            })
            .count()
    }

    /// How many of `targets` have no probe result at all yet: right after
    /// startup, a set switch, or a config reload, every repo starts this
    /// way. Treated the same as dirty for the confirmation in
    /// [`request_run`](Self::request_run), since a repo the app hasn't
    /// actually looked at could be dirty and running unconfirmed against it
    /// would defeat the point of the confirmation (section 11).
    pub fn unprobed_count(&self, targets: &[usize]) -> usize {
        targets
            .iter()
            .filter(|&&i| self.probes.get(i).and_then(|p| p.as_ref()).is_none())
            .count()
    }

    /// Ask to run `action_name` over the effective selection. Debug-asserts
    /// the action is actually defined somewhere: the palette must never
    /// have offered it otherwise (section 08). Refused by
    /// [`mutation_blocker`](Self::mutation_blocker), the same guard
    /// `open_set_picker`/`reload_config` use, since a manual run sharing a
    /// repo with a live auto-update pass would run `git` against it through
    /// two different semaphores at once. Goes straight to `run_requested`
    /// when nothing in the target selection is dirty or unprobed, or when
    /// `force` is set; otherwise waits on confirmation.
    pub fn request_run(&mut self, action_name: &str) {
        debug_assert!(
            self.actions.iter().any(|a| a.name == action_name),
            "the palette must never offer an action nothing defines: {action_name}"
        );
        if self.refuse_if_mutation_blocked("start a run") {
            return;
        }
        let targets = self.effective_selection();
        if targets.is_empty() {
            self.status_message = Some(self.no_visible_rows_message());
            return;
        }
        let dirty = self.dirty_count(&targets);
        let unknown = self.unprobed_count(&targets);
        if (dirty > 0 || unknown > 0) && !self.force {
            self.pending_run = Some(PendingRun {
                action: action_name.to_string(),
                targets,
                dirty_count: dirty,
                unknown_count: unknown,
            });
        } else {
            self.run_requested = Some(RunRequest {
                action: action_name.to_string(),
                targets,
            });
        }
    }

    /// Confirm a pending run. Re-checks
    /// [`mutation_blocker`](Self::mutation_blocker) rather than trusting the
    /// check [`request_run`](Self::request_run) made when the confirmation
    /// prompt opened: an auto-update pass can start while the prompt sat
    /// waiting for an answer, and the pending run is dropped rather than
    /// launched into a repo an auto-update pass already owns.
    pub fn confirm_pending_run(&mut self) {
        let Some(p) = self.pending_run.take() else {
            return;
        };
        if self.refuse_if_mutation_blocked("start a run") {
            return;
        }
        self.run_requested = Some(RunRequest {
            action: p.action,
            targets: p.targets,
        });
    }

    pub fn cancel_pending_run(&mut self) {
        self.pending_run = None;
    }

    /// Set by a confirmed or unconfirmed-but-clean [`request_run`]; consumed
    /// by the run loop, the only thing with a runtime handle to plan and
    /// spawn the resulting operations with.
    pub fn take_run_requested(&mut self) -> Option<RunRequest> {
        self.run_requested.take()
    }

    /// The result column's text for a row: a summary once the repo's most
    /// recent run has finished, the live step label while one is running or
    /// queued, or [`NEVER_RUN`] for a repo that hasn't taken part in a run
    /// this session.
    pub fn result_text(&self, idx: usize) -> String {
        match self.run_results.get(idx).and_then(|r| r.as_ref()) {
            None => NEVER_RUN.into(),
            Some(RunStatus::Running) => "running".into(),
            Some(RunStatus::Step { label }) => label.clone(),
            Some(RunStatus::Skipped { reason }) => reason.clone(),
            Some(RunStatus::Finished { steps, exit_code }) => {
                summarize::summarize_steps(steps, *exit_code)
            }
        }
    }

    /// Repos to re-probe for `r`: the selection, or everything when nothing
    /// is selected. The same reading of an empty selection as
    /// `effective_selection` uses, since "probe what I'm about to act on" is
    /// the useful interpretation here too.
    pub fn reprobe_targets(&self) -> Vec<usize> {
        if self.selected.is_empty() {
            (0..self.repos.len()).collect()
        } else {
            self.selected.iter().copied().collect()
        }
    }

    /// Start a new probe generation over `targets`: bumps the counter, marks
    /// every target in-flight, and returns the generation so the caller can
    /// tag the probe it is about to spawn with it.
    pub fn begin_probe(&mut self, targets: &[usize]) -> u64 {
        self.probe_generation += 1;
        self.probing = targets.iter().copied().collect();
        self.probe_generation
    }

    /// Apply one probe result, unless it belongs to a generation a later
    /// probe has since superseded.
    pub fn on_probe(&mut self, generation: u64, state: RepoState) {
        if generation < self.probe_generation {
            return;
        }
        self.probing.remove(&state.index);
        // Sticky per repo, and set as soon as this one result lands rather
        // than waiting on the rest of the cycle: a repo whose own fetch
        // failed must not borrow another repo's success.
        if state.fetched || self.fetch_head_moved(&state) {
            self.fetched_repos.insert(state.index);
        }
        if let Some(slot) = self.probes.get_mut(state.index) {
            *slot = Some(state);
        }
        self.maybe_complete_poll(generation);
    }

    /// Whether this result's `FETCH_HEAD` is newer than the one the first
    /// probe of the session recorded, which means something fetched the repo
    /// meanwhile. Records the baseline on the way through, so the first
    /// sighting is never itself treated as a fetch: mrx has no idea how old
    /// a timestamp it has only just read for the first time is.
    fn fetch_head_moved(&mut self, state: &RepoState) -> bool {
        match self.fetch_baseline.entry(state.index) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(state.fetch_head);
                false
            }
            std::collections::btree_map::Entry::Occupied(baseline) => {
                state.fetch_head > *baseline.get()
            }
        }
    }

    /// The tick loop's poll `Interval` arm always fires; this is what
    /// decides whether a given tick actually does anything (section 05,
    /// "a timer that exists but does nothing is cheaper to reason about
    /// than one that gets created and dropped as the mode toggles").
    /// Suspended rather than queued while a run is live (section 02): a
    /// fetch storm competing with a live update for the network is worse
    /// than a poll landing a cycle late.
    pub fn on_poll_due(&mut self) {
        if !self.poll_enabled || self.run_action.is_some() {
            return;
        }
        let targets: Vec<usize> = (0..self.repos.len()).collect();
        let generation = self.begin_probe(&targets);
        self.poll_generation = Some(generation);
        self.poll_targets_requested = Some(targets);
    }

    /// Set by [`on_poll_due`](Self::on_poll_due); consumed by the run loop,
    /// the only thing with a runtime handle to spawn the resulting fetch
    /// with.
    pub fn take_poll_requested(&mut self) -> Option<Vec<usize>> {
        self.poll_targets_requested.take()
    }

    /// `F`: turn the freshness poll on or off. The interval stays whatever
    /// it last was rather than resetting to the default every time. Turning
    /// it off also turns auto-update off: a fast-forward pass with nothing
    /// feeding it fresh data has nothing to act on (section 02).
    pub fn toggle_poll(&mut self) {
        self.poll_enabled = !self.poll_enabled;
        if !self.poll_enabled {
            self.auto_update = false;
        }
    }

    /// `Ctrl-A`: turn auto-update on or off. Refuses while the poll itself
    /// is off, since auto-update only ever acts on a poll's results
    /// (section 02: "after a poll, pull the repos that came back behind").
    pub fn toggle_auto_update(&mut self) {
        if !self.poll_enabled {
            self.status_message = Some("auto-update needs the freshness poll on first".into());
            return;
        }
        self.auto_update = !self.auto_update;
    }

    /// Once every repo a poll cycle covered has reported back, decide which
    /// ones auto-update is allowed to touch. A plain probe or reprobe never
    /// sets `poll_generation`, so this is a no-op for those.
    fn maybe_complete_poll(&mut self, generation: u64) {
        if self.poll_generation != Some(generation) || !self.probing.is_empty() {
            return;
        }
        self.poll_generation = None;
        // Per-repo freshness is already recorded as each result lands (see
        // `on_probe`); nothing left to do here for that. A plain probe
        // reaching this point never got here at all, since it never set
        // `poll_generation` to begin with.
        if !self.auto_update {
            return;
        }
        // A run that started after this poll began but before it finished
        // suspends the fast-forward pass the same way `on_poll_due` would
        // have suspended the poll itself from starting (section 02: "both
        // suspend while a run is live").
        if self.run_action.is_some() {
            return;
        }
        // Refuse to start a second cycle on top of one still in flight: a
        // late result from the first would otherwise land against the
        // second cycle's counters.
        if self.auto_update_in_flight() {
            return;
        }
        // `s.fetched` restricts eligibility to repos whose fetch actually
        // succeeded in this cycle; a repo whose fetch failed keeps whatever
        // stale ahead/behind it already had and must not be trusted for a
        // merge on the strength of it.
        let targets: Vec<usize> = self
            .probes
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                p.as_ref()
                    .filter(|s| poll::can_fast_forward(s) && s.fetched)
                    .map(|_| i)
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        self.auto_update_generation += 1;
        self.auto_update_total = targets.len();
        self.auto_update_done = 0;
        self.auto_update_ok = 0;
        self.auto_update_requested = Some(targets);
    }

    /// The generation the current in-flight auto-update cycle was tagged
    /// with; consumed by the run loop to tag the `spawn_auto_update` call it
    /// is about to make.
    pub fn auto_update_generation(&self) -> u64 {
        self.auto_update_generation
    }

    /// Set by [`maybe_complete_poll`](Self::maybe_complete_poll); consumed
    /// by the run loop, the only thing with a runtime handle to spawn the
    /// resulting merges with.
    pub fn take_auto_update_requested(&mut self) -> Option<Vec<usize>> {
        self.auto_update_requested.take()
    }

    /// Apply one repo's outcome from an auto-update pass, unless it belongs
    /// to a cycle a later one has since superseded (a late
    /// result from an old cycle must not corrupt a new one's counters).
    /// Once every targeted repo has reported in, leaves an honest one-line
    /// summary in the status bar: repos a fast-forward could not touch are
    /// reported, not fixed (section 02), and are simply left out of the
    /// count rather than named individually here.
    pub fn on_auto_update_result(&mut self, result: AutoUpdateResult) {
        if result.generation != self.auto_update_generation {
            return;
        }
        self.auto_update_done += 1;
        let fast_forwarded = matches!(result.outcome, AutoUpdateOutcome::FastForwarded);
        if fast_forwarded {
            self.auto_update_ok += 1;
            self.auto_update_reprobe_targets
                .get_or_insert_with(Vec::new)
                .push(result.index);
        }

        if self.auto_update_done < self.auto_update_total {
            return;
        }
        // Guarded rather than a plain `-`: an overlapping cycle that still
        // slipped through the generation check would otherwise underflow
        // here, and a panic in this path takes the terminal down with it.
        let left_alone = self.auto_update_total.saturating_sub(self.auto_update_ok);
        self.status_message = Some(if left_alone == 0 {
            format!("auto-update: fast-forwarded {}", self.auto_update_ok)
        } else {
            format!(
                "auto-update: fast-forwarded {}, {left_alone} left alone",
                self.auto_update_ok
            )
        });
        self.auto_update_total = 0;
        self.auto_update_done = 0;
        self.auto_update_ok = 0;
    }

    /// Set once an auto-update pass finishes with at least one repo it
    /// actually touched; consumed by the run loop, which owns spawning the
    /// resulting re-probe so those rows' branch and ahead/behind reflect
    /// the merge.
    pub fn take_auto_update_reprobe_targets(&mut self) -> Option<Vec<usize>> {
        self.auto_update_reprobe_targets.take()
    }

    /// The header's `poll 5m` / `poll 5m · auto` text, `None` when the poll
    /// is off. A mode that silently modifies repos and is invisible on
    /// screen is a bug waiting to be filed (section 02).
    pub fn poll_status_text(&self) -> Option<String> {
        if !self.poll_enabled {
            return None;
        }
        let interval = poll::format_interval(self.poll_interval);
        Some(if self.auto_update {
            format!("poll {interval} · auto")
        } else {
            format!("poll {interval}")
        })
    }

    /// The header's right-hand text: the live run's summary while one is
    /// running (section 02, "the only place the run's global state
    /// appears"), otherwise the selection count, with the poll's state and
    /// a restored filter's match count layered on. A restored filter is
    /// shown here, not only in the status bar, since "4 of 42 repos" with
    /// no explanation otherwise looks like a broken config (section 09).
    pub fn header_right_text(&self) -> String {
        if let Some(run) = self.run_status_text() {
            return run;
        }
        let mut text = if self.filter.is_empty() {
            format!("{} repos", self.repos.len())
        } else {
            format!(
                "{} of {} repos · filter",
                self.visible_indices().len(),
                self.repos.len()
            )
        };
        // Only ever a count of what was actually picked. An empty selection
        // means every visible repo, and calling that "42 selected" would
        // make the two states impossible to tell apart on the one line
        // that is supposed to distinguish them.
        if !self.selected.is_empty() {
            text.push_str(&format!(" · {} selected", self.selected.len()));
        }
        if let Some(poll) = self.poll_status_text() {
            text.push_str(&format!(" · {poll}"));
        }
        text
    }

    /// The live run's summary for the header: action name, done/total, and
    /// a failure count once there's one to show.
    fn run_status_text(&self) -> Option<String> {
        let action = self.run_action.as_ref()?;
        let mut text = format!("{} {}/{}", action, self.run_completed, self.run_total);
        if self.run_failed > 0 {
            text.push_str(&format!(" · {} failed", self.run_failed));
        }
        Some(text)
    }

    /// Set by `r`; consumed by the run loop, which is the only thing with a
    /// runtime handle to spawn the resulting probe with.
    pub fn take_probe_request(&mut self) -> bool {
        std::mem::take(&mut self.probe_requested)
    }

    pub fn open_palette(&mut self) {
        self.palette_open = true;
        self.palette_filter.clear();
        self.palette_cursor = 0;
    }

    pub fn close_palette(&mut self) {
        self.palette_open = false;
    }

    /// Actions matching the palette's filter, in the same order `discover`
    /// returned them.
    pub fn palette_visible(&self) -> Vec<&Action> {
        if self.palette_filter.is_empty() {
            return self.actions.iter().collect();
        }
        let needle = self.palette_filter.to_lowercase();
        self.actions
            .iter()
            .filter(|a| a.name.to_lowercase().contains(&needle))
            .collect()
    }

    fn clamp_palette_cursor(&mut self) {
        let n = self.palette_visible().len();
        if self.palette_cursor >= n {
            self.palette_cursor = n.saturating_sub(1);
        }
    }

    pub fn palette_push(&mut self, c: char) {
        self.palette_filter.push(c);
        self.clamp_palette_cursor();
    }

    pub fn palette_backspace(&mut self) {
        self.palette_filter.pop();
        self.clamp_palette_cursor();
    }

    pub fn palette_move(&mut self, delta: isize) {
        let n = self.palette_visible().len();
        if n == 0 {
            return;
        }
        let next = (self.palette_cursor as isize + delta).clamp(0, n as isize - 1) as usize;
        self.palette_cursor = next;
    }

    /// Close the palette and request a run of whatever it's currently
    /// pointing at, if anything matches the filter.
    pub fn palette_confirm(&mut self) {
        let chosen = self
            .palette_visible()
            .get(self.palette_cursor)
            .map(|a| a.name.clone());
        self.close_palette();
        if let Some(action) = chosen {
            self.request_run(&action);
        }
    }

    /// Open the detail view for the cursor row. A no-op with a status
    /// message when the filter hides every row: the cursor can still index
    /// a repo the table isn't showing.
    pub fn open_detail(&mut self) {
        if self.visible_indices().is_empty() {
            self.status_message = Some(self.no_visible_rows_message());
            return;
        }
        self.detail_open = true;
        // Opening from a row means "show me this one", so the list keeps
        // the keys and `j`/`k` keep walking rows with the output following.
        self.focus = Pane::List;
    }

    /// Back to the full-width list.
    pub fn close_detail(&mut self) {
        self.detail_open = false;
    }

    /// `tab` in the detail view: hand the keys to the other pane.
    pub fn toggle_focus(&mut self) {
        self.focus = self.focus.other();
    }

    /// Enter on a row whose output is already on screen: the row is not the
    /// question any more, so the keys go to the pane holding the answer.
    pub fn focus_output(&mut self) {
        self.focus = Pane::Output;
    }

    /// Half a screen page for `Ctrl-D`/`Ctrl-U`, floored at one line so a
    /// very short terminal still scrolls. Approximate: it reads the last
    /// known frame height rather than the exact viewport, which is close
    /// enough for a "half page" key.
    fn half_page(&self) -> usize {
        ((self.terminal_height as usize).saturating_sub(6) / 2).max(1)
    }

    /// Move the cursor row's detail scroll by `delta` lines, floored at 0.
    /// The actual upper bound depends on the open step's length, which
    /// render.rs clamps to when it draws.
    pub fn detail_scroll_by(&mut self, delta: isize) {
        let entry = self.detail_scroll.entry(self.cursor).or_insert(0);
        *entry = (*entry as isize + delta).max(0) as usize;
    }

    pub fn detail_scroll_down(&mut self) {
        let step = self.half_page() as isize;
        self.detail_scroll_by(step);
    }

    pub fn detail_scroll_up(&mut self) {
        let step = -(self.half_page() as isize);
        self.detail_scroll_by(step);
    }

    /// Copy the step currently visible in the cursor row's detail view,
    /// falling back to a file when there's no clipboard (section 03).
    pub fn copy_visible_step(&mut self) {
        let Some(Some(RunStatus::Finished { steps, .. })) = self.run_results.get(self.cursor)
        else {
            self.status_message = Some("nothing to copy yet".into());
            return;
        };
        let lines = detail::detail_lines(steps);
        let scroll = self.detail_scroll.get(&self.cursor).copied().unwrap_or(0);
        let idx = detail::step_at_line(&lines, scroll);
        let Some(step) = steps.get(idx) else {
            return;
        };
        let text = format!("{}\n{}", step.stdout, step.stderr);
        let repo_name = self
            .repos
            .get(self.cursor)
            .map(|r| r.name.as_str())
            .unwrap_or("repo");
        self.status_message = Some(detail::copy_or_save(&text, repo_name, &step.label));
    }

    pub fn toggle_mouse_capture(&mut self) {
        self.mouse_captured = !self.mouse_captured;
        self.mouse_capture_dirty = true;
    }

    /// Set by `m`; consumed by the run loop, the only thing that can
    /// actually write the terminal escape sequence.
    pub fn take_mouse_capture_dirty(&mut self) -> bool {
        std::mem::take(&mut self.mouse_capture_dirty)
    }

    /// `o`: open `$EDITOR` on whatever the current view is about (section
    /// 03, "o is worth including early"). In the list that is the cursor
    /// row's repo; in the detail view it is the transcript on screen, since
    /// that is what you are looking at and the repo is one `esc` away.
    ///
    /// A no-op with a status message when the filter hides every row, for
    /// the same reason [`open_detail`](Self::open_detail) is. Also refused
    /// by [`mutation_blocker`](Self::mutation_blocker): a live run or
    /// auto-update pass keeps writing to repos in the background while the
    /// editor has the terminal, and the user should not open one on a repo
    /// something else is mid-write to.
    pub fn request_open_editor(&mut self) {
        if self.detail_open {
            self.request_open_transcript();
            return;
        }
        self.request_foreground(Foreground::Repo);
    }

    /// `!`: a shell in the cursor row's repo, for the things no action
    /// covers. Same suspend path as the editor, and refused on the same
    /// grounds.
    pub fn request_shell(&mut self) {
        self.request_foreground(Foreground::Shell);
    }

    /// `o` from the detail view: write the transcript somewhere real and
    /// open that. A file rather than a pipe, so the editor gets a name to
    /// show and the text survives being closed.
    fn request_open_transcript(&mut self) {
        let Some(Some(RunStatus::Finished { steps, .. })) = self.run_results.get(self.cursor)
        else {
            self.status_message = Some("no finished output to open yet".into());
            return;
        };
        let name = self
            .repos
            .get(self.cursor)
            .map(|r| r.name.as_str())
            .unwrap_or("repo");
        match detail::write_transcript(steps, name) {
            Ok(path) => self.foreground = Some(Foreground::Path(path)),
            Err(e) => self.status_message = Some(format!("could not write the transcript: {e}")),
        }
    }

    fn request_foreground(&mut self, what: Foreground) {
        if self.visible_indices().is_empty() {
            self.status_message = Some(self.no_visible_rows_message());
            return;
        }
        let verb = match what {
            Foreground::Shell => "open a shell",
            _ => "open the editor",
        };
        if self.refuse_if_mutation_blocked(verb) {
            return;
        }
        self.foreground = Some(what);
    }

    /// Set by the requests above; consumed by the run loop, the only thing
    /// that can suspend and restore the terminal. Resolves against the
    /// cursor at the moment it's taken rather than when it was requested.
    pub fn take_foreground(&mut self) -> Option<Suspend> {
        let repo = self.repos.get(self.cursor);
        match self.foreground.take()? {
            Foreground::Repo => Some(Suspend::Editor(repo?.path.clone())),
            Foreground::Path(path) => Some(Suspend::Editor(path)),
            Foreground::Shell => Some(Suspend::Shell(repo?.path.clone())),
        }
    }

    /// The global repo index at on-screen row `row` within the table body
    /// (0-based, below the header), given `scroll_offset`: the same lookup
    /// `move_cursor` uses, so a click and a keystroke can't disagree about
    /// which repo a row is.
    pub fn repo_at_row(&self, row: usize, scroll_offset: usize) -> Option<usize> {
        self.visible_indices().get(scroll_offset + row).copied()
    }

    /// Branch and working-tree text for a row, resolved once so `render.rs`
    /// only has to lay strings out. `spinner` is true while the row's probe
    /// is still in flight and there's nothing to show yet.
    pub fn probe_display(&self, idx: usize) -> ProbeDisplay {
        match self.probes.get(idx).and_then(|p| p.as_ref()) {
            Some(state) => ProbeDisplay {
                branch: probe::branch_text(state),
                state: probe::dirty_text(state, self.fetched_repos.contains(&idx)),
                spinner: false,
            },
            None => ProbeDisplay {
                branch: String::new(),
                state: String::new(),
                spinner: self.probing.contains(&idx),
            },
        }
    }

    /// Global indices of repos matching the current filter, in list order.
    /// An empty filter matches everything.
    pub fn visible_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.repos.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        self.repos
            .iter()
            .enumerate()
            .filter(|(_, r)| r.name.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    /// The repos a run would target: the explicit selection, or every
    /// visible row when nothing is selected. "No selection" reads as "all of
    /// them", which is what a multi-repo tool is for; narrowing to one repo
    /// is what `space` and `/` are for.
    ///
    /// An explicit selection is honored even if the active filter currently
    /// hides every member of it: the user chose those repos on purpose, and
    /// a filter narrows what's on screen, not what was already selected
    /// (`selection_survives_a_filter_change`). The fallback has no such
    /// choice behind it, so it follows the filter exactly, and a zero-match
    /// filter targets nothing rather than reaching a repo that is no longer
    /// on screen.
    pub fn effective_selection(&self) -> Vec<usize> {
        if !self.selected.is_empty() {
            return self.selected.iter().copied().collect();
        }
        self.visible_indices()
    }

    /// Status text for an action that would otherwise act on a repo the
    /// filter currently hides: "no repos" when the set itself
    /// is empty, "no repos match the filter" when a filter is why nothing
    /// is visible.
    fn no_visible_rows_message(&self) -> String {
        if self.filter.is_empty() {
            "no repos".into()
        } else {
            "no repos match the filter".into()
        }
    }

    fn clamp_cursor_to_visible(&mut self) {
        let visible = self.visible_indices();
        if visible.contains(&self.cursor) {
            self.follow_cursor();
            return;
        }
        if let Some(&first) = visible.first() {
            self.cursor = first;
        }
        self.follow_cursor();
    }

    /// Pull [`list_scroll`](Self::list_scroll) the shortest distance that
    /// puts the cursor back on screen, leaving it alone while the cursor is
    /// already within the window. Render clamps the same way, so a path that
    /// misses this call still draws a visible cursor; it just loses the
    /// window's memory of where the user had scrolled to.
    fn follow_cursor(&mut self) {
        let visible = self.visible_indices();
        let pos = visible.iter().position(|&i| i == self.cursor).unwrap_or(0);
        let height = super::render::list_height(self, self.terminal_height);
        self.list_scroll =
            super::render::scroll_offset(self.list_scroll, pos, visible.len(), height);
    }

    /// Move the cursor by `delta` positions among visible rows, clamped to
    /// the first and last visible row.
    pub fn move_cursor(&mut self, delta: isize) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        let pos = visible.iter().position(|&i| i == self.cursor).unwrap_or(0);
        let next = (pos as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
        self.cursor = visible[next];
        self.follow_cursor();
    }

    /// Move the cursor `dir` half-pages, the same jump `Ctrl-D`/`Ctrl-U`
    /// make in the detail view, so the chord means the same thing in both.
    pub fn move_cursor_half_page(&mut self, dir: isize) {
        self.move_cursor(dir * self.half_page() as isize);
    }

    pub fn move_to_first(&mut self) {
        if let Some(&first) = self.visible_indices().first() {
            self.cursor = first;
        }
        self.follow_cursor();
    }

    pub fn move_to_last(&mut self) {
        if let Some(&last) = self.visible_indices().last() {
            self.cursor = last;
        }
        self.follow_cursor();
    }

    /// Toggle the cursor row's selection, then advance the cursor so
    /// repeated presses of the key walk down the list. A no-op when the
    /// cursor isn't on a row the filter currently shows: a filter that
    /// narrows to zero matches can leave `cursor` pointing at a hidden repo
    /// (`clamp_cursor_to_visible` has nothing visible to clamp it to), and
    /// toggling that would manufacture an explicit selection the user never
    /// saw on screen, which `effective_selection` then honors on purpose for
    /// selections made while visible.
    pub fn toggle_selection_at_cursor(&mut self) {
        if !self.visible_indices().contains(&self.cursor) {
            return;
        }
        if !self.selected.remove(&self.cursor) {
            self.selected.insert(self.cursor);
        }
        self.move_cursor(1);
    }

    /// Select every row the current filter shows, replacing whatever was
    /// selected before. A no-op with a status message, leaving the existing
    /// selection untouched, when the filter hides every row: replacing a
    /// real selection with an empty one just because nothing currently
    /// matches would be a silent selection wipe.
    pub fn select_all_visible(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            self.status_message = Some(self.no_visible_rows_message());
            return;
        }
        self.selected = visible.into_iter().collect();
    }

    /// Select every repo in the set, filter or no filter. The filter-aware
    /// [`select_all_visible`](Self::select_all_visible) is the common one;
    /// this is for building a selection that outlives the filter you used to
    /// find part of it.
    pub fn select_all_in_set(&mut self) {
        if self.repos.is_empty() {
            self.status_message = Some("no repos".into());
            return;
        }
        self.selected = (0..self.repos.len()).collect();
    }

    /// Back to no selection, which means every visible repo again rather
    /// than none: see [`effective_selection`](Self::effective_selection).
    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// Flip the selection of every visible row; a row hidden by the filter
    /// keeps whatever selection state it already had. A no-op with a status
    /// message when the filter hides every row.
    pub fn invert_selection(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            self.status_message = Some(self.no_visible_rows_message());
            return;
        }
        for i in visible {
            if !self.selected.remove(&i) {
                self.selected.insert(i);
            }
        }
    }

    pub fn start_filter(&mut self) {
        self.filtering = true;
    }

    pub fn filter_push(&mut self, c: char) {
        self.filter.push(c);
        self.clamp_cursor_to_visible();
    }

    pub fn filter_backspace(&mut self) {
        self.filter.pop();
        self.clamp_cursor_to_visible();
    }

    /// Esc: drop the filter text entirely and go back to the full list.
    pub fn cancel_filter(&mut self) {
        self.filter.clear();
        self.filtering = false;
        self.clamp_cursor_to_visible();
    }

    /// Enter: stop editing but keep the narrowed list.
    pub fn commit_filter(&mut self) {
        self.filtering = false;
    }
}

/// Branch and working-tree text for one row, plus whether its probe is still
/// in flight.
pub struct ProbeDisplay {
    pub branch: String,
    pub state: String,
    pub spinner: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo(name: &str) -> Repo {
        Repo {
            name: name.to_string(),
            path: PathBuf::from(format!("/nonexistent/{}", name)),
            clone_url: None,
            keys: Default::default(),
        }
    }

    fn app(names: &[&str]) -> App {
        App::new(
            names.iter().map(|n| repo(n)).collect(),
            "work".into(),
            4,
            BTreeMap::new(),
            PathBuf::from("/dev/null"),
            false,
            None,
        )
    }

    /// A list long enough to scroll in a short terminal, with the window
    /// height the app would draw it at.
    fn scrolling_app(rows: usize, terminal_height: u16) -> (App, usize) {
        let names: Vec<String> = (0..rows).map(|i| format!("repo-{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut a = app(&refs);
        a.terminal_height = terminal_height;
        let height = super::super::render::list_height(&a, a.terminal_height);
        assert!(height >= 2 && rows > height, "list must actually scroll");
        (a, height)
    }

    #[test]
    fn the_cursor_walks_back_up_through_the_window_before_it_scrolls() {
        let (mut a, height) = scrolling_app(20, 12);
        a.move_to_last();
        let bottom = a.repos.len() - height;
        assert_eq!(a.list_scroll, bottom);

        for _ in 1..height {
            a.move_cursor(-1);
        }
        assert_eq!(
            a.list_scroll, bottom,
            "the window holds still while the cursor crosses it"
        );

        a.move_cursor(-1);
        assert_eq!(
            a.list_scroll,
            bottom - 1,
            "only a cursor stepping off the top edge moves the window"
        );
    }

    #[test]
    fn a_filter_that_shortens_the_list_pulls_the_window_back_into_range() {
        let (mut a, _) = scrolling_app(20, 12);
        a.move_to_last();
        assert!(a.list_scroll > 0);

        a.start_filter();
        for c in "repo-0".chars() {
            a.filter_push(c); // ten rows, all of them above the window
        }
        assert_eq!(a.list_scroll, 0);
        assert!(a.visible_indices().contains(&a.cursor));
    }

    #[test]
    fn a_filter_narrows_the_visible_rows() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filter = "ba".into();
        assert_eq!(a.visible_indices(), vec![1, 2]);
    }

    #[test]
    fn filtering_is_case_insensitive() {
        let mut a = app(&["Foo", "Bar"]);
        a.filter = "FO".into();
        assert_eq!(a.visible_indices(), vec![0]);
    }

    #[test]
    fn select_all_visible_selects_only_what_the_filter_shows() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filter = "ba".into();
        a.select_all_visible();
        assert_eq!(a.selected, BTreeSet::from([1, 2]));
    }

    #[test]
    fn selection_survives_a_filter_change() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.cursor = 0;
        a.toggle_selection_at_cursor(); // selects foo, advances the cursor
        assert!(a.selected.contains(&0));

        a.filter = "ba".into(); // foo is no longer visible
        assert!(
            a.selected.contains(&0),
            "filtering must not touch the selection"
        );

        a.filter.clear();
        assert!(a.selected.contains(&0));
    }

    #[test]
    fn an_empty_selection_means_every_visible_repo() {
        let mut a = app(&["foo", "bar", "baz"]);
        assert_eq!(a.effective_selection(), vec![0, 1, 2]);

        a.filter = "ba".into();
        assert_eq!(
            a.effective_selection(),
            vec![1, 2],
            "the fallback follows the filter, since that is what is on screen"
        );
    }

    #[test]
    fn an_explicit_selection_overrides_the_fallback() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.cursor = 2;
        a.selected.insert(0);
        assert_eq!(a.effective_selection(), vec![0]);
    }

    /// A filter that matches nothing leaves no row on screen, so the cursor
    /// fallback must not act on whatever repo the cursor still happens to
    /// index from before the filter narrowed to zero.
    #[test]
    fn a_zero_match_filter_makes_the_cursor_fallback_empty() {
        let mut a = app(&["foo", "bar"]);
        a.cursor = 0;
        a.filter = "zzz".into(); // matches nothing
        assert_eq!(a.effective_selection(), Vec::<usize>::new());
    }

    /// An explicit selection is a deliberate choice, unlike the cursor
    /// fallback, so it still runs even when a filter typed afterwards hides
    /// every member of it (the plan's own "decide deliberately" note): this
    /// is the same invariant `selection_survives_a_filter_change` already
    /// covers, just followed through to what a run actually targets.
    #[test]
    fn an_explicit_selection_still_targets_a_repo_the_filter_now_hides() {
        let mut a = app(&["foo", "bar"]);
        a.selected.insert(0);
        a.filter = "zzz".into(); // hides every row, foo included
        assert_eq!(a.effective_selection(), vec![0]);
    }

    #[test]
    fn request_run_on_a_zero_match_filter_is_a_no_op_with_a_status_message() {
        let mut a = app(&["foo"]);
        a.filter = "zzz".into();
        a.request_run("update");
        assert!(a.run_requested.is_none());
        assert!(a.pending_run.is_none());
        assert!(a.status_message.is_some());
    }

    /// The dangerous case: a repo already probed clean would otherwise run
    /// with no confirmation at all, since clean-and-known skips the
    /// dirty-selection prompt.
    #[test]
    fn a_probed_clean_repo_does_not_run_once_hidden_by_a_zero_match_filter() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main")); // clean and known: would run unconfirmed if targeted
        a.filter = "zzz".into();
        a.request_run("update");
        assert!(
            a.run_requested.is_none(),
            "a hidden cursor row must not run just because it's clean"
        );
        assert!(a.pending_run.is_none());
    }

    #[test]
    fn opening_the_detail_view_on_a_zero_match_filter_is_a_no_op() {
        let mut a = app(&["foo"]);
        a.filter = "zzz".into();
        a.open_detail();
        assert!(!a.detail_open);
        assert!(a.status_message.is_some());
    }

    #[test]
    fn requesting_the_editor_on_a_zero_match_filter_is_a_no_op() {
        let mut a = app(&["foo"]);
        a.filter = "zzz".into();
        a.request_open_editor();
        assert!(a.foreground.is_none());
        assert!(a.status_message.is_some());
    }

    /// A live run keeps writing to repos in the background while the
    /// editor has the terminal; opening one is refused rather than handing
    /// the user an editor on a repo something else is mid-write to.
    #[test]
    fn requesting_the_editor_is_refused_while_a_run_is_live() {
        let mut a = app(&["foo"]);
        a.begin_named_run("update".into(), vec![0]);
        a.request_open_editor();
        assert!(a.foreground.is_none());
        assert!(a.status_message.is_some());
    }

    /// Same hazard, same guard, for an in-flight auto-update pass.
    #[test]
    fn requesting_the_editor_is_refused_while_auto_update_is_in_flight() {
        let mut a = app(&["foo"]);
        a.auto_update_total = 1;
        a.request_open_editor();
        assert!(a.foreground.is_none());
        assert!(a.status_message.is_some());
    }

    /// Cursor on a repo, then a filter that matches nothing, then space:
    /// toggling must not manufacture an explicit selection out of a row the
    /// user can no longer see, since an explicit selection is honored across
    /// a filter change on purpose.
    #[test]
    fn toggling_selection_on_a_zero_match_filter_is_a_no_op() {
        let mut a = app(&["foo"]);
        a.cursor = 0;
        a.filter = "zzz".into();
        a.toggle_selection_at_cursor();
        assert!(a.selected.is_empty());
        assert_eq!(a.effective_selection(), Vec::<usize>::new());
    }

    #[test]
    fn select_all_on_a_zero_match_filter_leaves_the_existing_selection_untouched() {
        let mut a = app(&["foo", "bar"]);
        a.selected.insert(0);
        a.filter = "zzz".into();
        a.select_all_visible();
        assert_eq!(
            a.selected,
            BTreeSet::from([0]),
            "must not silently wipe an explicit selection"
        );
        assert!(a.status_message.is_some());
    }

    #[test]
    fn invert_on_a_zero_match_filter_is_a_no_op_with_a_message() {
        let mut a = app(&["foo"]);
        a.filter = "zzz".into();
        a.invert_selection();
        assert!(a.selected.is_empty());
        assert!(a.status_message.is_some());
    }

    #[test]
    fn toggle_selection_advances_the_cursor() {
        let mut a = app(&["foo", "bar"]);
        a.toggle_selection_at_cursor();
        assert_eq!(a.cursor, 1);
        assert!(a.selected.contains(&0));
    }

    #[test]
    fn invert_selection_flips_only_visible_rows() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.selected.insert(0);
        a.filter = "ba".into(); // bar, baz visible; foo hidden by the filter
        a.invert_selection();
        assert!(a.selected.contains(&0), "hidden selection is untouched");
        assert!(a.selected.contains(&1));
        assert!(a.selected.contains(&2));
    }

    #[test]
    fn cancel_filter_clears_text_and_restores_the_full_list() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filtering = true;
        a.filter_push('b');
        a.filter_push('a');
        a.cancel_filter();
        assert_eq!(a.filter, "");
        assert!(!a.filtering);
        assert_eq!(a.visible_indices().len(), 3);
    }

    #[test]
    fn commit_filter_keeps_the_text_and_leaves_editing() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filtering = true;
        a.filter_push('b');
        a.filter_push('a');
        a.commit_filter();
        assert_eq!(a.filter, "ba");
        assert!(!a.filtering);
    }

    #[test]
    fn typing_a_filter_clamps_the_cursor_onto_a_visible_row() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.cursor = 0; // "foo"
        a.filtering = true;
        a.filter_push('b'); // "foo" no longer matches
        assert_ne!(a.cursor, 0);
        assert!(a.visible_indices().contains(&a.cursor));
    }

    fn probed(index: usize, branch: &str) -> RepoState {
        RepoState {
            index,
            branch: Some(branch.to_string()),
            upstream: None,
            ahead: 0,
            behind: 0,
            changed: 0,
            present: true,
            timed_out: false,
            fetched: false,
            fetch_head: None,
        }
    }

    #[test]
    fn a_probe_result_for_the_current_generation_is_applied() {
        let mut a = app(&["foo"]);
        let generation = a.begin_probe(&[0]);
        a.on_probe(generation, probed(0, "main"));
        assert!(a.probes[0].is_some());
        assert!(
            !a.probing.contains(&0),
            "an applied result clears in-flight"
        );
    }

    /// A repo behind its upstream, so `dirty_text` has a number to either
    /// print or withhold.
    fn behind_by(index: usize, behind: u32, fetch_head: Option<SystemTime>) -> RepoState {
        RepoState {
            upstream: Some("origin/main".into()),
            behind,
            fetch_head,
            ..probed(index, "main")
        }
    }

    #[test]
    fn the_first_probe_of_a_repo_is_not_itself_evidence_of_a_fetch() {
        let mut a = app(&["foo"]);
        let generation = a.begin_probe(&[0]);
        a.on_probe(generation, behind_by(0, 3, Some(SystemTime::now())));
        assert!(
            !a.fetched_repos.contains(&0),
            "mrx has no idea how old a timestamp it has only just read is"
        );
        assert!(!a.probe_display(0).state.contains('↓'));
    }

    /// The case that makes an update look broken: `u` runs `git pull`, the
    /// re-probe after it finds a clean repo, and the behind column has to
    /// stop saying unknown. mrx never fetched, so the moved `FETCH_HEAD` is
    /// the only evidence there is.
    #[test]
    fn a_fetch_mrx_did_not_perform_still_settles_the_behind_count() {
        let mut a = app(&["foo"]);
        let before = SystemTime::now();
        let generation = a.begin_probe(&[0]);
        a.on_probe(generation, behind_by(0, 3, Some(before)));

        let generation = a.begin_probe(&[0]);
        a.on_probe(
            generation,
            behind_by(0, 0, Some(before + Duration::from_secs(1))),
        );

        assert!(a.fetched_repos.contains(&0));
        assert_eq!(a.probe_display(0).state, "clean");
    }

    #[test]
    fn a_repo_that_has_never_fetched_starts_counting_from_its_first_one() {
        let mut a = app(&["foo"]);
        let generation = a.begin_probe(&[0]);
        a.on_probe(generation, behind_by(0, 3, None));

        let generation = a.begin_probe(&[0]);
        a.on_probe(generation, behind_by(0, 3, Some(SystemTime::now())));

        assert!(
            a.fetched_repos.contains(&0),
            "a FETCH_HEAD appearing where there was none is a first fetch"
        );
        assert!(a.probe_display(0).state.contains("↓3"));
    }

    #[test]
    fn an_unchanged_fetch_head_leaves_the_behind_count_unknown() {
        let mut a = app(&["foo"]);
        let stamp = Some(SystemTime::now());
        for _ in 0..3 {
            let generation = a.begin_probe(&[0]);
            a.on_probe(generation, behind_by(0, 3, stamp));
        }
        assert!(!a.fetched_repos.contains(&0));
        assert!(!a.probe_display(0).state.contains('↓'));
    }

    #[test]
    fn a_stale_probe_result_is_dropped() {
        let mut a = app(&["foo", "bar"]);
        a.begin_probe(&[0, 1]); // generation 1
        a.begin_probe(&[0, 1]); // generation 2 supersedes it
        a.on_probe(1, probed(0, "stale-branch"));
        assert!(
            a.probes[0].is_none(),
            "a result from a superseded generation must be dropped"
        );
    }

    #[test]
    fn reprobe_targets_default_to_everything_when_nothing_is_selected() {
        let a = app(&["foo", "bar", "baz"]);
        assert_eq!(a.reprobe_targets(), vec![0, 1, 2]);
    }

    #[test]
    fn reprobe_targets_are_the_selection_when_something_is_selected() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.selected.insert(1);
        assert_eq!(a.reprobe_targets(), vec![1]);
    }

    #[test]
    fn a_row_with_no_probe_result_yet_shows_a_spinner_while_in_flight() {
        let mut a = app(&["foo"]);
        a.begin_probe(&[0]);
        assert!(a.probe_display(0).spinner);
    }

    #[test]
    fn a_repo_that_has_never_run_shows_the_never_run_placeholder() {
        let a = app(&["foo"]);
        assert_eq!(a.result_text(0), NEVER_RUN);
    }

    #[test]
    fn output_arriving_mid_run_builds_a_transcript_before_the_run_ends() {
        let mut a = app(&["foo"]);
        let run_id = a.begin_run();
        a.on_task(run_id, TaskEvent::Started { index: 0 });
        a.on_task(
            run_id,
            TaskEvent::Step {
                index: 0,
                label: "git pull".into(),
            },
        );
        for line in ["remote: counting", "Fast-forward"] {
            a.on_task(
                run_id,
                TaskEvent::Output {
                    index: 0,
                    step: 0,
                    stderr: false,
                    line: line.into(),
                },
            );
        }

        let live = a.live.get(&0).expect("a step in flight has a section");
        assert_eq!(live.steps.len(), 1);
        assert_eq!(live.steps[0].stdout, "remote: counting\nFast-forward\n");
    }

    #[test]
    fn a_finished_run_replaces_the_partial_output_rather_than_sitting_beside_it() {
        let mut a = app(&["foo"]);
        let run_id = a.begin_run();
        a.on_task(
            run_id,
            TaskEvent::Step {
                index: 0,
                label: "git pull".into(),
            },
        );
        a.on_task(
            run_id,
            TaskEvent::Output {
                index: 0,
                step: 0,
                stderr: false,
                line: "partial".into(),
            },
        );
        a.on_task(
            run_id,
            TaskEvent::Finished {
                index: 0,
                steps: vec![StepResult {
                    label: "git pull".into(),
                    shape: summarize::Shape::Generic,
                    stdout: "partial\n".into(),
                    stderr: String::new(),
                    code: 0,
                }],
                exit_code: 0,
            },
        );
        assert!(!a.live.contains_key(&0));
    }

    /// A line for a step that never announced itself would otherwise have to
    /// invent a section to hold it, and the heading it invented would be a
    /// guess about what produced the text.
    #[test]
    fn output_for_an_unknown_step_is_dropped_rather_than_given_a_section() {
        let mut a = app(&["foo"]);
        let run_id = a.begin_run();
        a.on_task(
            run_id,
            TaskEvent::Output {
                index: 0,
                step: 7,
                stderr: false,
                line: "orphan".into(),
            },
        );
        assert!(!a.live.contains_key(&0));
    }

    #[test]
    fn a_result_older_than_the_ttl_goes_back_to_never_run() {
        let mut a = app(&["foo", "bar"]);
        a.result_ttl = Some(Duration::from_secs(60));
        let run_id = a.begin_run();
        a.on_task(
            run_id,
            TaskEvent::Skipped {
                index: 0,
                reason: "not checked out".into(),
            },
        );
        a.expire_results();
        assert_eq!(a.result_text(0), "not checked out");

        a.result_at
            .insert(0, Instant::now() - Duration::from_secs(61));
        a.expire_results();
        assert_eq!(a.result_text(0), NEVER_RUN);
    }

    #[test]
    fn results_never_expire_while_the_run_that_made_them_is_still_going() {
        let mut a = app(&["foo", "bar"]);
        a.result_ttl = Some(Duration::from_secs(60));
        a.run_action = Some("update".into());
        a.run_results[0] = Some(RunStatus::Running);
        a.result_at
            .insert(0, Instant::now() - Duration::from_secs(600));
        a.expire_results();
        assert!(a.run_results[0].is_some(), "a live run is not stale");
    }

    #[test]
    fn results_are_kept_indefinitely_when_the_ttl_is_off() {
        let mut a = app(&["foo"]);
        a.result_ttl = None;
        a.run_results[0] = Some(RunStatus::Skipped { reason: "x".into() });
        a.result_at
            .insert(0, Instant::now() - Duration::from_secs(9999));
        a.expire_results();
        assert!(a.run_results[0].is_some());
    }

    #[test]
    fn a_selects_what_is_on_screen_and_shift_a_selects_the_whole_set() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filter = "ba".into();

        a.select_all_visible();
        assert_eq!(a.selected, BTreeSet::from([1, 2]));

        a.select_all_in_set();
        assert_eq!(
            a.selected,
            BTreeSet::from([0, 1, 2]),
            "the whole set, filter or no filter"
        );

        a.clear_selection();
        assert!(a.selected.is_empty());
    }

    #[test]
    fn tab_hands_the_keys_to_the_other_pane_and_back() {
        let mut a = app(&["foo"]);
        a.open_detail();
        assert_eq!(a.focus, Pane::List, "opening a row is about that row");
        a.toggle_focus();
        assert_eq!(a.focus, Pane::Output);
        a.toggle_focus();
        assert_eq!(a.focus, Pane::List);
    }

    #[test]
    fn reopening_the_detail_view_starts_on_the_list_again() {
        let mut a = app(&["foo"]);
        a.open_detail();
        a.toggle_focus();
        a.close_detail();
        a.open_detail();
        assert_eq!(a.focus, Pane::List);
    }

    #[test]
    fn on_task_tracks_a_run_from_started_through_step_to_finished() {
        let mut a = app(&["foo"]);
        let run_id = a.begin_run();

        a.on_task(run_id, TaskEvent::Started { index: 0 });
        assert_eq!(a.result_text(0), "running");

        a.on_task(
            run_id,
            TaskEvent::Step {
                index: 0,
                label: "post_update".into(),
            },
        );
        assert_eq!(a.result_text(0), "post_update");

        a.on_task(
            run_id,
            TaskEvent::Finished {
                index: 0,
                steps: vec![StepResult {
                    label: "post_update".into(),
                    shape: crate::summarize::Shape::Generic,
                    stdout: "wrote 3 files".into(),
                    stderr: String::new(),
                    code: 0,
                }],
                exit_code: 0,
            },
        );
        assert_eq!(a.result_text(0), "wrote 3 files");
    }

    #[test]
    fn a_skipped_task_reports_its_reason() {
        let mut a = app(&["foo"]);
        let run_id = a.begin_run();
        a.on_task(
            run_id,
            TaskEvent::Skipped {
                index: 0,
                reason: "no update action defined".into(),
            },
        );
        assert_eq!(a.result_text(0), "no update action defined");
    }

    #[test]
    fn an_event_from_a_superseded_run_is_dropped() {
        let mut a = app(&["foo"]);
        let stale = a.begin_run();
        a.begin_run(); // a newer run supersedes it
        a.on_task(stale, TaskEvent::Started { index: 0 });
        assert_eq!(
            a.result_text(0),
            NEVER_RUN,
            "an event tagged with an old run id must not be applied"
        );
    }

    #[test]
    fn dirty_count_counts_only_targets_the_probe_found_changed() {
        let mut a = app(&["foo", "bar", "baz"]);
        let mut dirty = probed(0, "main");
        dirty.changed = 2;
        a.on_probe(0, dirty);
        a.on_probe(0, probed(1, "main")); // clean
        assert_eq!(a.dirty_count(&[0, 1]), 1);
        assert_eq!(a.dirty_count(&[1]), 0);
        assert_eq!(
            a.dirty_count(&[2]),
            0,
            "dirty_count itself doesn't count an unprobed repo; unprobed_count does"
        );
    }

    #[test]
    fn unprobed_count_counts_targets_with_no_probe_result_yet() {
        let mut a = app(&["foo", "bar"]);
        a.on_probe(0, probed(0, "main"));
        assert_eq!(a.unprobed_count(&[0]), 0);
        assert_eq!(a.unprobed_count(&[1]), 1);
        assert_eq!(a.unprobed_count(&[0, 1]), 1);
    }

    #[test]
    fn a_clean_and_probed_selection_runs_immediately_without_confirmation() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main")); // clean and known
        a.request_run("update");
        assert!(a.pending_run.is_none());
        assert!(a.run_requested.is_some());
    }

    #[test]
    fn an_unprobed_selection_waits_on_confirmation_unless_forced() {
        // No probe result has come back yet, e.g. right after startup, a set
        // switch, or a config reload: dirtiness is unknown, not clean, and
        // must not run unconfirmed (section 11).
        let mut a = app(&["foo"]);
        a.request_run("update");
        assert!(
            a.run_requested.is_none(),
            "must not run before confirming an unprobed selection"
        );
        let pending = a
            .pending_run
            .as_ref()
            .expect("an unprobed run needs confirming");
        assert_eq!(pending.dirty_count, 0);
        assert_eq!(pending.unknown_count, 1);

        a.confirm_pending_run();
        assert_eq!(a.run_requested.as_ref().unwrap().action, "update");
    }

    #[test]
    fn force_skips_confirmation_even_on_an_unprobed_selection() {
        let mut a = app(&["foo"]);
        a.force = true;
        a.request_run("update");
        assert!(a.pending_run.is_none());
        assert!(a.run_requested.is_some());
    }

    #[test]
    fn request_run_refuses_while_a_run_is_already_live() {
        let mut a = app(&["foo", "bar"]);
        a.on_probe(0, probed(0, "main"));
        a.begin_named_run("update".into(), vec![0]);

        a.request_run("status");
        assert!(
            a.run_requested.is_none(),
            "must not start a second run over a live one"
        );
        assert!(a.pending_run.is_none());
        assert!(a.status_message.is_some());
    }

    /// A manual run sharing a repo with a live auto-update pass would run
    /// `git` against it through two different semaphores at once.
    #[test]
    fn request_run_refuses_while_auto_update_is_in_flight() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main"));
        a.auto_update_total = 1;

        a.request_run("status");
        assert!(a.run_requested.is_none());
        assert!(a.pending_run.is_none());
        assert!(a.status_message.is_some());
    }

    /// The confirmation prompt can sit open with nothing blocking it, then
    /// an auto-update pass can start while the user is still deciding: a
    /// poll completing doesn't wait on a modal. Confirming must re-check at
    /// that point rather than trusting the now-stale check `request_run`
    /// made when the prompt opened.
    #[test]
    fn confirm_pending_run_refuses_when_auto_update_starts_while_the_prompt_is_open() {
        let mut a = app(&["foo"]);
        let mut dirty = probed(0, "main");
        dirty.changed = 1;
        a.on_probe(0, dirty);

        a.request_run("update");
        assert!(a.pending_run.is_some(), "a dirty run needs confirming");

        a.auto_update_total = 1; // starts while the prompt is still open
        a.confirm_pending_run();

        assert!(
            a.run_requested.is_none(),
            "must not launch into a repo an in-flight auto-update pass owns"
        );
        assert!(a.status_message.is_some());
    }

    #[test]
    fn a_dirty_selection_waits_on_confirmation_unless_forced() {
        let mut a = app(&["foo"]);
        let mut dirty = probed(0, "main");
        dirty.changed = 3;
        a.on_probe(0, dirty);

        a.request_run("update");
        assert!(a.run_requested.is_none(), "must not run before confirming");
        let pending = a
            .pending_run
            .as_ref()
            .expect("a dirty run needs confirming");
        assert_eq!(pending.dirty_count, 1);

        a.confirm_pending_run();
        assert!(a.pending_run.is_none());
        assert_eq!(a.run_requested.as_ref().unwrap().action, "update");
    }

    #[test]
    fn cancelling_a_pending_run_drops_it_without_running() {
        let mut a = app(&["foo"]);
        let mut dirty = probed(0, "main");
        dirty.changed = 1;
        a.on_probe(0, dirty);

        a.request_run("update");
        a.cancel_pending_run();
        assert!(a.pending_run.is_none());
        assert!(a.run_requested.is_none());
    }

    #[test]
    fn force_skips_confirmation_even_on_a_dirty_selection() {
        let mut a = app(&["foo"]);
        a.force = true;
        let mut dirty = probed(0, "main");
        dirty.changed = 1;
        a.on_probe(0, dirty);

        a.request_run("update");
        assert!(a.pending_run.is_none());
        assert!(a.run_requested.is_some());
    }

    #[test]
    #[should_panic(expected = "nothing defines")]
    fn requesting_an_action_nothing_defines_is_a_bug() {
        let mut a = app(&["foo"]);
        a.request_run("does-not-exist-anywhere");
    }

    #[test]
    fn a_completed_run_clears_the_live_action_and_requests_a_reprobe() {
        let mut a = app(&["foo", "bar"]);
        let run_id = a.begin_named_run("update".into(), vec![0, 1]);

        a.on_task(run_id, TaskEvent::Started { index: 0 });
        assert_eq!(a.run_action.as_deref(), Some("update"));
        assert!(a.take_post_run_targets().is_none(), "still running");

        a.on_task(
            run_id,
            TaskEvent::Finished {
                index: 0,
                steps: vec![],
                exit_code: 1,
            },
        );
        assert_eq!(a.run_failed, 1);
        a.on_task(
            run_id,
            TaskEvent::Skipped {
                index: 1,
                reason: "not checked out".into(),
            },
        );

        assert_eq!(a.run_completed, 2);
        assert_eq!(
            a.run_action, None,
            "the header stops showing a finished run"
        );
        let targets = a
            .take_post_run_targets()
            .expect("a finished run reprobes its targets");
        assert_eq!(targets, vec![0, 1]);
        assert!(a.take_post_run_targets().is_none(), "only taken once");
    }

    #[test]
    fn palette_filter_narrows_the_action_list() {
        let a = app(&["foo"]);
        let all = a.palette_visible().len();
        let mut a = a;
        a.palette_filter = "upda".into();
        let filtered: Vec<&str> = a
            .palette_visible()
            .iter()
            .map(|x| x.name.as_str())
            .collect();
        assert_eq!(filtered, vec!["update"]);
        assert!(filtered.len() < all);
    }

    #[test]
    fn palette_confirm_requests_a_run_of_the_highlighted_action() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main")); // clean and known, so it runs immediately
        a.open_palette();
        a.palette_filter = "status".into();
        a.palette_confirm();
        assert!(!a.palette_open);
        assert_eq!(a.run_requested.unwrap().action, "status");
    }

    #[test]
    fn toggle_mouse_capture_flips_the_flag_and_marks_it_dirty() {
        let mut a = app(&["foo"]);
        assert!(a.mouse_captured, "capture starts on");
        a.toggle_mouse_capture();
        assert!(!a.mouse_captured);
        assert!(a.take_mouse_capture_dirty());
        assert!(!a.take_mouse_capture_dirty(), "only taken once");
    }

    #[test]
    fn open_editor_resolves_to_the_cursor_repo_at_the_moment_its_taken() {
        let mut a = app(&["foo", "bar"]);
        a.cursor = 1;
        a.request_open_editor();
        assert!(a.foreground.is_some());

        // Moving the cursor before the run loop gets around to taking the
        // request is what "at the moment it's taken" means.
        a.cursor = 0;
        match a.take_foreground() {
            Some(Suspend::Editor(path)) => assert_eq!(path, PathBuf::from("/nonexistent/foo")),
            _ => panic!("expected the editor on the cursor's repo"),
        }
        assert!(a.foreground.is_none(), "only taken once");
        assert!(a.take_foreground().is_none());
    }

    #[test]
    fn bang_asks_for_a_shell_in_the_cursor_repo() {
        let mut a = app(&["foo", "bar"]);
        a.cursor = 1;
        a.request_shell();
        match a.take_foreground() {
            Some(Suspend::Shell(dir)) => assert_eq!(dir, PathBuf::from("/nonexistent/bar")),
            _ => panic!("expected a shell in the cursor's repo"),
        }
    }

    /// In the list `o` means the repo; in the detail view the repo is not
    /// what is on screen, so it means the transcript instead.
    #[test]
    fn o_in_the_detail_view_asks_for_the_log_rather_than_the_repo() {
        let mut a = app(&["foo"]);
        a.detail_open = true;
        a.run_results[0] = Some(RunStatus::Finished {
            steps: vec![StepResult {
                label: "update".into(),
                shape: summarize::Shape::Generic,
                stdout: "Already up to date.\n".into(),
                stderr: String::new(),
                code: 0,
            }],
            exit_code: 0,
        });

        a.request_open_editor();
        match a.take_foreground() {
            Some(Suspend::Editor(path)) => {
                assert!(path.to_string_lossy().ends_with(".log"), "got {path:?}");
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(text.contains("Already up to date."), "got {text:?}");
            }
            _ => panic!("expected the transcript"),
        }
    }

    #[test]
    fn o_in_the_detail_view_says_so_when_there_is_no_output_to_open() {
        let mut a = app(&["foo"]);
        a.detail_open = true;
        a.request_open_editor();
        assert!(a.foreground.is_none());
        assert!(a.status_message.is_some());
    }

    #[test]
    fn detail_scroll_is_kept_per_repo() {
        let mut a = app(&["foo", "bar"]);
        a.cursor = 0;
        a.detail_scroll_down();
        a.cursor = 1;
        assert_eq!(
            a.detail_scroll.get(&1).copied().unwrap_or(0),
            0,
            "a different repo starts unscrolled"
        );
        a.cursor = 0;
        assert!(a.detail_scroll[&0] > 0);
    }

    #[test]
    fn detail_scroll_up_does_not_go_negative() {
        let mut a = app(&["foo"]);
        a.detail_scroll_up();
        assert_eq!(a.detail_scroll[&0], 0);
    }

    #[test]
    fn copying_before_anything_has_run_reports_nothing_to_copy() {
        let mut a = app(&["foo"]);
        a.copy_visible_step();
        assert_eq!(a.status_message.as_deref(), Some("nothing to copy yet"));
    }

    #[test]
    fn a_click_row_resolves_to_the_same_repo_the_cursor_would_under_a_filter_and_scroll() {
        let mut a = app(&[
            "aardvark", "bar-1", "bar-2", "bar-3", "bar-4", "bar-5", "bar-6", "bar-7",
        ]);
        a.filter = "bar".into(); // "aardvark" is filtered out
        a.cursor = 7; // "bar-7", scrolled past the top of a short list

        let visible = a.visible_indices();
        let list_height = 3;
        let cursor_pos = visible.iter().position(|&i| i == a.cursor).unwrap();
        let scroll = super::super::render::scroll_offset(0, cursor_pos, visible.len(), list_height);
        assert!(
            scroll > 0,
            "the cursor must actually be scrolled for this test to mean anything"
        );

        let on_screen_row = cursor_pos - scroll;
        assert_eq!(a.repo_at_row(on_screen_row, scroll), Some(a.cursor));
    }

    fn write_config(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn reload_config_keeps_the_selection_by_name_and_drops_removed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        write_config(&cfg, "[bar]\n[foo]\n[zzz]\n");

        let config::Config {
            repos, defaults, ..
        } = config::load(&cfg, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, cfg.clone(), false, None);

        let foo_before = a.repos.iter().position(|r| r.name == "foo").unwrap();
        let zzz_before = a.repos.iter().position(|r| r.name == "zzz").unwrap();
        a.selected.insert(foo_before);
        a.selected.insert(zzz_before);
        a.cursor = foo_before;

        // "aab" sorts above every existing name, landing above "foo" in the
        // reloaded list; "zzz" is gone entirely.
        write_config(&cfg, "[aab]\n[bar]\n[foo]\n");
        a.reload_config();

        let names: Vec<&str> = a.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["aab", "bar", "foo"]);

        let foo_after = a.repos.iter().position(|r| r.name == "foo").unwrap();
        assert_ne!(
            foo_before, foo_after,
            "the repo added above foo must actually have moved its index for this test to mean anything"
        );
        assert_eq!(
            a.selected,
            BTreeSet::from([foo_after]),
            "zzz drops out silently, foo is kept by name rather than by its old index"
        );
        assert_eq!(
            a.cursor, foo_after,
            "the cursor follows foo by name even though a repo was added above it"
        );
        assert!(a.full_reprobe_requested);
    }

    #[test]
    fn reload_config_is_a_no_op_while_a_run_is_live() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        write_config(&cfg, "[foo]\n");
        let config::Config {
            repos, defaults, ..
        } = config::load(&cfg, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, cfg.clone(), false, None);
        a.begin_named_run("update".into(), vec![0]);

        write_config(&cfg, "[foo]\n[bar]\n");
        a.reload_config();

        assert_eq!(a.repos.len(), 1, "the reload must not have happened");
        assert!(a.status_message.is_some());
    }

    /// `reload_config` used to call `config::load`, which calls
    /// `std::process::exit(1)` on a bad file. `Ctrl-R` runs with raw mode,
    /// the alternate screen, and mouse capture all active, so that exit
    /// bypassed teardown and the panic hook and left the terminal wrecked.
    /// It must instead keep the config already loaded and report the error.
    #[test]
    fn reload_config_keeps_the_current_config_when_the_edit_does_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        write_config(&cfg, "[foo]\n[bar]\n");
        let config::Config {
            repos, defaults, ..
        } = config::load(&cfg, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, cfg.clone(), false, None);

        // An unclosed section bracket: invalid INI, fails to parse.
        write_config(&cfg, "[baz\n");
        a.reload_config();

        assert_eq!(
            a.repos.len(),
            2,
            "the previous config must still be loaded, not process::exit(1)"
        );
        let names: Vec<&str> = a.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["bar", "foo"]);
        assert!(
            a.status_message
                .as_deref()
                .is_some_and(|m| m.contains("reload")),
            "got {:?}",
            a.status_message
        );
    }

    /// Same as above but through the set-picker path.
    #[test]
    fn confirm_set_picker_keeps_the_current_config_when_the_target_does_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.mrconfig");
        write_config(&good, "[foo]\n");
        let bad = dir.path().join("bad.mrconfig");
        write_config(&bad, "[baz\n");

        let config::Config {
            repos, defaults, ..
        } = config::load(&good, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, good.clone(), false, None);

        a.set_entries = vec![SetEntry {
            name: "bad".into(),
            path: bad,
        }];
        a.set_picker_cursor = 0;
        a.set_picker_open = true;
        a.confirm_set_picker();

        assert!(!a.set_picker_open);
        assert_eq!(
            a.config_path, good,
            "must not have switched away from the config that parses"
        );
        assert_eq!(a.repos.len(), 1);
        assert!(
            a.status_message
                .as_deref()
                .is_some_and(|m| m.contains("switch")),
            "got {:?}",
            a.status_message
        );
    }

    #[test]
    fn the_set_picker_is_blocked_while_a_run_is_live() {
        let mut a = app(&["foo"]);
        a.begin_named_run("update".into(), vec![0]);
        a.open_set_picker();
        assert!(!a.set_picker_open);
        assert!(a.status_message.is_some());
    }

    /// The picker can sit open with nothing blocking it, then an auto-update
    /// pass can start while the user is still choosing: a poll completing
    /// and starting a fast-forward cycle doesn't wait on a modal. Confirming
    /// must re-check at that point rather than trusting the now-stale check
    /// `open_set_picker` made, or the switch would hand the in-flight
    /// auto-update's results indices into a repo list it no longer applies to.
    #[test]
    fn confirm_set_picker_is_blocked_when_auto_update_starts_after_the_picker_opened() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("current.mrconfig");
        write_config(&current, "[foo]\n");
        let other = dir.path().join("other.mrconfig");
        write_config(&other, "[bar]\n");

        let config::Config {
            repos, defaults, ..
        } = config::load(&current, None);
        let mut a = App::new(
            repos,
            "work".into(),
            4,
            defaults,
            current.clone(),
            false,
            None,
        );

        a.set_entries = vec![SetEntry {
            name: "other".into(),
            path: other,
        }];
        a.set_picker_cursor = 0;
        a.set_picker_open = true; // picker opened while nothing was blocking it

        a.auto_update_total = 1; // then a poll cycle started fast-forwarding
        a.confirm_set_picker();

        assert!(!a.set_picker_open);
        assert_eq!(
            a.config_path, current,
            "must not have switched sets out from under the in-flight auto-update"
        );
        assert_eq!(a.repos.len(), 1);
        assert!(a.status_message.is_some());
    }

    #[test]
    fn opening_the_set_picker_appends_the_active_config_as_unnamed_when_undiscovered() {
        let mut a = app(&["foo"]); // config_path is /dev/null, never a discovered set
        a.open_set_picker();
        assert!(a.set_picker_open);
        assert!(
            a.set_entries
                .iter()
                .any(|e| e.name == "(unnamed)" && e.path == a.config_path),
            "got {:?}",
            a.set_entries
        );
    }

    #[test]
    fn cancelling_a_live_run_reports_queued_versus_still_finishing() {
        let mut a = app(&["a", "b", "c"]);
        let run_id = a.begin_named_run("update".into(), vec![0, 1, 2]);
        a.on_task(run_id, TaskEvent::Started { index: 0 }); // 1 and 2 are still queued

        a.request_cancel();
        assert_eq!(
            a.status_message.as_deref(),
            Some("cancelled, 2 queued skipped, 1 still finishing")
        );
        assert!(a.take_cancel_requested());
        assert!(!a.take_cancel_requested(), "only taken once");
    }

    #[test]
    fn cancel_is_a_no_op_when_nothing_is_running() {
        let mut a = app(&["foo"]);
        a.request_cancel();
        assert!(a.status_message.is_none());
        assert!(!a.take_cancel_requested());
    }

    #[test]
    fn quitting_while_a_run_is_live_waits_for_confirmation() {
        let mut a = app(&["foo"]);
        a.begin_named_run("update".into(), vec![0]);
        assert!(!a.request_quit(), "must not quit immediately");
        assert!(a.quit_pending);
        assert!(a.confirm_quit());
        assert!(!a.quit_pending);
    }

    #[test]
    fn quitting_with_nothing_running_needs_no_confirmation() {
        let mut a = app(&["foo"]);
        assert!(a.request_quit());
        assert!(!a.quit_pending);
    }

    #[test]
    fn declining_the_quit_prompt_closes_it_without_quitting() {
        let mut a = app(&["foo"]);
        a.begin_named_run("update".into(), vec![0]);
        a.request_quit();
        a.cancel_quit();
        assert!(!a.quit_pending);
    }

    #[test]
    fn a_due_poll_while_a_run_is_live_is_a_no_op_and_the_next_one_after_it_finishes_is_not() {
        let mut a = app(&["foo"]);
        a.poll_enabled = true;
        let run_id = a.begin_named_run("update".into(), vec![0]);
        a.on_task(run_id, TaskEvent::Started { index: 0 });

        a.on_poll_due();
        assert!(
            a.take_poll_requested().is_none(),
            "a live run suspends the poll"
        );

        a.on_task(
            run_id,
            TaskEvent::Finished {
                index: 0,
                steps: Vec::new(),
                exit_code: 0,
            },
        );
        assert!(a.run_action.is_none(), "the run really did finish");

        a.on_poll_due();
        assert_eq!(a.take_poll_requested(), Some(vec![0]));
    }

    #[test]
    fn a_due_poll_while_disabled_is_a_no_op() {
        let mut a = app(&["foo"]);
        a.on_poll_due();
        assert!(a.take_poll_requested().is_none());
    }

    #[test]
    fn toggling_the_poll_off_also_turns_off_auto_update() {
        let mut a = app(&["foo"]);
        a.poll_enabled = true;
        a.auto_update = true;
        a.toggle_poll();
        assert!(!a.poll_enabled);
        assert!(
            !a.auto_update,
            "auto-update has nothing to act on without the poll"
        );
    }

    #[test]
    fn auto_update_refuses_to_turn_on_while_the_poll_is_off() {
        let mut a = app(&["foo"]);
        a.toggle_auto_update();
        assert!(!a.auto_update);
        assert!(a.status_message.is_some());
    }

    #[test]
    fn a_finished_poll_cycle_requests_auto_update_only_for_fast_forwardable_repos() {
        let mut a = app(&["clean-behind", "dirty-behind"]);
        a.poll_enabled = true;
        a.auto_update = true;

        a.on_poll_due();
        let targets = a.take_poll_requested().expect("poll started");
        let generation = a.probe_generation;

        let clean = RepoState {
            index: 0,
            branch: Some("main".into()),
            upstream: Some("origin/main".into()),
            ahead: 0,
            behind: 2,
            changed: 0,
            present: true,
            timed_out: false,
            fetched: true,
            fetch_head: None,
        };
        let mut dirty = clean.clone();
        dirty.index = 1;
        dirty.changed = 1;

        for state in [clean, dirty] {
            assert!(targets.contains(&state.index));
            a.on_probe(generation, state);
        }

        assert_eq!(a.take_auto_update_requested(), Some(vec![0]));
    }

    #[test]
    fn a_run_starting_mid_poll_suppresses_that_polls_auto_update() {
        // The poll begins while nothing is running, so `on_poll_due` lets it
        // start; a run then begins before every repo's fetch has landed.
        // The fast-forward pass that poll cycle would otherwise queue must
        // not fire underneath the live run (section 02: "both suspend while
        // a run is live").
        let mut a = app(&["clean-behind"]);
        a.poll_enabled = true;
        a.auto_update = true;

        a.on_poll_due();
        let targets = a.take_poll_requested().expect("poll started");
        let generation = a.probe_generation;

        a.begin_named_run("update".into(), vec![0]);

        for &index in &targets {
            a.on_probe(
                generation,
                RepoState {
                    index,
                    branch: Some("main".into()),
                    upstream: Some("origin/main".into()),
                    ahead: 0,
                    behind: 2,
                    changed: 0,
                    present: true,
                    timed_out: false,
                    fetched: true,
                    fetch_head: None,
                },
            );
        }

        assert!(
            a.take_auto_update_requested().is_none(),
            "a run that started mid-poll must suppress that poll's auto-update pass"
        );
    }

    /// A repo's own fetch can fail (offline, VPN, auth) even while other
    /// repos in the same poll cycle succeed; its behind column must read
    /// unknown rather than borrowing the cycle's overall completion.
    #[test]
    fn a_repos_behind_column_is_known_only_once_its_own_fetch_has_succeeded() {
        let mut a = app(&["ok", "fails"]);
        a.poll_enabled = true;

        a.on_poll_due();
        let targets = a.take_poll_requested().expect("poll started");
        let generation = a.probe_generation;

        let mut fetch_ok = probed(targets[0], "main");
        fetch_ok.upstream = Some("origin/main".into());
        fetch_ok.behind = 2;
        fetch_ok.fetched = true;

        let mut fetch_failed = probed(targets[1], "main");
        fetch_failed.upstream = Some("origin/main".into());
        fetch_failed.behind = 2;
        fetch_failed.fetched = false;

        a.on_probe(generation, fetch_ok);
        a.on_probe(generation, fetch_failed);

        assert!(
            a.probe_display(targets[0]).state.contains("↓2"),
            "the repo whose own fetch succeeded shows a real behind count, got {:?}",
            a.probe_display(targets[0]).state
        );
        assert!(
            !a.probe_display(targets[1]).state.contains('↓'),
            "the repo whose own fetch failed must not borrow the other one's freshness, got {:?}",
            a.probe_display(targets[1]).state
        );

        // A later fetch-less reprobe of the repo that did succeed must not
        // downgrade it back to unknown: the sticky per-repo record is what
        // makes "known" mean "has fetched at least once", not "just fetched".
        let mut later = probed(targets[0], "main");
        later.upstream = Some("origin/main".into());
        later.behind = 2;
        later.fetched = false;
        let g2 = a.begin_probe(&[targets[0]]);
        a.on_probe(g2, later);
        assert!(
            a.probe_display(targets[0]).state.contains("↓2"),
            "a repo that has already fetched successfully stays known"
        );
    }

    #[test]
    fn a_plain_reprobe_never_triggers_auto_update() {
        let mut a = app(&["clean-behind"]);
        a.auto_update = true; // set directly: never went through the poll it needs

        let generation = a.begin_probe(&[0]);
        a.on_probe(
            generation,
            RepoState {
                index: 0,
                branch: Some("main".into()),
                upstream: Some("origin/main".into()),
                ahead: 0,
                behind: 2,
                changed: 0,
                present: true,
                timed_out: false,
                fetched: false,
                fetch_head: None,
            },
        );
        assert!(
            a.take_auto_update_requested().is_none(),
            "only a poll cycle's own results should ever trigger auto-update"
        );
    }

    /// A repo whose own fetch failed must not become an auto-update
    /// candidate on the strength of stale ahead/behind data, even if it
    /// otherwise passes every other `can_fast_forward` condition.
    #[test]
    fn a_repo_whose_fetch_failed_this_cycle_is_not_an_auto_update_candidate() {
        let mut a = app(&["fails"]);
        a.poll_enabled = true;
        a.auto_update = true;

        a.on_poll_due();
        let targets = a.take_poll_requested().expect("poll started");
        let generation = a.probe_generation;

        let mut s = probed(targets[0], "main");
        s.upstream = Some("origin/main".into());
        s.behind = 2;
        s.fetched = false; // this repo's own git fetch failed

        a.on_probe(generation, s);

        assert!(
            a.take_auto_update_requested().is_none(),
            "a repo whose fetch failed this cycle must not be picked for auto-update"
        );
    }

    #[test]
    fn an_auto_update_result_summarises_once_every_target_has_reported() {
        let mut a = app(&["ok", "fails"]);
        a.poll_enabled = true;
        a.auto_update = true;
        a.on_poll_due();
        let targets = a.take_poll_requested().expect("poll started");
        let generation = a.probe_generation;

        for &i in &targets {
            let mut s = probed(i, "main");
            s.upstream = Some("origin/main".into());
            s.behind = 2;
            s.fetched = true;
            a.on_probe(generation, s);
        }

        let auto_targets = a
            .take_auto_update_requested()
            .expect("both repos are eligible");
        let cycle = a.auto_update_generation();

        a.on_auto_update_result(AutoUpdateResult {
            index: auto_targets[0],
            generation: cycle,
            outcome: AutoUpdateOutcome::FastForwarded,
        });
        assert!(a.status_message.is_none(), "not done yet");

        a.on_auto_update_result(AutoUpdateResult {
            index: auto_targets[1],
            generation: cycle,
            outcome: AutoUpdateOutcome::Failed("not fast-forward possible".into()),
        });
        assert_eq!(
            a.status_message.as_deref(),
            Some("auto-update: fast-forwarded 1, 1 left alone")
        );
        assert_eq!(
            a.take_auto_update_reprobe_targets(),
            Some(vec![auto_targets[0]])
        );
    }

    /// A result tagged with an older auto-update generation belongs to a
    /// cycle the counters have already moved past and must be dropped, the
    /// same way a stale probe result is.
    #[test]
    fn an_auto_update_result_from_a_superseded_generation_is_dropped() {
        let mut a = app(&["foo"]);
        a.auto_update_generation = 2;
        a.auto_update_total = 1;

        a.on_auto_update_result(AutoUpdateResult {
            index: 0,
            generation: 1, // an older cycle
            outcome: AutoUpdateOutcome::FastForwarded,
        });

        assert_eq!(
            a.auto_update_done, 0,
            "a result from a superseded generation must not be counted"
        );
        assert!(a.status_message.is_none());
    }

    /// A poll cycle must not start a second auto-update pass while one is
    /// still in flight, since a late result from the first would otherwise
    /// land against the second cycle's counters.
    #[test]
    fn a_poll_cycle_refuses_to_start_a_second_auto_update_pass_while_one_is_in_flight() {
        let mut a = app(&["foo"]);
        a.poll_enabled = true;
        a.auto_update = true;
        a.auto_update_total = 1; // a cycle is already in flight
        a.auto_update_done = 0;

        a.on_poll_due();
        let targets = a.take_poll_requested().expect("poll started");
        let generation = a.probe_generation;
        let mut s = probed(targets[0], "main");
        s.upstream = Some("origin/main".into());
        s.behind = 2;
        s.fetched = true;
        a.on_probe(generation, s);

        assert!(
            a.take_auto_update_requested().is_none(),
            "must not start a second auto-update cycle while one is still in flight"
        );
    }

    /// The completion arithmetic must not panic even if a stale result
    /// slips past the generation check with `ok` already ahead of `total`,
    /// since a panic in this path takes the terminal down with it.
    #[test]
    fn on_auto_update_result_does_not_panic_when_ok_would_exceed_total() {
        let mut a = app(&["foo"]);
        a.auto_update_generation = 1;
        a.auto_update_total = 0;
        a.auto_update_done = 0;
        a.auto_update_ok = 1;

        a.on_auto_update_result(AutoUpdateResult {
            index: 0,
            generation: 1,
            outcome: AutoUpdateOutcome::FastForwarded,
        });
        // The point of the test is that this doesn't panic; a debug build
        // panics on integer underflow, which is what an unguarded
        // `total - ok` would do here.
    }

    /// Switching sets while auto-update has merges in flight would hand a
    /// later `AutoUpdateResult` an index into a different repo list, the
    /// same hazard a live run guards against.
    #[test]
    fn the_set_picker_is_blocked_while_auto_update_is_in_flight() {
        let mut a = app(&["foo"]);
        a.auto_update_total = 1;
        a.open_set_picker();
        assert!(!a.set_picker_open);
        assert!(a.status_message.is_some());
    }

    /// Same hazard, same guard, for a config reload.
    #[test]
    fn reload_config_is_a_no_op_while_auto_update_is_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        write_config(&cfg, "[foo]\n");
        let config::Config {
            repos, defaults, ..
        } = config::load(&cfg, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, cfg.clone(), false, None);
        a.auto_update_total = 1;

        write_config(&cfg, "[foo]\n[bar]\n");
        a.reload_config();

        assert_eq!(a.repos.len(), 1, "the reload must not have happened");
        assert!(a.status_message.is_some());
    }

    #[test]
    fn poll_status_text_is_none_until_the_poll_is_on() {
        let a = app(&["foo"]);
        assert_eq!(a.poll_status_text(), None);
    }

    #[test]
    fn poll_status_text_shows_auto_only_once_auto_update_is_on_too() {
        let mut a = app(&["foo"]);
        a.poll_enabled = true;
        a.poll_interval = Duration::from_secs(300);
        assert_eq!(a.poll_status_text().as_deref(), Some("poll 5m"));

        a.auto_update = true;
        assert_eq!(a.poll_status_text().as_deref(), Some("poll 5m · auto"));
    }

    #[test]
    fn a_restored_session_applies_the_filter_and_selection_dropping_unknown_names() {
        let mut a = app(&["bill-api", "menu-api", "mr-yum"]);
        let session = Session {
            filter: "api".into(),
            selected: vec!["bill-api".into(), "menu-api".into(), "gone".into()],
            ..Default::default()
        };
        a.restore_session(&session);

        assert_eq!(a.filter, "api");
        assert_eq!(
            a.selected,
            BTreeSet::from([0, 1]),
            "a name the repo list doesn't have is dropped silently"
        );
    }

    #[test]
    fn a_restored_session_applies_the_cursor_and_poll_settings() {
        let mut a = app(&["bill-api", "menu-api", "mr-yum"]);
        let session = Session {
            cursor: Some("mr-yum".into()),
            poll_interval: Some(Duration::from_secs(120)),
            auto_update: true,
            ..Default::default()
        };
        a.restore_session(&session);

        assert_eq!(a.cursor, 2);
        assert!(a.poll_enabled);
        assert_eq!(a.poll_interval, Duration::from_secs(120));
        assert!(a.auto_update);
    }

    #[test]
    fn a_restored_cursor_not_visible_under_the_restored_filter_falls_back_to_the_first_visible_row()
    {
        let mut a = app(&["bill-api", "menu-api", "mr-yum"]);
        let session = Session {
            filter: "api".into(),
            cursor: Some("mr-yum".into()), // doesn't match "api"
            ..Default::default()
        };
        a.restore_session(&session);
        assert_eq!(
            a.cursor, 0,
            "the same rule move_cursor already applies while filtering live"
        );
    }

    #[test]
    fn a_restored_session_never_turns_on_auto_update_without_the_poll() {
        let mut a = app(&["foo"]);
        let session = Session {
            auto_update: true, // poll_interval left None: the poll was off
            ..Default::default()
        };
        a.restore_session(&session);
        assert!(!a.poll_enabled);
        assert!(!a.auto_update);
    }
}
