//! The `:` palette: its filter, its cursor, and the selection verbs it offers
//! alongside real actions.

use super::App;
use crate::ui::app::actions::{Action, Source};

/// The palette's selection commands, named as verbs so typing `sel` at `:`
/// finds them. They mirror `A`, `a` and `c` in the list.
const SELECT_ALL: &str = "select-all";
const SELECT_VISIBLE: &str = "select-visible";
const DESELECT_ALL: &str = "deselect-all";

/// The palette's way into the run-command prompt, the same one `r` opens.
const RUN_COMMAND: &str = "run-command";

impl App {
    pub fn open_palette(&mut self) {
        self.palette_open = true;
        self.palette_filter.clear();
        self.palette_cursor = 0;
    }

    pub fn close_palette(&mut self) {
        self.palette_open = false;
    }

    /// Actions matching the palette's filter, in the same order `discover`
    /// returned them, then the run-command prompt and the selection commands.
    ///
    /// Every runnable entry's count is re-scoped to the current selection by
    /// [`targets_defining`](Self::targets_defining): `discover` counts the
    /// whole set, which is not the question being asked at the moment someone
    /// is picking what to run.
    pub fn palette_visible(&self) -> Vec<Action> {
        let targets = self.effective_selection();
        let all = self
            .actions
            .iter()
            .map(|a| Action {
                repos: self.targets_defining(a, &targets),
                ..a.clone()
            })
            .chain(std::iter::once(self.run_command_entry()))
            .chain(self.selection_commands());
        if self.palette_filter.is_empty() {
            return all.collect();
        }
        let needle = self.palette_filter.to_lowercase();
        all.filter(|a| a.name.to_lowercase().contains(&needle))
            .collect()
    }

    /// The palette's selection entries, carrying the count each would leave
    /// selected so the list says what it is about to do. Built on demand
    /// rather than stored, since the counts move with the filter and the
    /// selection.
    fn selection_commands(&self) -> Vec<Action> {
        let command = |name: &str, repos: usize| Action {
            name: name.to_string(),
            source: Source::Selection,
            repos,
        };
        vec![
            command(SELECT_ALL, self.repos.len()),
            command(SELECT_VISIBLE, self.visible_indices().len()),
            command(DESELECT_ALL, 0),
        ]
    }

    /// How many of `targets` would actually run `action`. A built-in verb and
    /// a `[DEFAULT]` key run everywhere, so only a per-repo key can come back
    /// short, and that shortfall is the number worth showing: those are the
    /// repos the run will silently skip.
    fn targets_defining(&self, action: &Action, targets: &[usize]) -> usize {
        match action.source {
            Source::PerRepo => targets
                .iter()
                .filter(|&&i| self.repos[i].keys.contains_key(&action.name))
                .count(),
            _ => targets.len(),
        }
    }

    /// The palette's run-command entry. Nothing defines it, so its count is
    /// the repos the body would run against rather than the repos it exists
    /// on.
    fn run_command_entry(&self) -> Action {
        Action {
            name: RUN_COMMAND.to_string(),
            source: Source::Prompt,
            repos: self.effective_selection().len(),
        }
    }

    /// Apply a [`Source::Selection`] palette entry. The palette is the only
    /// caller: these are the same three the list binds to `A`, `a` and `c`.
    fn run_selection_command(&mut self, name: &str) {
        match name {
            SELECT_ALL => self.select_all_in_set(),
            SELECT_VISIBLE => self.select_all_visible(),
            DESELECT_ALL => self.clear_selection(),
            other => debug_assert!(false, "palette offered an unknown command: {other}"),
        }
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

    /// Close the palette and carry out whatever it's currently pointing at,
    /// if anything matches the filter: a run for an action, a new selection
    /// for one of the selection commands, the prompt for the run-command one.
    pub fn palette_confirm(&mut self) {
        let chosen = self.palette_visible().get(self.palette_cursor).cloned();
        self.close_palette();
        let Some(action) = chosen else {
            return;
        };
        match action.source {
            Source::Selection => self.run_selection_command(&action.name),
            Source::Prompt => self.open_run_command(),
            _ => self.request_run(&action.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{app, probed};
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn palette_filter_narrows_the_action_list() {
        let a = app(&["foo"]);
        let all = a.palette_visible().len();
        let mut a = a;
        a.palette_filter = "upda".into();
        let visible = a.palette_visible();
        let filtered: Vec<&str> = visible.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(filtered, vec!["update"]);
        assert!(filtered.len() < all);
    }

    #[test]
    fn the_palette_selection_commands_change_the_selection_without_running() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filter = "ba".into();

        a.open_palette();
        a.palette_filter = SELECT_VISIBLE.into();
        a.palette_confirm();
        assert_eq!(a.selected, BTreeSet::from([1, 2]));
        assert!(
            a.run_requested.is_none(),
            "a selection command runs nothing"
        );

        a.open_palette();
        a.palette_filter = SELECT_ALL.into();
        a.palette_confirm();
        assert_eq!(
            a.selected,
            BTreeSet::from([0, 1, 2]),
            "the filter is no bound on select-all"
        );

        a.open_palette();
        a.palette_filter = DESELECT_ALL.into();
        a.palette_confirm();
        assert!(a.selected.is_empty());
        assert!(a.run_requested.is_none());
    }

    /// `select-visible` and `select-all` differ only under a filter, so the
    /// palette says how many each would leave selected.
    #[test]
    fn the_palette_counts_what_each_selection_command_would_select() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filter = "ba".into();
        a.palette_filter = "select".into();

        let counts: Vec<(String, usize)> = a
            .palette_visible()
            .iter()
            .map(|c| (c.name.clone(), c.repos))
            .collect();
        assert_eq!(
            counts,
            vec![
                (SELECT_ALL.into(), 3),
                (SELECT_VISIBLE.into(), 2),
                (DESELECT_ALL.into(), 0),
            ]
        );
    }

    /// The palette is the second way to the prompt, and the one that could
    /// most easily fire a run named `run-command` instead of opening it.
    #[test]
    fn the_palette_run_command_entry_opens_the_prompt_rather_than_running() {
        let mut a = app(&["foo", "bar"]);
        a.on_probe(0, probed(0, "main")); // clean and known, so a run would start at once
        a.open_palette();
        a.palette_filter = RUN_COMMAND.into();
        a.palette_confirm();

        assert!(a.run_command_open);
        assert!(a.run_requested.is_none());
        assert!(a.pending_run.is_none());
    }

    /// A selection is the whole point of the palette, and a count reading
    /// "99 of 99" while two rows are selected says the opposite of what the
    /// run will do.
    #[test]
    fn a_selection_narrows_what_the_palette_says_an_action_will_run_on() {
        let mut a = app(&["foo", "bar", "baz"]);
        let count = |a: &App, name: &str| {
            a.palette_visible()
                .into_iter()
                .find(|x| x.name == name)
                .expect("the action is offered")
                .repos
        };
        assert_eq!(count(&a, "update"), 3, "no selection means every repo");

        a.selected = BTreeSet::from([0, 2]);
        assert_eq!(count(&a, "update"), 2);
    }

    /// The count a per-repo action loses is the warning: those repos are
    /// skipped rather than failed, so the palette is the last place to say so.
    #[test]
    fn a_per_repo_action_counts_only_the_selected_repos_that_define_it() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.repos[0]
            .keys
            .insert("deploy".into(), "./deploy.sh".into());
        a.actions = crate::ui::app::actions::discover(&a.repos, &a.defaults);

        a.selected = BTreeSet::from([0, 1]);
        let deploy = a
            .palette_visible()
            .into_iter()
            .find(|x| x.name == "deploy")
            .expect("the action is offered");
        assert_eq!(deploy.repos, 1, "only foo defines it");
        assert_eq!(deploy.source, Source::PerRepo);
    }

    #[test]
    fn the_palette_counts_the_repos_a_typed_command_would_run_against() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.selected = BTreeSet::from([0, 2]);
        a.palette_filter = RUN_COMMAND.into();

        let entry = a.palette_visible().pop().expect("the entry is offered");
        assert_eq!(entry.repos, 2, "the effective selection, not the whole set");
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
}
