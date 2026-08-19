//! The freshness poll and the fast-forward pass it feeds, including the
//! counters that track a cycle from start to summary.

use super::App;
use crate::ui::app::poll::{self, AutoUpdateOutcome, AutoUpdateResult};
use std::time::{Duration, Instant};

impl App {
    /// The tick loop's poll `Interval` arm always fires; this is what decides
    /// whether a given tick actually does anything. Suspended rather than
    /// queued while a run is live: a fetch storm competing with a live update
    /// for the network is worse than a poll landing a cycle late.
    pub fn on_poll_due(&mut self) {
        if !self.poll_enabled || self.run_action.is_some() {
            return;
        }
        let targets: Vec<usize> = (0..self.repos.len()).collect();
        let generation = self.begin_probe(&targets);
        self.poll_generation = Some(generation);
        self.poll_targets_requested = Some(targets);
        self.last_poll_at = Some(Instant::now());
    }

    /// Apply a set's `[DEFAULT] auto_fetch`. A config that says nothing leaves
    /// the poll alone rather than turning it off, so `F` still decides for a
    /// set with no opinion, and a session restored afterwards still overrides
    /// what this sets.
    pub fn apply_auto_fetch(&mut self, configured: Option<Duration>) {
        let Some(interval) = configured else {
            return;
        };
        self.poll_enabled = !interval.is_zero();
        if self.poll_enabled {
            self.poll_interval = poll::clamp_interval(interval);
        } else {
            self.auto_update = false;
        }
    }

    /// Owe an opening fetch when the poll is on and nothing on screen has a
    /// sync answer yet: without it a first run leaves every ↓ blank for a
    /// whole interval, which reads as "up to date" rather than "not asked".
    /// Called once the poll's settings have settled, config and session both.
    pub fn arm_boot_fetch(&mut self) {
        self.boot_fetch_pending = self.poll_enabled && self.fetched_repos.is_empty();
    }

    /// Whether the opening fetch should go now, claimed once. Held back until
    /// the opening probe has finished, so the table fills from the fast local
    /// read before a cycle of network calls supersedes its generation.
    pub fn take_boot_fetch(&mut self) -> bool {
        if !self.boot_fetch_pending || !self.probing.is_empty() {
            return false;
        }
        self.boot_fetch_pending = false;
        true
    }

    /// The header's "checked 12s ago", once a cycle has run this session.
    /// Absent rather than "never": a session that has not polled has nothing
    /// to report, and saying so takes header room from what does.
    pub fn last_check_text(&self) -> Option<String> {
        self.last_poll_at
            .map(|at| format!("checked {}", poll::format_ago(at.elapsed())))
    }

    pub fn take_poll_requested(&mut self) -> Option<Vec<usize>> {
        self.poll_targets_requested.take()
    }

    /// `F`: turn the freshness poll on or off. The interval stays whatever
    /// it last was rather than resetting to the default every time. Turning
    /// it off also turns auto-update off, which has nothing to act on
    /// without a poll feeding it fresh data.
    pub fn toggle_poll(&mut self) {
        self.poll_enabled = !self.poll_enabled;
        if !self.poll_enabled {
            self.auto_update = false;
        }
    }

    /// `Ctrl-A`: turn auto-update on or off. Refuses while the poll itself
    /// is off, since auto-update only ever acts on a poll's results.
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
        // Per-repo freshness is already recorded as each result lands, in
        // `on_probe`; there is nothing left to do here for that.
        if !self.auto_update {
            return;
        }
        // A run that started mid-cycle suspends the fast-forward pass, the
        // same way `on_poll_due` suspends the poll itself.
        if self.run_action.is_some() {
            return;
        }
        // Refuse to start a second cycle on top of one still in flight: a
        // late result from the first would otherwise land against the
        // second cycle's counters.
        if self.auto_update_in_flight() {
            return;
        }
        // A repo whose fetch failed keeps whatever stale ahead/behind it
        // already had, so `s.fetched` is what makes it ineligible.
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
    /// with, for the caller to tag its `spawn_auto_update` call with.
    pub fn auto_update_generation(&self) -> u64 {
        self.auto_update_generation
    }

    pub fn take_auto_update_requested(&mut self) -> Option<Vec<usize>> {
        self.auto_update_requested.take()
    }

    /// Apply one repo's outcome from an auto-update pass, unless it belongs
    /// to a cycle a later one has since superseded. Once every targeted repo
    /// has reported in, leaves a one-line summary in the status bar: repos a
    /// fast-forward could not touch are reported rather than fixed, counted
    /// rather than named, and that count spans both the repos that failed
    /// here and the ones
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

    pub fn take_auto_update_reprobe_targets(&mut self) -> Option<Vec<usize>> {
        self.auto_update_reprobe_targets.take()
    }

    /// The header's `poll 5m` / `poll 5m · auto` text, `None` when the poll
    /// is off, so a mode that modifies repos is never invisible on screen.
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
    fn a_configured_auto_fetch_turns_the_poll_on_at_its_own_interval() {
        let mut a = app(&["foo"]);
        assert!(!a.poll_enabled, "off unless something says otherwise");

        a.apply_auto_fetch(Some(Duration::from_secs(120)));
        assert!(a.poll_enabled);
        assert_eq!(a.poll_interval, Duration::from_secs(120));

        a.apply_auto_fetch(Some(Duration::ZERO));
        assert!(!a.poll_enabled, "off is a value, not an absence");
    }

    /// A set with no opinion must not overrule `F`, or switching into one
    /// would silently stop a poll the user turned on.
    #[test]
    fn a_config_that_says_nothing_leaves_the_poll_alone() {
        let mut a = app(&["foo"]);
        a.poll_enabled = true;
        a.poll_interval = Duration::from_secs(90);

        a.apply_auto_fetch(None);
        assert!(a.poll_enabled);
        assert_eq!(a.poll_interval, Duration::from_secs(90));
    }

    #[test]
    fn turning_auto_fetch_off_takes_auto_update_with_it() {
        let mut a = app(&["foo"]);
        a.apply_auto_fetch(Some(Duration::from_secs(120)));
        a.auto_update = true;

        a.apply_auto_fetch(Some(Duration::ZERO));
        assert!(!a.auto_update, "auto-update has nothing to act on");
    }

    /// The opening fetch waits for the opening probe: firing it first bumps
    /// the generation the probe's own results are tagged with, so the table
    /// would sit blank through a set's worth of network calls.
    #[test]
    fn the_opening_fetch_waits_for_the_opening_probe_to_land() {
        let mut a = app(&["foo", "bar"]);
        a.apply_auto_fetch(Some(Duration::from_secs(360)));
        a.arm_boot_fetch();

        let generation = a.begin_probe(&[0, 1]);
        assert!(!a.take_boot_fetch(), "a probe is still in flight");

        a.on_probe(generation, probed(0, "main"));
        assert!(!a.take_boot_fetch(), "one of two is not the set");

        a.on_probe(generation, probed(1, "main"));
        assert!(a.take_boot_fetch());
        assert!(!a.take_boot_fetch(), "claimed once, not every frame");
    }

    #[test]
    fn a_set_that_already_has_sync_answers_waits_for_its_first_interval() {
        let mut a = app(&["foo"]);
        a.apply_auto_fetch(Some(Duration::from_secs(360)));
        a.fetched_repos.insert(0);
        a.arm_boot_fetch();

        assert!(
            !a.take_boot_fetch(),
            "something has already fetched, so there is nothing to catch up on"
        );
    }

    #[test]
    fn the_header_reports_when_the_last_cycle_went_out() {
        let mut a = app(&["foo"]);
        assert_eq!(a.last_check_text(), None, "nothing has been checked yet");

        a.poll_enabled = true;
        a.on_poll_due();
        assert_eq!(a.last_check_text().as_deref(), Some("checked 0s ago"));

        a.last_poll_at = Some(Instant::now() - Duration::from_secs(150));
        assert_eq!(a.last_check_text().as_deref(), Some("checked 2m ago"));
    }

    /// A cycle that never started is not a check, so the clock must not move.
    #[test]
    fn a_suppressed_cycle_does_not_count_as_a_check() {
        let mut a = app(&["foo"]);
        a.on_poll_due();
        assert_eq!(a.last_poll_at, None, "the poll is off");

        a.poll_enabled = true;
        a.begin_named_run("update".into(), vec![0]);
        a.on_task(1, TaskEvent::Started { index: 0 });
        a.on_poll_due();
        assert_eq!(a.last_poll_at, None, "a live run suspends the poll");
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
        // The poll starts with nothing running, then a run begins before
        // every repo's fetch has landed.
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
