//! Applying probe results under the generation counter, and the `FETCH_HEAD`
//! baseline that decides when a behind count is trustworthy.

use super::App;
use crate::ui::app::probe::{self, RepoState};

impl App {
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

    /// Branch and working-tree text for a row, resolved once so `render.rs`
    /// only has to lay strings out. A row being re-probed keeps reporting
    /// what it last knew rather than blanking; the spinner announcing the
    /// in-flight probe has a cell of its own.
    pub fn probe_display(&self, idx: usize) -> ProbeDisplay {
        match self.probes.get(idx).and_then(|p| p.as_ref()) {
            Some(state) => ProbeDisplay {
                branch: probe::branch_text(state),
                state: probe::dirty_text(state),
            },
            None => ProbeDisplay {
                branch: String::new(),
                state: String::new(),
            },
        }
    }

    /// A row's distance from its upstream, `(ahead, behind)`.
    pub fn sync_counts(&self, idx: usize) -> Option<(u32, u32)> {
        let state = self.probes.get(idx).and_then(|p| p.as_ref())?;
        probe::sync_counts(state, self.fetched_repos.contains(&idx))
    }

    /// The width of the SYNC column's two fields, each sized to the widest
    /// count any row carries. Fixed fields are the whole point of the column:
    /// they are what puts every ↓ at the same offset, however many digits the
    /// ↑ beside it needs. A field no row uses is zero wide, so a set with
    /// nothing ahead of its upstream spends no cells saying so.
    pub fn sync_widths(&self) -> (usize, usize) {
        let field = |count: u32| {
            if count == 0 {
                0
            } else {
                count.to_string().len() + 1
            }
        };
        (0..self.repos.len())
            .filter_map(|i| self.sync_counts(i))
            .fold((0, 0), |(aw, bw), (ahead, behind)| {
                (aw.max(field(ahead)), bw.max(field(behind)))
            })
    }

    /// One row's SYNC cell, laid into the fields [`sync_widths`](Self::sync_widths)
    /// sized. Trailing padding is left on so the cell's own width is the
    /// column's, whichever of the two counts the row happens to have.
    pub fn sync_text(&self, idx: usize, (ahead_w, behind_w): (usize, usize)) -> String {
        let arrow = |arrow: char, count: u32, width: usize| match count {
            0 => " ".repeat(width),
            n => format!("{:<width$}", format!("{arrow}{n}")),
        };
        let Some((ahead, behind)) = self.sync_counts(idx) else {
            return " ".repeat(sync_width((ahead_w, behind_w)));
        };
        let gap = if ahead_w > 0 && behind_w > 0 { " " } else { "" };
        format!(
            "{}{gap}{}",
            arrow('↑', ahead, ahead_w),
            arrow('↓', behind, behind_w)
        )
    }
}

/// How wide the SYNC column is overall: both fields plus the space between
/// them, and nothing at all when neither field is in use.
pub fn sync_width((ahead_w, behind_w): (usize, usize)) -> usize {
    match (ahead_w, behind_w) {
        (0, 0) => 0,
        (a, 0) | (0, a) => a,
        (a, b) => a + 1 + b,
    }
}

/// Branch and working-tree text for one row.
pub struct ProbeDisplay {
    pub branch: String,
    pub state: String,
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

    /// A repo behind its upstream, so the SYNC column has a number to either
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
        assert_eq!(a.sync_counts(0), Some((0, 0)));
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
        assert_eq!(a.sync_counts(0), Some((0, 0)));
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
        assert_eq!(a.sync_counts(0), Some((0, 3)));
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
            behind_by(0, 1, Some(stamp - Duration::from_mins(1))),
        );
        let generation = before.begin_probe(&[0]);
        before.on_probe(generation, behind_by(0, 1, Some(stamp)));
        assert_eq!(before.sync_counts(0), Some((0, 1)));

        let mut after = app(&["foo"]);
        after.restore_session(&Session::snapshot(&before));
        let generation = after.begin_probe(&[0]);
        after.on_probe(generation, behind_by(0, 1, Some(stamp)));

        assert_eq!(
            after.sync_counts(0),
            Some((0, 1)),
            "an unchanged FETCH_HEAD after a restart is the same fetch"
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
        assert_eq!(a.sync_counts(0), Some((0, 0)));
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
    fn a_row_being_re_probed_still_reports_what_it_knows() {
        let mut a = app(&["foo"]);
        let generation = a.begin_probe(&[0]);
        a.on_probe(generation, probed(0, "main"));

        a.begin_probe(&[0]);
        let display = a.probe_display(0);
        assert!(a.probing.contains(&0), "a re-probe is still in flight");
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

        assert_eq!(
            a.sync_counts(targets[0]),
            Some((0, 2)),
            "the repo whose own fetch succeeded shows a real behind count"
        );
        assert_eq!(
            a.sync_counts(targets[1]),
            Some((0, 0)),
            "the repo whose own fetch failed must not borrow the other one's freshness"
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
        assert_eq!(
            a.sync_counts(targets[0]),
            Some((0, 2)),
            "a repo that has already fetched successfully stays known"
        );
    }
}
