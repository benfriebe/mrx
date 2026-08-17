//! Applying probe results under the generation counter, and the `FETCH_HEAD`
//! baseline that decides when a behind count is trustworthy.

use super::App;
use crate::ui::app::probe::{self, RepoState};

impl App {
    /// Repos to re-probe for `r`: the selection, or everything when nothing
    /// is selected, the same reading of an empty selection
    /// [`effective_selection`](Self::effective_selection) uses.
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
        // Recorded as this one result lands rather than at the end of the
        // cycle; see [`fetched_repos`](Self::fetched_repos).
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

    pub fn take_probe_request(&mut self) -> bool {
        std::mem::take(&mut self.probe_requested)
    }

    /// Branch and working-tree text for a row, resolved once so `render.rs`
    /// only has to lay strings out. `spinner` is true while the row's probe
    /// is in flight, whether or not it already has a result to show, and the
    /// last known text is reported alongside it rather than blanked: how
    /// much of the row the spinner takes over is render.rs's decision.
    pub fn probe_display(&self, idx: usize) -> ProbeDisplay {
        let spinner = self.probing.contains(&idx);
        match self.probes.get(idx).and_then(|p| p.as_ref()) {
            Some(state) => ProbeDisplay {
                branch: probe::branch_text(state),
                state: probe::dirty_text(state, self.fetched_repos.contains(&idx)),
                spinner,
            },
            None => ProbeDisplay {
                branch: String::new(),
                state: String::new(),
                spinner,
            },
        }
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
    use super::super::testkit::{app, probed};
    use super::*;
    use crate::ui::app::session::Session;
    use std::time::{Duration, SystemTime};

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

    /// A fetch mrx watched happen does not stop having happened when the
    /// app is restarted, so the `↓` it settled has to come back with it:
    /// otherwise a relaunch says nobody has asked when somebody has.
    #[test]
    fn a_fetch_seen_before_a_restart_still_settles_the_behind_count_after_it() {
        let stamp = SystemTime::now();
        let mut before = app(&["foo"]);
        let generation = before.begin_probe(&[0]);
        before.on_probe(
            generation,
            behind_by(0, 1, Some(stamp - Duration::from_secs(60))),
        );
        let generation = before.begin_probe(&[0]);
        before.on_probe(generation, behind_by(0, 1, Some(stamp)));
        assert!(before.probe_display(0).state.contains("↓1"));

        let mut after = app(&["foo"]);
        after.restore_session(&Session::snapshot(&before));
        let generation = after.begin_probe(&[0]);
        after.on_probe(generation, behind_by(0, 1, Some(stamp)));

        assert!(
            after.probe_display(0).state.contains("↓1"),
            "an unchanged FETCH_HEAD after a restart is the same fetch, got {:?}",
            after.probe_display(0).state
        );
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
    fn a_row_being_re_probed_shows_a_spinner_and_still_reports_what_it_knows() {
        let mut a = app(&["foo"]);
        let generation = a.begin_probe(&[0]);
        a.on_probe(generation, probed(0, "main"));

        a.begin_probe(&[0]);
        let display = a.probe_display(0);
        assert!(display.spinner, "a re-probe is still a probe in flight");
        assert_eq!(display.branch, "main");
        assert_eq!(
            display.state, "clean",
            "the last known state stays readable while the new one is fetched"
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
}
