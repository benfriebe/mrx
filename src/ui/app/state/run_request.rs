//! Deciding whether a run may start: the dirty and unprobed confirmation and
//! its three answers.

use super::App;
use crate::ui::app::actions;

/// A run waiting on user confirmation because part of its target selection
/// is dirty, or because part of it has no probe result yet and dirtiness is
/// simply unknown.
pub struct PendingRun {
    pub action: String,
    /// The shell body of an ad-hoc run, `None` for a named action the run
    /// loop is to look up instead.
    pub body: Option<String>,
    pub targets: Vec<usize>,
    pub dirty_count: usize,
    pub unknown_count: usize,
    /// The cursor row, when the targets are every visible repo only because
    /// nothing was selected. The prompt offers it as a third answer, since
    /// an empty selection meaning "all" is easy to walk into.
    pub cursor_only: Option<usize>,
}

/// A run that's been decided on (no confirmation needed, or confirmed) and
/// is ready for the run loop to plan and spawn.
pub struct RunRequest {
    pub action: String,
    /// As on [`PendingRun`], carried across the confirmation unchanged.
    pub body: Option<String>,
    pub targets: Vec<usize>,
}

impl App {
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

    /// How many of `targets` have no probe result yet, which is every repo right
    /// after startup, a set switch or a config reload. Treated as dirty for the
    /// confirmation in [`request_run`](Self::request_run).
    pub fn unprobed_count(&self, targets: &[usize]) -> usize {
        targets
            .iter()
            .filter(|&&i| self.probes.get(i).and_then(|p| p.as_ref()).is_none())
            .count()
    }

    /// Ask to run `action_name` over the effective selection. Goes straight to
    /// `run_requested` when nothing in it is dirty or unprobed, or when `force`
    /// is set; otherwise waits on confirmation. Refused by
    /// [`mutation_blocker`](Self::mutation_blocker).
    pub fn request_run(&mut self, action_name: &str) {
        debug_assert!(
            self.actions.iter().any(|a| a.name == action_name),
            "the palette must never offer an action nothing defines: {action_name}"
        );
        self.request(action_name, None);
    }

    /// Ask to run an ad-hoc shell `body` over the effective selection, under
    /// the label [`actions::body_label`] derives from it. No assertion to
    /// match [`request_run`](Self::request_run)'s: a body typed at the prompt
    /// is by definition not one of `self.actions`.
    pub fn request_run_body(&mut self, body: &str) {
        self.request(&actions::body_label(body), Some(body.to_string()));
    }

    /// Run `action_name` over exactly `targets`, with no confirmation step.
    /// Auto-update's way in: its targets are its own, nobody is at the keyboard
    /// to answer a prompt, and every target has already probed clean.
    pub(super) fn request_run_over(&mut self, action_name: &str, targets: Vec<usize>) {
        if self.refuse_if_mutation_blocked("start a run") {
            return;
        }
        self.run_requested = Some(RunRequest {
            action: action_name.to_string(),
            body: None,
            targets,
        });
    }

    /// What the two entry points above share: everything from here on is the
    /// same whether the run has a name or a body.
    fn request(&mut self, action_name: &str, body: Option<String>) {
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
            let cursor_only =
                (self.selected.is_empty() && targets.len() > 1 && targets.contains(&self.cursor))
                    .then_some(self.cursor);
            self.pending_run = Some(PendingRun {
                action: action_name.to_string(),
                body,
                targets,
                dirty_count: dirty,
                unknown_count: unknown,
                cursor_only,
            });
        } else {
            self.run_requested = Some(RunRequest {
                action: action_name.to_string(),
                body,
                targets,
            });
        }
    }

    /// Confirm a pending run. Re-checks
    /// [`mutation_blocker`](Self::mutation_blocker) rather than trusting the
    /// check [`request_run`](Self::request_run) made: an auto-update pass
    /// can start while the prompt sits waiting for an answer.
    pub fn confirm_pending_run(&mut self) {
        let Some(p) = self.pending_run.take() else {
            return;
        };
        if self.refuse_if_mutation_blocked("start a run") {
            return;
        }
        self.run_requested = Some(RunRequest {
            action: p.action,
            body: p.body,
            targets: p.targets,
        });
    }

    /// Confirm a pending run, narrowed to the cursor row. A no-op unless the
    /// prompt offered the choice, so the keystroke can never silently
    /// rewrite a deliberate selection down to one repo.
    pub fn confirm_pending_run_at_cursor(&mut self) {
        let Some(cursor) = self.pending_run.as_ref().and_then(|p| p.cursor_only) else {
            return;
        };
        let Some(p) = self.pending_run.take() else {
            return;
        };
        if self.refuse_if_mutation_blocked("start a run") {
            return;
        }
        self.run_requested = Some(RunRequest {
            action: p.action,
            body: p.body,
            targets: vec![cursor],
        });
    }

    pub fn cancel_pending_run(&mut self) {
        self.pending_run = None;
    }

    pub fn take_run_requested(&mut self) -> Option<RunRequest> {
        self.run_requested.take()
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{app, probed};
    use std::collections::BTreeSet;

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
        // No probe result has come back yet: dirtiness is unknown, not clean.
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

    /// A poll completing doesn't wait on a modal, so auto-update can start a
    /// run of its own while the user is still deciding.
    #[test]
    fn confirm_pending_run_refuses_when_a_run_starts_while_the_prompt_is_open() {
        let mut a = app(&["foo"]);
        let mut dirty = probed(0, "main");
        dirty.changed = 1;
        a.on_probe(0, dirty);

        a.request_run("update");
        assert!(a.pending_run.is_some(), "a dirty run needs confirming");

        a.begin_named_run("update".into(), vec![0]); // starts while the prompt is still open
        a.confirm_pending_run();

        assert!(
            a.run_requested.is_none(),
            "must not launch into a repo a live run already owns"
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

    /// A run over everything is one keystroke away with nothing selected,
    /// so the prompt that catches it also offers the narrower answer.
    #[test]
    fn confirming_at_the_cursor_narrows_an_all_repos_run_to_one() {
        let mut a = app(&["foo", "bar", "baz"]);
        for i in 0..3 {
            let mut dirty = probed(i, "main");
            dirty.changed = 1;
            a.on_probe(0, dirty);
        }
        a.cursor = 1;

        a.request_run("update");
        let pending = a.pending_run.as_ref().unwrap();
        assert_eq!(pending.targets.len(), 3, "an empty selection means all");
        assert_eq!(pending.cursor_only, Some(1));

        a.confirm_pending_run_at_cursor();
        assert!(a.pending_run.is_none());
        assert_eq!(a.run_requested.as_ref().unwrap().targets, vec![1]);
    }

    #[test]
    fn the_cursor_answer_is_not_offered_against_a_selection_the_user_made() {
        let mut a = app(&["foo", "bar", "baz"]);
        for i in 0..3 {
            let mut dirty = probed(i, "main");
            dirty.changed = 1;
            a.on_probe(0, dirty);
        }
        a.selected = BTreeSet::from([0, 2]);
        a.cursor = 1;

        a.request_run("update");
        assert_eq!(a.pending_run.as_ref().unwrap().cursor_only, None);

        a.confirm_pending_run_at_cursor();
        assert!(
            a.run_requested.is_none(),
            "an answer the prompt never offered must not rewrite the selection"
        );
        assert!(a.pending_run.is_some(), "and must not dismiss the prompt");
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
}
