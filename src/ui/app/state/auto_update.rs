//! The freshness poll and the fast-forward pass it feeds, including the
//! counters that track a cycle from start to summary.

use super::App;
use crate::ui::app::poll::{self, AutoUpdateOutcome, AutoUpdateResult};

impl App {
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
    pub(super) fn maybe_complete_poll(&mut self, generation: u64) {
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
        //
        // Everything this cycle found behind is either a target or a skip,
        // split on `can_fast_forward` itself so the two can't drift apart.
        // A skip never reaches the merge, so this is the only place it can
        // be counted for the summary.
        let mut targets: Vec<usize> = Vec::new();
        let mut skipped = 0usize;
        for (i, probe) in self.probes.iter().enumerate() {
            let Some(s) = probe.as_ref() else { continue };
            if !s.fetched || s.behind == 0 {
                continue;
            }
            if poll::can_fast_forward(s) {
                targets.push(i);
            } else {
                skipped += 1;
            }
        }
        if targets.is_empty() {
            // Nothing merged still leaves something to say when repos were
            // skipped; a cycle that found nothing behind stays quiet. No
            // counters are set either way, so nothing looks in flight.
            if skipped > 0 {
                self.status_message = Some(Self::auto_update_summary(0, skipped));
            }
            return;
        }
        self.auto_update_generation += 1;
        self.auto_update_total = targets.len();
        self.auto_update_done = 0;
        self.auto_update_ok = 0;
        self.auto_update_skipped = skipped;
        self.auto_update_requested = Some(targets);
    }

    /// The one-line summary, shared by the two paths that can produce it:
    /// a finished merge pass, and a cycle that never had a target to merge.
    fn auto_update_summary(fast_forwarded: usize, left_alone: usize) -> String {
        if left_alone == 0 {
            format!("auto-update: fast-forwarded {fast_forwarded}")
        } else {
            format!("auto-update: fast-forwarded {fast_forwarded}, {left_alone} left alone")
        }
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
    /// reported, not fixed (section 02), counted rather than named. That
    /// count spans both the repos that failed here and the ones
    /// [`maybe_complete_poll`](Self::maybe_complete_poll) never targeted.
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
        let left_alone =
            self.auto_update_total.saturating_sub(self.auto_update_ok) + self.auto_update_skipped;
        self.status_message = Some(Self::auto_update_summary(self.auto_update_ok, left_alone));
        self.auto_update_total = 0;
        self.auto_update_done = 0;
        self.auto_update_ok = 0;
        self.auto_update_skipped = 0;
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
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{app, probed};
    use super::*;
    use crate::executor::TaskEvent;
    use crate::ui::app::probe::RepoState;
    use std::time::Duration;

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
            changes: Default::default(),
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
                    changes: Default::default(),
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
                changes: Default::default(),
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

    /// The common shape of "left alone": a repo that came back behind and
    /// dirty is never eligible in the first place, so it never reaches the
    /// merge and never lands in the merge-time count. It still has to appear
    /// in the summary, or a skipped repo reads exactly like one with nothing
    /// to do.
    #[test]
    fn a_repo_skipped_at_eligibility_time_is_counted_as_left_alone() {
        let mut a = app(&["clean-behind", "dirty-behind"]);
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
            s.changed = usize::from(i == 1); // the second repo is dirty
            a.on_probe(generation, s);
        }

        let auto_targets = a
            .take_auto_update_requested()
            .expect("the clean repo is eligible");
        assert_eq!(auto_targets, vec![0], "the dirty repo is never a target");
        let cycle = a.auto_update_generation();

        a.on_auto_update_result(AutoUpdateResult {
            index: auto_targets[0],
            generation: cycle,
            outcome: AutoUpdateOutcome::FastForwarded,
        });
        assert_eq!(
            a.status_message.as_deref(),
            Some("auto-update: fast-forwarded 1, 1 left alone")
        );
    }

    /// A cycle that could merge nothing at all still has something honest to
    /// say, so the "no targets" path reports rather than going quiet, and
    /// leaves no phantom in-flight cycle behind it.
    #[test]
    fn a_cycle_that_merges_nothing_still_reports_what_it_left_alone() {
        let mut a = app(&["dirty-behind", "diverged"]);
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
            if i == 0 {
                s.changed = 1;
            } else {
                s.ahead = 1;
            }
            a.on_probe(generation, s);
        }

        assert!(
            a.take_auto_update_requested().is_none(),
            "nothing was eligible to merge"
        );
        assert_eq!(
            a.status_message.as_deref(),
            Some("auto-update: fast-forwarded 0, 2 left alone")
        );
        assert!(
            !a.auto_update_in_flight(),
            "a cycle that spawned no merges must not look like one still running"
        );
    }

    /// The other half of that: an idle tick, where nothing came back behind,
    /// must stay silent rather than announce itself every poll interval.
    #[test]
    fn a_cycle_that_found_nothing_behind_says_nothing() {
        let mut a = app(&["up-to-date"]);
        a.poll_enabled = true;
        a.auto_update = true;
        a.on_poll_due();
        let targets = a.take_poll_requested().expect("poll started");
        let generation = a.probe_generation;

        let mut s = probed(targets[0], "main");
        s.upstream = Some("origin/main".into());
        s.fetched = true;
        a.on_probe(generation, s);

        assert!(a.take_auto_update_requested().is_none());
        assert!(a.status_message.is_none(), "an idle tick stays quiet");
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
}
