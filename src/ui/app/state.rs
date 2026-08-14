//! State for the resident app: the repo list, cursor, selection, and filter.
//! Every decision worth testing lives here as a method that returns data;
//! `render.rs` only turns that data into widgets.

use super::probe::{self, RepoState};
use crate::config::Repo;
use std::collections::BTreeSet;

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
}

impl App {
    pub fn new(repos: Vec<Repo>, set_label: String, jobs: usize) -> Self {
        let n = repos.len();
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
        App::new(names.iter().map(|n| repo(n)).collect(), "work".into(), 4)
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
}
