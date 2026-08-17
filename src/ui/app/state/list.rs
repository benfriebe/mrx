//! The repo list: the filter, the selection, and where the cursor and the
//! scroll window are allowed to sit.

use super::App;
use crate::ui::app::render;

impl App {
    /// The global repo index at on-screen row `row` within the table body
    /// (0-based, below the header), given `scroll_offset`: the same lookup
    /// `move_cursor` uses, so a click and a keystroke can't disagree about
    /// which repo a row is.
    pub fn repo_at_row(&self, row: usize, scroll_offset: usize) -> Option<usize> {
        self.visible_indices().get(scroll_offset + row).copied()
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

    /// The repos a run would target: the explicit selection, or every
    /// visible row when nothing is selected.
    ///
    /// An explicit selection is honoured even if the active filter currently
    /// hides every member of it, since a filter narrows what's on screen,
    /// not what was already selected. The fallback has no such choice behind
    /// it, so it follows the filter exactly, and a zero-match filter targets
    /// nothing rather than reaching a repo that is no longer on screen.
    pub fn effective_selection(&self) -> Vec<usize> {
        if !self.selected.is_empty() {
            return self.selected.iter().copied().collect();
        }
        self.visible_indices()
    }

    /// Status text for an action that would otherwise act on a repo the
    /// filter currently hides, distinguishing an empty set from a filter
    /// that matches nothing.
    pub(super) fn no_visible_rows_message(&self) -> String {
        if self.filter.is_empty() {
            "no repos".into()
        } else {
            "no repos match the filter".into()
        }
    }

    pub(super) fn clamp_cursor_to_visible(&mut self) {
        let visible = self.visible_indices();
        if visible.contains(&self.cursor) {
            self.follow_cursor();
            return;
        }
        if let Some(&first) = visible.first() {
            self.cursor = first;
        }
        self.follow_cursor();
    }

    /// Pull [`list_scroll`](Self::list_scroll) the shortest distance that
    /// puts the cursor back on screen, leaving it alone while the cursor is
    /// already within the window. Render clamps the same way, so a path that
    /// misses this call still draws a visible cursor; it just loses the
    /// window's memory of where the user had scrolled to.
    fn follow_cursor(&mut self) {
        let visible = self.visible_indices();
        let height = render::list_height(self, self.terminal_height);
        self.list_scroll = render::list_start(self, &visible, height);
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
        self.follow_cursor();
    }

    /// Move the cursor `dir` half-pages, the same jump `Ctrl-D`/`Ctrl-U`
    /// make in the detail view, so the chord means the same thing in both.
    pub fn move_cursor_half_page(&mut self, dir: isize) {
        self.move_cursor(dir * self.half_page() as isize);
    }

    pub fn move_to_first(&mut self) {
        if let Some(&first) = self.visible_indices().first() {
            self.cursor = first;
        }
        self.follow_cursor();
    }

    pub fn move_to_last(&mut self) {
        if let Some(&last) = self.visible_indices().last() {
            self.cursor = last;
        }
        self.follow_cursor();
    }

    /// Toggle the cursor row's selection, then advance the cursor so
    /// repeated presses of the key walk down the list. A no-op when the
    /// cursor isn't on a row the filter currently shows: a zero-match filter
    /// leaves `cursor` pointing at a hidden repo, and toggling that would
    /// manufacture an explicit selection the user never saw on screen, which
    /// [`effective_selection`](Self::effective_selection) then honours.
    pub fn toggle_selection_at_cursor(&mut self) {
        if !self.visible_indices().contains(&self.cursor) {
            return;
        }
        if !self.selected.remove(&self.cursor) {
            self.selected.insert(self.cursor);
        }
        self.move_cursor(1);
    }

    /// Select every row the current filter shows, replacing whatever was
    /// selected before. A no-op with a status message when the filter hides
    /// every row, rather than a silent selection wipe.
    pub fn select_all_visible(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            self.status_message = Some(self.no_visible_rows_message());
            return;
        }
        self.selected = visible.into_iter().collect();
    }

    /// Select every repo in the set, filter or no filter, for building a
    /// selection that outlives the filter you used to find part of it. The
    /// filter-aware [`select_all_visible`](Self::select_all_visible) is the
    /// common one.
    pub fn select_all_in_set(&mut self) {
        if self.repos.is_empty() {
            self.status_message = Some("no repos".into());
            return;
        }
        self.selected = (0..self.repos.len()).collect();
    }

    /// Back to no selection, which means every visible repo again rather
    /// than none: see [`effective_selection`](Self::effective_selection).
    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// Flip the selection of every visible row; a row hidden by the filter
    /// keeps whatever selection state it already had. A no-op with a status
    /// message when the filter hides every row.
    pub fn invert_selection(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            self.status_message = Some(self.no_visible_rows_message());
            return;
        }
        for i in visible {
            if !self.selected.remove(&i) {
                self.selected.insert(i);
            }
        }
    }

    /// `/`: start a new search, dropping whatever a previous one committed.
    /// Esc in the list is a no-op by design, so `/` is the only way back to
    /// the full list once Enter has kept a filter.
    pub fn start_filter(&mut self) {
        self.filtering = true;
        self.filter.clear();
        self.clamp_cursor_to_visible();
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

#[cfg(test)]
mod tests {
    use super::super::testkit::{app, scrolling_app};
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn the_cursor_walks_back_up_through_the_window_before_it_scrolls() {
        let (mut a, height) = scrolling_app(20, 12);
        a.move_to_last();
        let bottom = a.repos.len() - height;
        assert_eq!(a.list_scroll, bottom);

        for _ in 1..height {
            a.move_cursor(-1);
        }
        assert_eq!(
            a.list_scroll, bottom,
            "the window holds still while the cursor crosses it"
        );

        a.move_cursor(-1);
        assert_eq!(
            a.list_scroll,
            bottom - 1,
            "only a cursor stepping off the top edge moves the window"
        );
    }

    #[test]
    fn a_filter_that_shortens_the_list_pulls_the_window_back_into_range() {
        let (mut a, _) = scrolling_app(20, 12);
        a.move_to_last();
        assert!(a.list_scroll > 0);

        a.start_filter();
        for c in "repo-0".chars() {
            a.filter_push(c); // ten rows, all of them above the window
        }
        assert_eq!(a.list_scroll, 0);
        assert!(a.visible_indices().contains(&a.cursor));
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
    fn an_empty_selection_means_every_visible_repo() {
        let mut a = app(&["foo", "bar", "baz"]);
        assert_eq!(a.effective_selection(), vec![0, 1, 2]);

        a.filter = "ba".into();
        assert_eq!(
            a.effective_selection(),
            vec![1, 2],
            "the fallback follows the filter, since that is what is on screen"
        );
    }

    #[test]
    fn an_explicit_selection_overrides_the_fallback() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.cursor = 2;
        a.selected.insert(0);
        assert_eq!(a.effective_selection(), vec![0]);
    }

    /// The cursor still indexes whatever repo it was on before the filter
    /// narrowed to zero, and the fallback must not act on it.
    #[test]
    fn a_zero_match_filter_makes_the_cursor_fallback_empty() {
        let mut a = app(&["foo", "bar"]);
        a.cursor = 0;
        a.filter = "zzz".into(); // matches nothing
        assert_eq!(a.effective_selection(), Vec::<usize>::new());
    }

    /// `selection_survives_a_filter_change` followed through to what a run
    /// actually targets.
    #[test]
    fn an_explicit_selection_still_targets_a_repo_the_filter_now_hides() {
        let mut a = app(&["foo", "bar"]);
        a.selected.insert(0);
        a.filter = "zzz".into(); // hides every row, foo included
        assert_eq!(a.effective_selection(), vec![0]);
    }

    /// Toggling must not manufacture an explicit selection out of a row the
    /// user can no longer see, since an explicit selection is then honoured
    /// across every later filter change.
    #[test]
    fn toggling_selection_on_a_zero_match_filter_is_a_no_op() {
        let mut a = app(&["foo"]);
        a.cursor = 0;
        a.filter = "zzz".into();
        a.toggle_selection_at_cursor();
        assert!(a.selected.is_empty());
        assert_eq!(a.effective_selection(), Vec::<usize>::new());
    }

    #[test]
    fn select_all_on_a_zero_match_filter_leaves_the_existing_selection_untouched() {
        let mut a = app(&["foo", "bar"]);
        a.selected.insert(0);
        a.filter = "zzz".into();
        a.select_all_visible();
        assert_eq!(
            a.selected,
            BTreeSet::from([0]),
            "must not silently wipe an explicit selection"
        );
        assert!(a.status_message.is_some());
    }

    #[test]
    fn invert_on_a_zero_match_filter_is_a_no_op_with_a_message() {
        let mut a = app(&["foo"]);
        a.filter = "zzz".into();
        a.invert_selection();
        assert!(a.selected.is_empty());
        assert!(a.status_message.is_some());
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
    fn start_filter_begins_a_fresh_search_rather_than_resuming_the_committed_one() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filtering = true;
        a.filter_push('b');
        a.commit_filter();

        a.start_filter();
        assert!(a.filtering);
        assert_eq!(a.filter, "");
        assert_eq!(a.visible_indices().len(), 3);
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

    #[test]
    fn a_selects_what_is_on_screen_and_shift_a_selects_the_whole_set() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filter = "ba".into();

        a.select_all_visible();
        assert_eq!(a.selected, BTreeSet::from([1, 2]));

        a.select_all_in_set();
        assert_eq!(
            a.selected,
            BTreeSet::from([0, 1, 2]),
            "the whole set, filter or no filter"
        );

        a.clear_selection();
        assert!(a.selected.is_empty());
    }

    #[test]
    fn a_click_row_resolves_to_the_same_repo_the_cursor_would_under_a_filter_and_scroll() {
        let mut a = app(&[
            "aardvark", "bar-1", "bar-2", "bar-3", "bar-4", "bar-5", "bar-6", "bar-7",
        ]);
        a.filter = "bar".into(); // "aardvark" is filtered out
        a.cursor = 7; // "bar-7", scrolled past the top of a short list

        let visible = a.visible_indices();
        let list_height = 3;
        let cursor_pos = visible.iter().position(|&i| i == a.cursor).unwrap();
        let scroll = render::scroll_offset(0, cursor_pos, visible.len(), list_height);
        assert!(
            scroll > 0,
            "the cursor must actually be scrolled for this test to mean anything"
        );

        let on_screen_row = cursor_pos - scroll;
        assert_eq!(a.repo_at_row(on_screen_row, scroll), Some(a.cursor));
    }
}
