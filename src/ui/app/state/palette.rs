//! The `:` palette: its filter, its cursor, and the selection verbs it offers
//! alongside real actions.

use super::App;
use crate::ui::app::actions::{Action, Source};

/// The palette's selection commands, named as verbs so typing `sel` at `:`
/// finds them. They mirror `A`, `a` and `c` in the list.
const SELECT_ALL: &str = "select-all";
const SELECT_VISIBLE: &str = "select-visible";
const DESELECT_ALL: &str = "deselect-all";

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
    /// returned them, then the selection commands.
    pub fn palette_visible(&self) -> Vec<Action> {
        let all = self
            .actions
            .iter()
            .cloned()
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
    /// rather than stored: the counts move with the filter and the
    /// selection, and a stale count in a menu is worse than no count.
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
    /// for one of the selection commands.
    pub fn palette_confirm(&mut self) {
        let chosen = self.palette_visible().get(self.palette_cursor).cloned();
        self.close_palette();
        let Some(action) = chosen else {
            return;
        };
        if action.source == Source::Selection {
            self.run_selection_command(&action.name);
        } else {
            self.request_run(&action.name);
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
