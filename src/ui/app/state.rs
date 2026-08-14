//! State for the resident app: the repo list, cursor, selection, and filter.
//! Every decision worth testing lives here as a method that returns data;
//! `render.rs` only turns that data into widgets.

use super::actions::{self, Action};
use super::detail;
use super::probe::{self, RepoState};
use crate::config::{self, Repo};
use crate::executor::{StepResult, TaskEvent};
use crate::sets;
use crate::summarize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Shown for a repo that has never taken part in a run this session, per
/// section 02: "a repo that has never been run shows `·` rather than a fake
/// 'pending'".
const NEVER_RUN: &str = "·";

pub struct App {
    pub repos: Vec<Repo>,
    /// Active set's display name, or `(unnamed)` for a bare config file.
    pub set_label: String,
    /// Default parallelism for runs launched from inside the app. Unused
    /// until the executor work lands in a later phase.
    pub jobs: usize,
    /// Global index into `repos`, always pointing at a visible row.
    pub cursor: usize,
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
    /// Whether anything has fetched remote refs this session. Until it has,
    /// the behind column reads unknown rather than claiming to be current.
    pub fetched_this_session: bool,
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
}

/// One row in the set picker: a discovered set's name and the config path
/// it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetEntry {
    pub name: String,
    pub path: PathBuf,
}

/// A run waiting on user confirmation because part of its target selection
/// is dirty (section 11, "destructive actions one keystroke away").
pub struct PendingRun {
    pub action: String,
    pub targets: Vec<usize>,
    pub dirty_count: usize,
}

/// A run that's been decided on (no confirmation needed, or confirmed) and
/// is ready for the run loop to plan and spawn.
pub struct RunRequest {
    pub action: String,
    pub targets: Vec<usize>,
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
            selected: BTreeSet::new(),
            filter: String::new(),
            filtering: false,
            tick: 0,
            probes: vec![None; n],
            probing: BTreeSet::new(),
            probe_generation: 0,
            fetched_this_session: false,
            probe_requested: false,
            run_id: 0,
            run_results: vec![None; n],
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
        }
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
            TaskEvent::Started { index } => (index, RunStatus::Running),
            TaskEvent::Step { index, label } => (index, RunStatus::Step { label }),
            TaskEvent::Finished {
                index,
                steps,
                exit_code,
            } => (index, RunStatus::Finished { steps, exit_code }),
            TaskEvent::Skipped { index, reason } => (index, RunStatus::Skipped { reason }),
        };

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

    /// `tab`: open the set picker. Blocked while a run is live, the same
    /// guard [`reload_config`](Self::reload_config) uses, since switching
    /// the repo list out from under a live run's indices would attribute
    /// its results to the wrong rows.
    pub fn open_set_picker(&mut self) {
        if self.run_action.is_some() {
            self.status_message = Some("can't switch sets while a run is live".into());
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
    pub fn confirm_set_picker(&mut self) {
        let Some(entry) = self.set_entries.get(self.set_picker_cursor).cloned() else {
            self.close_set_picker();
            return;
        };
        self.close_set_picker();
        let config::Config {
            repos, defaults, ..
        } = config::load(&entry.path, self.dir_override.as_deref());
        self.set_label = entry.name;
        self.reconcile_repos(repos, defaults, entry.path);
    }

    /// `Ctrl-R`: re-read the active config from disk without changing which
    /// config is active. Blocked while a run is live, for the same reason
    /// [`open_set_picker`](Self::open_set_picker) is.
    pub fn reload_config(&mut self) {
        if self.run_action.is_some() {
            self.status_message = Some("can't reload while a run is live".into());
            return;
        }
        let config::Config {
            repos, defaults, ..
        } = config::load(&self.config_path, self.dir_override.as_deref());
        let config_path = self.config_path.clone();
        self.reconcile_repos(repos, defaults, config_path);
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

    /// How many of `targets` the last probe found dirty. Used to decide
    /// whether starting a run needs the confirmation from section 11; a
    /// repo with no probe result yet counts as clean rather than blocking
    /// the run on a probe that hasn't come back.
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

    /// Ask to run `action_name` over the effective selection. Debug-asserts
    /// the action is actually defined somewhere: the palette must never
    /// have offered it otherwise (section 08). Goes straight to
    /// `run_requested` when nothing in the target selection is dirty, or
    /// when `force` is set; otherwise waits on confirmation.
    pub fn request_run(&mut self, action_name: &str) {
        debug_assert!(
            self.actions.iter().any(|a| a.name == action_name),
            "the palette must never offer an action nothing defines: {action_name}"
        );
        let targets = self.effective_selection();
        if targets.is_empty() {
            return;
        }
        let dirty = self.dirty_count(&targets);
        if dirty > 0 && !self.force {
            self.pending_run = Some(PendingRun {
                action: action_name.to_string(),
                targets,
                dirty_count: dirty,
            });
        } else {
            self.run_requested = Some(RunRequest {
                action: action_name.to_string(),
                targets,
            });
        }
    }

    pub fn confirm_pending_run(&mut self) {
        if let Some(p) = self.pending_run.take() {
            self.run_requested = Some(RunRequest {
                action: p.action,
                targets: p.targets,
            });
        }
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
        if let Some(slot) = self.probes.get_mut(state.index) {
            *slot = Some(state);
        }
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

    /// Open the detail view for the cursor row.
    pub fn open_detail(&mut self) {
        if !self.repos.is_empty() {
            self.detail_open = true;
        }
    }

    /// Back to the full-width list.
    pub fn close_detail(&mut self) {
        self.detail_open = false;
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
                state: probe::dirty_text(state, self.fetched_this_session),
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

    /// The repos a run would target: the explicit selection, or the row under
    /// the cursor when nothing is explicitly selected. Without this rule the
    /// common case (open the app, act on one repo) needs a redundant select
    /// first.
    pub fn effective_selection(&self) -> Vec<usize> {
        if !self.selected.is_empty() {
            return self.selected.iter().copied().collect();
        }
        if self.repos.is_empty() {
            Vec::new()
        } else {
            vec![self.cursor]
        }
    }

    fn clamp_cursor_to_visible(&mut self) {
        let visible = self.visible_indices();
        if visible.contains(&self.cursor) {
            return;
        }
        if let Some(&first) = visible.first() {
            self.cursor = first;
        }
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
    }

    pub fn move_to_first(&mut self) {
        if let Some(&first) = self.visible_indices().first() {
            self.cursor = first;
        }
    }

    pub fn move_to_last(&mut self) {
        if let Some(&last) = self.visible_indices().last() {
            self.cursor = last;
        }
    }

    /// Toggle the cursor row's selection, then advance the cursor so
    /// repeated presses of the key walk down the list.
    pub fn toggle_selection_at_cursor(&mut self) {
        if self.repos.is_empty() {
            return;
        }
        if !self.selected.remove(&self.cursor) {
            self.selected.insert(self.cursor);
        }
        self.move_cursor(1);
    }

    /// Select every row the current filter shows, replacing whatever was
    /// selected before.
    pub fn select_all_visible(&mut self) {
        self.selected = self.visible_indices().into_iter().collect();
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// Flip the selection of every visible row; a row hidden by the filter
    /// keeps whatever selection state it already had.
    pub fn invert_selection(&mut self) {
        for i in self.visible_indices() {
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
    fn an_empty_selection_means_the_cursor_row() {
        let a = app(&["foo", "bar"]);
        assert_eq!(a.effective_selection(), vec![0]);
    }

    #[test]
    fn an_explicit_selection_overrides_the_cursor_row() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.cursor = 2;
        a.selected.insert(0);
        assert_eq!(a.effective_selection(), vec![0]);
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
            "no probe result yet counts as clean"
        );
    }

    #[test]
    fn a_clean_selection_runs_immediately_without_confirmation() {
        let mut a = app(&["foo"]);
        a.request_run("update");
        assert!(a.pending_run.is_none());
        assert!(a.run_requested.is_some());
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
        let scroll = super::super::render::scroll_offset(cursor_pos, visible.len(), list_height);
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

    #[test]
    fn the_set_picker_is_blocked_while_a_run_is_live() {
        let mut a = app(&["foo"]);
        a.begin_named_run("update".into(), vec![0]);
        a.open_set_picker();
        assert!(!a.set_picker_open);
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
}
