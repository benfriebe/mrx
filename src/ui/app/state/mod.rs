//! State for ui mode: the repo list, cursor, selection, and filter.
//! Every decision worth testing lives here as a method that returns data;
//! `render.rs` only turns that data into widgets.

use super::actions::{self, Action};
use super::poll;
use super::probe::RepoState;
use super::session::Session;
use crate::config::Repo;
use crate::ui::textarea::TextArea;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

mod auto_update;
mod detail_view;
mod foreground;
mod list;
mod palette;
mod probing;
mod run;
mod run_command;
mod run_request;
mod set_picker;
mod sort;
#[cfg(test)]
mod testkit;

pub use detail_view::{OutputSelection, Pane};
pub use foreground::{Foreground, Suspend};
pub use probing::{sync_width, ProbeDisplay};
pub use run::{LiveRun, RunStatus, DEFAULT_RESULT_TTL};
pub use run_request::{PendingRun, RunRequest};
pub use set_picker::SetEntry;
pub use sort::{Direction, Sort};

/// Shown for a repo that has never taken part in a run this session, rather
/// than a fake "pending".
const NEVER_RUN: &str = "·";

/// Everything ui mode draws, plus the requests it hands back to the run loop.
///
/// State holds no runtime handle and does no I/O of its own, so anything
/// needing a spawn, a terminal write, or the executor's `RunHandle` is
/// recorded here as a flag or a target list and consumed by the run loop
/// through the `take_*` methods.
pub struct App {
    pub repos: Vec<Repo>,
    /// Active set's display name, or `(unnamed)` for a bare config file.
    pub set_label: String,
    /// Default parallelism for the probes, polls and runs ui mode spawns.
    pub jobs: usize,
    /// `-j`, kept so that a reload or a set switch, which re-resolves `jobs`
    /// against the config it just read, still lets the flag win.
    pub jobs_flag: Option<usize>,
    /// Global index into `repos`, always pointing at a visible row.
    pub cursor: usize,
    /// First visible-list position drawn at the top of the table. Kept as
    /// state rather than derived from the cursor, so the window only moves
    /// when the cursor would otherwise leave it.
    pub list_scroll: usize,
    pub selected: BTreeSet<usize>,
    pub filter: String,
    /// Whether `/` is currently capturing keystrokes into `filter`.
    pub filtering: bool,
    /// The column the table is ordered by, and which way it reads. Chosen
    /// together by [`choose_sort`](Self::choose_sort), which is the only
    /// thing that flips a direction.
    pub sort: Sort,
    pub sort_direction: Direction,
    /// Whether `S` is waiting on the column key that picks an order.
    pub sort_menu_open: bool,
    pub tick: usize,
    /// Latest known probe result per repo, `None` until the first one for
    /// that repo arrives.
    pub probes: Vec<Option<RepoState>>,
    /// Repos with an in-flight probe in the current generation, so a row
    /// shows a spinner instead of stale or blank data.
    pub probing: BTreeSet<usize>,
    /// Bumped every time a probe run starts; a result tagged with an older
    /// generation is dropped rather than painted over newer data.
    pub probe_generation: u64,
    /// Global indices of repos known to have fetched this session, either
    /// because a poll cycle's own `git fetch` succeeded for them or because
    /// their `FETCH_HEAD` moved since [`fetch_baseline`](Self::fetch_baseline)
    /// was taken. Until a given repo is in here, its behind column reads
    /// unknown rather than claiming to be current; a repo whose own fetch
    /// keeps failing (offline, VPN, auth) must not borrow another repo's
    /// success just because they polled in the same cycle. Sticky per repo,
    /// so a later fetch-less reprobe doesn't downgrade a repo that has
    /// genuinely fetched before.
    pub fetched_repos: BTreeSet<usize>,
    /// Each repo's `FETCH_HEAD` timestamp as of the first probe that saw it,
    /// so a later probe can tell a fetch that happened since from one that
    /// happened days ago. Absent means never probed; `None` means probed and
    /// it had never fetched. This is what credits a fetch mrx did not
    /// perform: an `update` action pulls, and the re-probe that follows sees
    /// the newer timestamp.
    pub fetch_baseline: BTreeMap<usize, Option<SystemTime>>,
    /// Set by the `R` key.
    /// Bumped every time a run starts; an executor event tagged with an
    /// older id belongs to a run that's since been cancelled and superseded,
    /// and is dropped by [`on_task`](Self::on_task).
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
    /// back to `·`. `None` keeps every result until the next run replaces it.
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
    /// Skip the dirty-selection confirmation; mirrors the CLI's `--force`.
    pub force: bool,
    /// Every runnable action for the active set, discovered once at
    /// startup so the palette has something to filter.
    pub actions: Vec<Action>,

    /// Whether the action palette (`:`) is capturing keystrokes.
    pub palette_open: bool,
    pub palette_filter: String,
    pub palette_cursor: usize,

    /// Whether the run-command prompt (`r`) is capturing keystrokes.
    pub run_command_open: bool,
    /// The body typed into that prompt, newlines and all: it is handed to
    /// `sh` as one script rather than a line at a time.
    pub run_command: TextArea,

    /// A run waiting on the dirty-selection confirmation; `None` once it's
    /// been answered either way.
    pub pending_run: Option<PendingRun>,
    /// Set once a run should actually start.
    pub run_requested: Option<RunRequest>,

    /// The live run's action name, `None` once it finishes so the header
    /// falls back to the plain selection count.
    pub run_action: Option<String>,
    /// Global indices the live (or just-finished) run covers.
    pub run_targets: Vec<usize>,
    pub run_total: usize,
    pub run_completed: usize,
    pub run_failed: usize,
    /// Set once a run finishes, to the repos it covered, for a re-probe.
    pub post_run_targets: Option<Vec<usize>>,

    /// Whether Enter has opened the detail view for the cursor row. The
    /// view always follows `cursor`, so no separate "which repo" field.
    pub detail_open: bool,
    /// Each repo's scroll position in its own detail view, kept per repo
    /// (keyed by global index) so paging through rows with j/k doesn't
    /// lose your place when you come back.
    pub detail_scroll: BTreeMap<usize, usize>,

    /// Whether the mouse is currently captured; starts `true`.
    pub mouse_captured: bool,
    /// Set by `m`.
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
    /// one of them. Rebuilt each time the picker opens, so a set created on
    /// disk since startup shows up.
    pub set_entries: Vec<SetEntry>,
    pub set_picker_cursor: usize,

    /// Set by a config reload or a set switch, both of which replace the
    /// repo list out from under the probe.
    pub full_reprobe_requested: bool,

    /// Set by `Esc` while a run is live; only the run loop holds the
    /// `RunHandle` carrying the executor's cancel flag.
    pub cancel_requested: bool,

    /// Whether `q`/`Ctrl-C` is waiting on confirmation because a run is live.
    pub quit_pending: bool,

    /// `?`: whether the keymap overlay is showing. Purely a view concern, so
    /// it gates nothing and blocks no operation.
    pub help_open: bool,
    /// Which half of the split `j`/`k` drive. Meaningless with the detail
    /// view closed, and reset every time it opens, so `tab` never leaves
    /// focus somewhere the next open would inherit.
    pub focus: Pane,
    /// The output pane's own text selection, dragged with the mouse.
    pub output_selection: Option<OutputSelection>,

    /// `o` and `!`: what to run in the foreground once the app has stepped
    /// out of the way.
    pub foreground: Option<Foreground>,

    /// `F`: whether the freshness poll is currently on. Off by default,
    /// since freshness is an opt-in loop.
    pub poll_enabled: bool,
    /// How often the poll fires when it's on; a value on `App` rather than a
    /// hardcoded constant, so a config or a persisted session can change it.
    pub poll_interval: Duration,
    /// When the last freshness cycle was dispatched, for the header's
    /// "checked N ago". A cycle still fetching already counts: the header
    /// answers when mrx last asked, not when every answer came back.
    pub last_poll_at: Option<Instant>,
    /// Whether the opening fetch is still owed. See
    /// [`arm_boot_fetch`](Self::arm_boot_fetch).
    pub boot_fetch_pending: bool,
    /// `Ctrl-A`: whether a completed poll cycle is allowed to run `update`
    /// on what it finds behind. Off by default; never true while
    /// `poll_enabled` is false, since it has nothing to act on without one.
    pub auto_update: bool,
    /// Set by [`on_poll_due`](Self::on_poll_due) when a tick is actually
    /// allowed to start a cycle.
    poll_targets_requested: Option<Vec<usize>>,
    /// The probe generation the current in-flight probe belongs to, when it
    /// was started as a poll cycle rather than a plain probe. Lets
    /// [`on_probe`](Self::on_probe) tell "a poll cycle just finished" from
    /// any other probe, without a second copy of the generation machinery.
    poll_generation: Option<u64>,
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
            jobs_flag: None,
            cursor: 0,
            list_scroll: 0,
            selected: BTreeSet::new(),
            filter: String::new(),
            filtering: false,
            sort: Sort::default(),
            sort_direction: Sort::default().natural(),
            sort_menu_open: false,
            tick: 0,
            probes: vec![None; n],
            probing: BTreeSet::new(),
            probe_generation: 0,
            fetched_repos: BTreeSet::new(),
            fetch_baseline: BTreeMap::new(),
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
            run_command_open: false,
            run_command: TextArea::default(),
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
            output_selection: None,
            poll_enabled: false,
            poll_interval: poll::DEFAULT_POLL_INTERVAL,
            last_poll_at: None,
            boot_fetch_pending: false,
            auto_update: false,
            poll_targets_requested: None,
            poll_generation: None,
        }
    }

    /// Indices of repos whose name satisfies `has_name`, used to carry a
    /// name-keyed selection across a repo list that has just been replaced.
    fn indices_matching(&self, has_name: impl Fn(&str) -> bool) -> BTreeSet<usize> {
        self.repos
            .iter()
            .enumerate()
            .filter(|(_, r)| has_name(&r.name))
            .map(|(i, _)| i)
            .collect()
    }

    /// The current index of the repo named `name`, if it's still present.
    fn index_of_name(&self, name: &str) -> Option<usize> {
        self.repos.iter().position(|r| r.name == name)
    }

    /// Apply a persisted session on top of a freshly built app. Filter and
    /// selection are matched by repo name so a config edit doesn't misdirect
    /// them onto the wrong row, and a name the current repo list doesn't
    /// have is dropped silently: a config edit is not an error. `set_label`
    /// is left untouched, since `main.rs` already decided which config to
    /// load before this ever runs.
    /// Whether an overlay is covering the table, so a click behind it lands
    /// on nothing rather than on the row it happens to sit over.
    pub fn any_overlay_open(&self) -> bool {
        self.palette_open || self.set_picker_open || self.run_command_open || self.sort_menu_open
    }

    pub fn restore_session(&mut self, session: &Session) {
        self.filter = session.filter.clone();
        self.sort = session.sort;
        self.sort_direction = session.sort_direction;

        self.selected = self.indices_matching(|n| session.selected.iter().any(|s| s == n));

        // `fetch_head_moved` cannot work this out again after a restart: the
        // first probe of a process has nothing to compare its `FETCH_HEAD`
        // against, so an untouched repo would read as never fetched and its
        // behind count would go back to withheld.
        self.fetched_repos = self.indices_matching(|n| session.fetched.iter().any(|f| f == n));

        if let Some(pos) = session
            .cursor
            .as_deref()
            .and_then(|n| self.index_of_name(n))
        {
            self.cursor = pos;
        }
        self.clamp_cursor_to_visible();

        // An explicit off outranks the set's `auto_fetch`, which has already
        // been applied by this point: pressing `F` has to mean something a
        // restart cannot undo. The interval is kept only when the poll is on,
        // so an off session doesn't also overwrite the config's cadence.
        if let Some(interval) = session.poll_interval {
            self.poll_enabled = !interval.is_zero();
            if self.poll_enabled {
                self.poll_interval = interval;
            }
        }
        // Never restore auto-update without the poll it depends on, even if
        // the file somehow has that combination.
        self.auto_update = session.auto_update && self.poll_enabled;
    }

    /// Whichever mutating operation currently owns the repos on disk, if
    /// any. A run (including the one auto-update starts), a set switch, a
    /// config reload, and an editor suspension all either write to a working
    /// tree or replace the repo list out from under indices another of them
    /// is still using, so at most one may be underway at a time. Callers
    /// must check at the moment they commit, not only when they open a
    /// modal: another one can start in the gap between the two.
    fn mutation_blocker(&self) -> Option<&'static str> {
        self.run_action
            .is_some()
            .then_some("another run is already live")
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
}

#[cfg(test)]
mod tests {
    use super::testkit::app;
    use super::*;

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

    /// The startup order this depends on: `apply_auto_fetch` runs first, then
    /// this. Without an explicit zero on disk the config would turn the poll
    /// straight back on every launch, and `F` would only ever mute it until
    /// the next one.
    #[test]
    fn a_session_that_turned_the_poll_off_outranks_a_set_that_asks_for_it() {
        let mut a = app(&["foo"]);
        a.apply_auto_fetch(Some(Duration::from_secs(360)));
        assert!(a.poll_enabled, "the set asked for the poll");

        let session = Session {
            poll_interval: Some(Duration::ZERO),
            ..Default::default()
        };
        a.restore_session(&session);
        assert!(!a.poll_enabled);
        assert_eq!(
            a.poll_interval,
            Duration::from_secs(360),
            "an off session says nothing about the cadence to use next time"
        );
    }

    /// The other half of the pair: a file that never mentioned the poll leaves
    /// the config's answer standing.
    #[test]
    fn a_session_with_no_poll_field_leaves_the_set_to_decide() {
        let mut a = app(&["foo"]);
        a.apply_auto_fetch(Some(Duration::from_secs(360)));
        a.restore_session(&Session::default());
        assert!(a.poll_enabled);
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
