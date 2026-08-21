//! A run that is already live: its events, counters, transcript, expiry, and
//! the two ways it ends.

use super::{App, NEVER_RUN};
use crate::executor::{StepResult, TaskEvent};
use crate::summarize;
use std::time::{Duration, Instant};

/// How long a result stays on its row before the column goes back to
/// [`NEVER_RUN`]. Long enough to run something, go and read it, and come
/// back; short enough that a table left open all afternoon isn't still
/// reporting this morning.
pub const DEFAULT_RESULT_TTL: Duration = Duration::from_mins(6);

/// What joins the header's status pieces, and so what marks the seam a
/// narrow header is allowed to shed at.
pub const SEGMENT_SEP: &str = " · ";

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
            // Nothing to report yet: the finished result carries the real
            // code, and a live step's heading is drawn without one.
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
    /// The step currently in flight, named so a row reads `post_update`
    /// instead of a fixed `running...`.
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
    /// Start a new run generation and return its id, so the caller can tag
    /// the `spawn_run` call it is about to make with it.
    pub fn begin_run(&mut self) -> u64 {
        self.run_id += 1;
        self.run_id
    }

    /// Begin a named run over `targets`: bumps the run id, resets the live
    /// counters the header reads, and drops each target's previous result,
    /// so the caller only has to plan and spawn the operations themselves.
    ///
    /// A target carrying the last run's result reads as already reported in,
    /// which [`cancel_counts`](Self::cancel_counts) would then count as
    /// neither queued nor finishing. Only the targets are cleared: a row this
    /// run is not acting on keeps its outcome until
    /// [`expire_results`](Self::expire_results) retires it.
    pub fn begin_named_run(&mut self, action: String, targets: Vec<usize>) -> u64 {
        let run_id = self.begin_run();
        self.run_action = Some(action);
        self.run_total = targets.len();
        self.run_completed = 0;
        self.run_failed = 0;
        for &index in &targets {
            self.forget_run(index);
        }
        self.run_targets = targets;
        run_id
    }

    /// Drop everything the last run left behind for one repo.
    ///
    /// The scroll pin belongs to the transcript it was scrolled against, so it
    /// has to go with it: leaving it behind opens the next run's output at an
    /// offset into output that no longer exists, instead of following its own
    /// tail. `live` is not cleared here because it never survives to be: every
    /// caller is gated on no run being in flight, and `on_task` drops each
    /// index as it finishes.
    fn forget_run(&mut self, index: usize) {
        if let Some(slot) = self.run_results.get_mut(index) {
            *slot = None;
        }
        self.result_at.remove(&index);
        self.detail_scroll.remove(&index);
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
                // the same text either way.
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
    /// never touched.
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
            self.forget_run(index);
        }
    }

    pub fn take_post_run_targets(&mut self) -> Option<Vec<usize>> {
        self.post_run_targets.take()
    }

    /// `Esc`: ask the live run to stop queueing new work. A no-op when
    /// nothing is running, so it's safe to bind unconditionally.
    ///
    /// `Command::output().await` has no kill, so a repo already past its
    /// semaphore permit keeps running to completion; only a repo still
    /// waiting behind it turns into a skip. Both counts are a snapshot as of
    /// the keypress, not a promise that stays accurate as events arrive.
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
        self.run_targets
            .iter()
            .fold((0, 0), |(queued, finishing), &i| {
                match self.run_results.get(i).and_then(|r| r.as_ref()) {
                    None => (queued + 1, finishing),
                    Some(RunStatus::Running | RunStatus::Step { .. }) => (queued, finishing + 1),
                    _ => (queued, finishing),
                }
            })
    }

    pub fn take_cancel_requested(&mut self) -> bool {
        std::mem::take(&mut self.cancel_requested)
    }

    /// `q`/`Ctrl-C`: quits immediately, unless a run is live, in which case
    /// it opens a confirmation first rather than losing sight of whether
    /// anything was left running. Returns whether the caller should quit now.
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

    /// The header's right-hand text: the live run's summary while one is
    /// running, otherwise the selection count, with the poll's state and a
    /// restored filter's match count layered on. A restored filter is shown
    /// here, not only in the status bar, since "4 of 42 repos" with no
    /// explanation otherwise looks like a broken config.
    pub fn header_right_text(&self) -> String {
        self.header_right_segments().join(SEGMENT_SEP)
    }

    /// The same text as the pieces it is built from, ordered by how much a
    /// narrow header should want to keep them: what a header cannot fit it
    /// sheds from the end, one whole piece at a time.
    ///
    /// The order is not arbitrary. A count answers "what am I looking at",
    /// a selection changes what the next run does, and the poll's cadence is
    /// static. The two on the end restate something already on screen: the
    /// SYNC column carries its own sort arrow, and a stale check shows up as
    /// counts that stop moving.
    pub fn header_right_segments(&self) -> Vec<String> {
        if let Some(run) = self.run_status_text() {
            return vec![run];
        }
        let mut parts = vec![if self.filter.is_empty() {
            format!("{} repos", self.repos.len())
        } else {
            format!(
                "{} of {} repos · filter",
                self.visible_indices().len(),
                self.repos.len()
            )
        }];
        // An empty selection already means every visible repo, and calling
        // that "42 selected" would make the two states indistinguishable.
        if !self.selected.is_empty() {
            parts.push(format!("{} selected", self.selected.len()));
        }
        parts.extend(self.poll_status_text());
        parts.extend(self.last_check_text());
        if !self.sorted_by_default() {
            parts.push(self.sort_label());
        }
        parts
    }

    /// The live run's summary for the header: action name, done/total, and
    /// a failure count once there's one to show.
    fn run_status_text(&self) -> Option<String> {
        let action = self.run_action.as_ref()?;
        let progress = format!("{action} {}/{}", self.run_completed, self.run_total);
        if self.run_failed == 0 {
            return Some(progress);
        }
        Some(format!("{progress} · {} failed", self.run_failed))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{ago, app};
    use super::*;

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
        a.result_ttl = Some(Duration::from_mins(1));
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

        a.result_at.insert(0, ago(Duration::from_secs(61)));
        a.expire_results();
        assert_eq!(a.result_text(0), NEVER_RUN);
    }

    #[test]
    fn results_never_expire_while_the_run_that_made_them_is_still_going() {
        let mut a = app(&["foo", "bar"]);
        a.result_ttl = Some(Duration::from_mins(1));
        a.run_action = Some("update".into());
        a.run_results[0] = Some(RunStatus::Running);
        a.result_at.insert(0, ago(Duration::from_mins(10)));
        a.expire_results();
        assert!(a.run_results[0].is_some(), "a live run is not stale");
    }

    #[test]
    fn results_are_kept_indefinitely_when_the_ttl_is_off() {
        let mut a = app(&["foo"]);
        a.result_ttl = None;
        a.run_results[0] = Some(RunStatus::Skipped { reason: "x".into() });
        a.result_at.insert(0, ago(Duration::from_secs(9999)));
        a.expire_results();
        assert!(a.run_results[0].is_some());
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

    /// The finished result of a run is what a still-queued target of the
    /// next one carries until that one reaches it, so the count has to be
    /// taken against this run's results rather than whatever is left over.
    #[test]
    fn cancelling_the_second_run_of_a_session_still_counts_its_queued_targets() {
        let mut a = app(&["a", "b", "c"]);
        let first = a.begin_named_run("update".into(), vec![0, 1, 2]);
        for index in 0..3 {
            a.on_task(
                first,
                TaskEvent::Finished {
                    index,
                    steps: vec![StepResult {
                        label: "update".into(),
                        shape: summarize::Shape::Generic,
                        stdout: "Already up to date.\n".into(),
                        stderr: String::new(),
                        code: 0,
                    }],
                    exit_code: 0,
                },
            );
        }

        let second = a.begin_named_run("update".into(), vec![0, 1, 2]);
        a.on_task(second, TaskEvent::Started { index: 0 });

        a.request_cancel();
        assert_eq!(
            a.status_message.as_deref(),
            Some("cancelled, 2 queued skipped, 1 still finishing")
        );
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
    /// The pin belongs to the transcript it was scrolled against. Keeping it
    /// across a re-run opened the new output partway down instead of at the
    /// tail it is still arriving on.
    #[test]
    fn a_rerun_drops_the_scroll_pin_the_last_transcript_left() {
        let mut a = app(&["foo", "bar"]);
        a.detail_scroll.insert(0, 4);
        a.detail_scroll.insert(1, 7);

        a.begin_named_run("update".into(), vec![0]);

        assert_eq!(a.detail_scroll.get(&0), None, "the target keeps its pin");
        assert_eq!(
            a.detail_scroll.get(&1),
            Some(&7),
            "a row this run is not acting on keeps its own"
        );
    }
}
