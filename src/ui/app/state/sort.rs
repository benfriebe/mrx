//! The order the table lists rows in: which column, which way, and what
//! "in order" means for each column's contents.

use super::{App, RunStatus};
use crate::ui::app::probe::{self, RepoState};
use std::cmp::Ordering;

/// A column the table can be ordered by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Repo,
    Branch,
    State,
    Result,
}

impl Sort {
    /// Every column, in the order the sort menu offers them, which is the
    /// order they are drawn in.
    pub const ALL: &'static [Sort] = &[Sort::Repo, Sort::Branch, Sort::State, Sort::Result];

    /// The column header this order's arrow is drawn on.
    pub fn header(self) -> &'static str {
        match self {
            Sort::Repo => "REPO",
            Sort::Branch => "BRANCH",
            Sort::State => "STATE",
            Sort::Result => "RESULT",
        }
    }

    /// The key the sort menu binds it to, each column's own initial except
    /// RESULT, whose `r` is spent on the column above it.
    pub fn key(self) -> char {
        match self {
            Sort::Repo => 'r',
            Sort::Branch => 'b',
            Sort::State => 's',
            Sort::Result => 'l',
        }
    }

    /// The direction a column opens in the first time it is chosen. Text
    /// reads forwards; a count of things gone wrong reads worst-first,
    /// which is the only reason to order by it.
    pub fn natural(self) -> Direction {
        match self {
            Sort::Repo | Sort::Branch => Direction::Ascending,
            Sort::State | Sort::Result => Direction::Descending,
        }
    }

    /// The column whose key is `c`, if the sort menu binds one.
    pub fn from_key(c: char) -> Option<Sort> {
        Sort::ALL.iter().copied().find(|s| s.key() == c)
    }

    /// The name the session file stores, and reads back through
    /// [`from_name`](Self::from_name).
    pub fn name(self) -> &'static str {
        match self {
            Sort::Repo => "repo",
            Sort::Branch => "branch",
            Sort::State => "state",
            Sort::Result => "result",
        }
    }

    pub fn from_name(name: &str) -> Option<Sort> {
        Sort::ALL.iter().copied().find(|s| s.name() == name)
    }
}

/// Which way a sorted column reads.
///
/// Kept beside [`Sort`] rather than folded into it, since which column is
/// active never changes what ascending means for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

impl Direction {
    /// The other way, which choosing the active column again switches to.
    pub fn reversed(self) -> Self {
        match self {
            Direction::Ascending => Direction::Descending,
            Direction::Descending => Direction::Ascending,
        }
    }

    /// The glyph the sorted column's header carries.
    pub fn arrow(self) -> &'static str {
        match self {
            Direction::Ascending => "↑",
            Direction::Descending => "↓",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Direction::Ascending => "asc",
            Direction::Descending => "desc",
        }
    }

    pub fn from_name(name: &str) -> Option<Direction> {
        match name {
            "asc" => Some(Direction::Ascending),
            "desc" => Some(Direction::Descending),
            _ => None,
        }
    }
}

impl App {
    /// Apply `sort`, the same key the sort menu binds it to: the active
    /// column reverses in place, and any other column opens at its own
    /// natural direction rather than carrying the last column's over.
    pub fn choose_sort(&mut self, sort: Sort) {
        self.sort_direction = if self.sort == sort {
            self.sort_direction.reversed()
        } else {
            sort.natural()
        };
        self.sort = sort;
        // The cursor stays on its repo, but that repo has moved; the window
        // has to move with it.
        self.clamp_cursor_to_visible();
    }

    /// How the header names the order in force, by the same column name the
    /// table's own header carries, for the panes narrow enough to have
    /// dropped that column altogether.
    pub fn sort_label(&self) -> String {
        format!(
            "sort {} {}",
            self.sort.header(),
            self.sort_direction.arrow()
        )
    }

    /// The order a fresh session opens on, which the header line leaves
    /// unsaid: a table in name order already looks like one.
    pub(super) fn sorted_by_default(&self) -> bool {
        self.sort == Sort::default() && self.sort_direction == Sort::default().natural()
    }

    /// The arrow to draw on `column`'s header, if that is the sorted one.
    pub fn sort_arrow(&self, column: Sort) -> Option<&'static str> {
        (self.sort == column).then(|| self.sort_direction.arrow())
    }

    /// Put `rows` in the order the table lists them.
    ///
    /// Every sort here is stable and `rows` arrives in index order, which
    /// `config::load` already made name order, so rows a column cannot
    /// separate stay alphabetical instead of swapping between frames.
    pub(super) fn apply_sort(&self, rows: &mut [usize]) {
        let descending = self.sort_direction == Direction::Descending;
        match self.sort {
            // Index order is already name order, so there is nothing to
            // compare: the whole column is one `reverse` away either way.
            Sort::Repo => {
                if descending {
                    rows.reverse();
                }
            }
            Sort::Branch => sort_unknown_last(rows, descending, |i| self.branch_key(i)),
            Sort::State => sort_unknown_last(rows, descending, |i| self.state_key(i)),
            Sort::Result => sort_unknown_last(rows, descending, |i| Some(self.result_rank(i))),
        }
    }

    /// BRANCH as the column prints it, so the order matches what is on
    /// screen rather than a name the row never shows.
    fn branch_key(&self, idx: usize) -> Option<String> {
        self.known_probe(idx)
            .map(|state| probe::branch_text(state).to_lowercase())
    }

    /// STATE's counts in the order the column itself prints them, so
    /// descending is "carrying the most", with no hidden weighting between
    /// uncommitted work and unpushed commits.
    fn state_key(&self, idx: usize) -> Option<(usize, u32, u32)> {
        self.known_probe(idx)
            .map(|state| (state.changed, state.ahead, state.behind))
    }

    /// A probe whose fields can be ordered. A timed-out one is excluded for
    /// the reason [`RepoState::timed_out`] gives: every other field is a
    /// default, so sorting by them would rank a repo mrx never read.
    fn known_probe(&self, idx: usize) -> Option<&RepoState> {
        self.probes
            .get(idx)
            .and_then(|p| p.as_ref())
            .filter(|state| !state.timed_out)
    }

    /// RESULT ranked by how much the row is asking for attention, so
    /// descending brings the failures to the top.
    fn result_rank(&self, idx: usize) -> u8 {
        match self.run_results.get(idx).and_then(|r| r.as_ref()) {
            None => 0,
            Some(RunStatus::Skipped { .. }) => 1,
            Some(RunStatus::Running | RunStatus::Step { .. }) => 2,
            Some(RunStatus::Finished { exit_code, .. }) if *exit_code == 0 => 3,
            Some(RunStatus::Finished { .. }) => 4,
        }
    }
}

/// Order `rows` by `key`, with the rows that have none after all the rest
/// whichever way the others read: an unprobed repo is not the cleanest in the
/// set and it is not the dirtiest either, so reversing the column has nothing
/// to say about where it goes.
fn sort_unknown_last<K: Ord>(
    rows: &mut [usize],
    descending: bool,
    key: impl Fn(usize) -> Option<K>,
) {
    rows.sort_by(|&a, &b| match (key(a), key(b)) {
        (Some(a), Some(b)) if descending => b.cmp(&a),
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{app, probed};
    use super::*;
    use crate::executor::StepResult;

    /// The repo names in the order the table would list them.
    fn shown(a: &App) -> Vec<&str> {
        a.visible_indices()
            .into_iter()
            .map(|i| a.repos[i].name.as_str())
            .collect()
    }

    fn sorted_by(a: &mut App, sort: Sort) -> Vec<&str> {
        a.choose_sort(sort);
        shown(a)
    }

    #[test]
    fn the_table_opens_in_name_order() {
        let a = app(&["bar", "baz", "foo"]);
        assert_eq!(a.sort, Sort::Repo);
        assert_eq!(a.sort_direction, Direction::Ascending);
        assert_eq!(shown(&a), vec!["bar", "baz", "foo"]);
    }

    #[test]
    fn choosing_the_active_column_again_flips_its_direction() {
        let mut a = app(&["bar", "baz", "foo"]);
        assert_eq!(sorted_by(&mut a, Sort::Repo), vec!["foo", "baz", "bar"]);
        assert_eq!(a.sort_direction, Direction::Descending);

        assert_eq!(sorted_by(&mut a, Sort::Repo), vec!["bar", "baz", "foo"]);
        assert_eq!(a.sort_direction, Direction::Ascending);
    }

    /// The direction belongs to the column, not to the table: carrying a
    /// reversal across would open a column backwards for no reason the user
    /// gave.
    #[test]
    fn another_column_opens_at_its_own_natural_direction() {
        let mut a = app(&["foo"]);
        a.choose_sort(Sort::Repo);
        assert_eq!(a.sort_direction, Direction::Descending, "repo reversed");

        a.choose_sort(Sort::State);
        assert_eq!(a.sort_direction, Sort::State.natural());
        assert_eq!(a.sort_direction, Direction::Descending);

        a.choose_sort(Sort::Branch);
        assert_eq!(a.sort_direction, Direction::Ascending);
    }

    #[test]
    fn branch_order_reverses_but_leaves_the_unprobed_rows_at_the_end() {
        let mut a = app(&["one", "three", "two"]);
        a.on_probe(0, probed(0, "main"));
        a.on_probe(0, probed(2, "develop"));

        assert_eq!(
            sorted_by(&mut a, Sort::Branch),
            vec!["two", "one", "three"],
            "develop, main, then the repo nothing has probed"
        );
        assert_eq!(
            sorted_by(&mut a, Sort::Branch),
            vec!["one", "two", "three"],
            "the branches reverse; the unprobed repo does not move"
        );
    }

    /// A timed-out probe is not a reading: every field beside the timeout is
    /// a default, so ordering by them would rank a repo mrx never got to.
    #[test]
    fn a_timed_out_probe_sorts_with_the_unprobed_rather_than_as_clean() {
        let mut a = app(&["clean", "timeout"]);
        a.on_probe(0, probed(0, "main"));
        let mut timed_out = probed(1, "main");
        timed_out.timed_out = true;
        a.on_probe(0, timed_out);

        assert_eq!(sorted_by(&mut a, Sort::State), vec!["clean", "timeout"]);
        assert_eq!(sorted_by(&mut a, Sort::State), vec!["clean", "timeout"]);
    }

    #[test]
    fn state_orders_by_what_a_repo_is_carrying() {
        let mut a = app(&["ahead", "clean", "dirty"]);
        let mut with_commits = probed(0, "main");
        with_commits.ahead = 3;
        a.on_probe(0, with_commits);
        a.on_probe(0, probed(1, "main"));
        let mut with_changes = probed(2, "main");
        with_changes.changed = 1;
        a.on_probe(0, with_changes);

        assert_eq!(
            sorted_by(&mut a, Sort::State),
            vec!["dirty", "ahead", "clean"],
            "uncommitted work outranks unpushed commits, which outrank nothing"
        );
        assert_eq!(
            sorted_by(&mut a, Sort::State),
            vec!["clean", "ahead", "dirty"]
        );
    }

    #[test]
    fn result_order_brings_the_failures_up() {
        let mut a = app(&["failed", "never", "passed", "skipped"]);
        let finished = |exit_code| RunStatus::Finished {
            steps: Vec::<StepResult>::new(),
            exit_code,
        };
        a.run_results[0] = Some(finished(1));
        a.run_results[2] = Some(finished(0));
        a.run_results[3] = Some(RunStatus::Skipped {
            reason: "no upstream".into(),
        });

        assert_eq!(
            sorted_by(&mut a, Sort::Result),
            vec!["failed", "passed", "skipped", "never"]
        );
        assert_eq!(
            sorted_by(&mut a, Sort::Result),
            vec!["never", "skipped", "passed", "failed"]
        );
    }

    /// Ties keep the order they arrived in, which is name order, so a column
    /// that cannot separate two rows never shuffles them between frames.
    #[test]
    fn rows_a_column_cannot_separate_stay_in_name_order_either_way() {
        let mut a = app(&["bar", "baz", "foo"]);
        for i in 0..3 {
            a.on_probe(0, probed(i, "main"));
        }
        assert_eq!(sorted_by(&mut a, Sort::Branch), vec!["bar", "baz", "foo"]);
        assert_eq!(sorted_by(&mut a, Sort::Branch), vec!["bar", "baz", "foo"]);
    }

    #[test]
    fn the_filter_and_the_order_apply_together() {
        let mut a = app(&["api-one", "api-two", "web"]);
        a.filter = "api".into();
        assert_eq!(sorted_by(&mut a, Sort::Repo), vec!["api-two", "api-one"]);
    }

    /// The header only names the order once it is not the one the table
    /// opens on, so the usual view keeps a header that says nothing extra.
    #[test]
    fn the_header_names_the_order_only_once_it_is_not_the_default() {
        let mut a = app(&["foo"]);
        assert!(!a.header_right_text().contains('↑'));

        a.choose_sort(Sort::State);
        assert!(a.header_right_text().contains("sort STATE ↓"));
    }

    #[test]
    fn only_the_sorted_column_carries_an_arrow() {
        let mut a = app(&["foo"]);
        a.choose_sort(Sort::State);
        assert_eq!(a.sort_arrow(Sort::State), Some("↓"));
        assert_eq!(a.sort_arrow(Sort::Repo), None);
    }

    #[test]
    fn every_column_has_its_own_menu_key_and_session_name() {
        for &sort in Sort::ALL {
            assert_eq!(Sort::from_key(sort.key()), Some(sort));
            assert_eq!(Sort::from_name(sort.name()), Some(sort));
        }
        let keys: std::collections::BTreeSet<char> = Sort::ALL.iter().map(|s| s.key()).collect();
        assert_eq!(keys.len(), Sort::ALL.len(), "two columns share a key");
    }
}
