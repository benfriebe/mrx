//! The detail pane: its scroll model, which half has the keys, and the text
//! selection dragged in the output.

use super::{App, RunStatus};
use crate::ansi;
use crate::ui::app::{detail, render};
use std::ops::RangeInclusive;

/// A drag-selection in the output pane, as indices into the transcript it
/// was taken on. `head` may be above `anchor`: a drag upward selects the
/// same range a drag downward would.
pub struct OutputSelection {
    pub repo: usize,
    pub anchor: usize,
    pub head: usize,
}

/// Which pane of the detail split has the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The repo list. `j`/`k` move the cursor and the output follows.
    List,
    /// The output. `j`/`k` scroll it and the cursor stays put.
    Output,
}

impl Pane {
    fn other(self) -> Self {
        match self {
            Pane::List => Pane::Output,
            Pane::Output => Pane::List,
        }
    }
}

impl App {
    /// Open the detail view for the cursor row. A no-op with a status
    /// message when the filter hides every row: the cursor can still index
    /// a repo the table isn't showing.
    pub fn open_detail(&mut self) {
        if self.visible_indices().is_empty() {
            self.status_message = Some(self.no_visible_rows_message());
            return;
        }
        self.detail_open = true;
        // Opening from a row means "show me this one", so `j`/`k` keep
        // walking rows with the output following.
        self.focus = Pane::List;
    }

    /// Back to the full-width list.
    pub fn close_detail(&mut self) {
        self.detail_open = false;
        self.output_selection = None;
    }

    /// The cursor row's output, finished or still arriving, or `None` when
    /// there is nothing to lay out yet. A live run is preferred over a stale
    /// finished one: the row is being written to right now, and the previous
    /// answer is no longer the one being asked about.
    pub fn transcript_lines(&self) -> Option<Vec<detail::DetailLine>> {
        if let Some(live) = self.live.get(&self.cursor).filter(|l| !l.steps.is_empty()) {
            return Some(detail::live_lines(&live.steps));
        }
        match self.run_results.get(self.cursor)? {
            Some(RunStatus::Finished { steps, .. }) => Some(detail::detail_lines(steps)),
            _ => None,
        }
    }

    /// The transcript line drawn on the output pane's first content row. A
    /// run still arriving follows its own tail until a scroll says
    /// otherwise; any scroll leaves an entry behind and pins it.
    pub fn detail_view_scroll(&self, total_lines: usize, content_height: usize) -> usize {
        match self.detail_scroll.get(&self.cursor) {
            Some(&scroll) => detail::clamp_scroll(scroll, total_lines, content_height),
            None => total_lines.saturating_sub(content_height),
        }
    }

    /// Start a drag-selection in the output pane at transcript line `line`.
    pub fn begin_output_selection(&mut self, line: usize) {
        self.output_selection = Some(OutputSelection {
            repo: self.cursor,
            anchor: line,
            head: line,
        });
    }

    /// Extend a drag-selection to `line`, which may be above the anchor.
    pub fn extend_output_selection(&mut self, line: usize) {
        if let Some(selection) = self.output_selection.as_mut() {
            selection.head = line;
        }
    }

    /// The lines a drag left selected, or `None` when the drag belongs to a
    /// repo the cursor has since moved off: the indices only mean anything
    /// against the transcript they were taken on.
    pub fn output_selection_range(&self) -> Option<RangeInclusive<usize>> {
        let selection = self.output_selection.as_ref()?;
        (selection.repo == self.cursor)
            .then(|| selection.anchor.min(selection.head)..=selection.anchor.max(selection.head))
    }

    /// Copy what the drag selected, on the button coming back up. While
    /// mouse capture is on the terminal hands drags to mrx instead of
    /// selecting text with them, so the app owes the user a selection of its
    /// own.
    ///
    /// A press that never moved is a click, and a click clears the last
    /// selection rather than making a one-line one out of nothing.
    pub fn finish_output_selection(&mut self) {
        let Some(selection) = self.output_selection.as_ref() else {
            return;
        };
        if selection.anchor == selection.head {
            self.output_selection = None;
            return;
        }
        let Some(range) = self.output_selection_range() else {
            return;
        };
        let Some(lines) = self.transcript_lines() else {
            return;
        };
        let text = lines
            .get(range)
            .map(|lines| {
                lines
                    .iter()
                    .map(detail::DetailLine::text)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let repo_name = self
            .repos
            .get(self.cursor)
            .map_or("repo", |r| r.name.as_str());
        self.status_message = Some(detail::copy_or_save(&text, repo_name, "selection"));
    }

    /// `tab` in the detail view: hand the keys to the other pane.
    pub fn toggle_focus(&mut self) {
        self.focus = self.focus.other();
    }

    /// Enter on a row whose output is already on screen: the row is not the
    /// question any more, so the keys go to the pane holding the answer.
    pub fn focus_output(&mut self) {
        self.focus = Pane::Output;
    }

    /// Half a screen page for `Ctrl-D`/`Ctrl-U`, floored at one line so a
    /// very short terminal still scrolls. Approximate: it reads the last
    /// known frame height rather than the exact viewport, which is close
    /// enough for a "half page" key.
    ///
    /// The row count comes from [`detail_content_height`](render::detail_content_height)
    /// rather than being worked out again here, so the key cannot keep
    /// scrolling by a chrome height the panes no longer have. The list and the
    /// detail pane draw the same number of content rows for a given terminal
    /// height, so the one helper serves both callers.
    pub(super) fn half_page(&self) -> usize {
        (render::detail_content_height(self.terminal_height, false) / 2).max(1)
    }

    /// Move the cursor row's detail scroll by `delta` lines, bounded at both
    /// ends of the transcript. The bound has to be applied here and not only
    /// where the pane is drawn: an offset stored past the end is a distance
    /// the reader then has to scroll back through before anything moves.
    ///
    /// The first scroll of a row measures from what is on screen, not from
    /// line 0: an unscrolled transcript is showing its tail, so measuring
    /// from 0 would jump to the top of a 4000-line log instead of a half
    /// page up from where the reader is.
    pub fn detail_scroll_by(&mut self, delta: isize) {
        let (total, height) = self.detail_extent();
        let from = match self.detail_scroll.get(&self.cursor) {
            Some(&scroll) => scroll,
            None => self.detail_view_scroll(total, height),
        };
        let moved = (from.cast_signed() + delta).max(0).cast_unsigned();
        self.detail_scroll
            .insert(self.cursor, detail::clamp_scroll(moved, total, height));
    }

    /// The cursor row's transcript length and the height the output pane
    /// draws it in, so a scroll and the frame it lands on cannot disagree
    /// about where the end is. Resolved against the last known terminal
    /// height rather than the exact viewport, the same approximation
    /// [`half_page`](Self::half_page) makes.
    fn detail_extent(&self) -> (usize, usize) {
        let total = self.transcript_lines().map_or(0, |lines| lines.len());
        let height = render::detail_content_height(self.terminal_height, false);
        (total, height)
    }

    pub fn detail_scroll_down(&mut self) {
        let step = self.half_page().cast_signed();
        self.detail_scroll_by(step);
    }

    pub fn detail_scroll_up(&mut self) {
        let step = -self.half_page().cast_signed();
        self.detail_scroll_by(step);
    }

    /// Copy the step currently visible in the cursor row's detail view,
    /// falling back to a file when there's no clipboard.
    pub fn copy_visible_step(&mut self) {
        let Some(Some(RunStatus::Finished { steps, .. })) = self.run_results.get(self.cursor)
        else {
            self.status_message = Some("nothing to copy yet".into());
            return;
        };
        let lines = detail::detail_lines(steps);
        let scroll = self.detail_scroll.get(&self.cursor).copied().unwrap_or(0);
        let idx = detail::step_at_line(&lines, scroll);
        let Some(step) = steps.get(idx) else {
            return;
        };
        // Reads the raw StepResult rather than DetailLine, so it has to strip
        // ANSI escapes itself.
        let text = format!(
            "{}\n{}",
            ansi::strip(&step.stdout),
            ansi::strip(&step.stderr)
        );
        let repo_name = self
            .repos
            .get(self.cursor)
            .map_or("repo", |r| r.name.as_str());
        self.status_message = Some(detail::copy_or_save(&text, repo_name, &step.label));
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::app;
    use super::*;
    use crate::executor::{StepResult, TaskEvent};
    use crate::summarize;

    /// `half_page` used to subtract its own chrome height, a literal 6 that
    /// happened to equal `LIST_HEADER_ROWS + FOOTER_ROWS`. Nothing tied the
    /// two together, so changing either constant would have moved the pane
    /// without moving the key that scrolls it.
    #[test]
    fn a_half_page_is_half_the_rows_the_pane_actually_draws() {
        let mut a = app(&["foo"]);
        for height in 0..80u16 {
            a.terminal_height = height;
            let drawn = render::detail_content_height(height, false);
            assert_eq!(
                a.half_page(),
                (drawn / 2).max(1),
                "terminal height {height} draws {drawn} rows"
            );
        }
    }

    #[test]
    fn opening_the_detail_view_on_a_zero_match_filter_is_a_no_op() {
        let mut a = app(&["foo"]);
        a.filter = "zzz".into();
        a.open_detail();
        assert!(!a.detail_open);
        assert!(a.status_message.is_some());
    }

    #[test]
    fn tab_hands_the_keys_to_the_other_pane_and_back() {
        let mut a = app(&["foo"]);
        a.open_detail();
        assert_eq!(a.focus, Pane::List, "opening a row is about that row");
        a.toggle_focus();
        assert_eq!(a.focus, Pane::Output);
        a.toggle_focus();
        assert_eq!(a.focus, Pane::List);
    }

    #[test]
    fn reopening_the_detail_view_starts_on_the_list_again() {
        let mut a = app(&["foo"]);
        a.open_detail();
        a.toggle_focus();
        a.close_detail();
        a.open_detail();
        assert_eq!(a.focus, Pane::List);
    }

    #[test]
    fn detail_scroll_is_kept_per_repo() {
        let mut a = app(&["foo", "bar"]);
        a.terminal_height = 30;
        ran_with_long_output(&mut a, 400);
        a.cursor = 0;
        a.detail_scroll_up(); // the view opens at the tail, so up is the way off it
        a.cursor = 1;
        assert_eq!(
            a.detail_scroll.get(&1).copied().unwrap_or(0),
            0,
            "a different repo starts unscrolled"
        );
        a.cursor = 0;
        assert!(a.detail_scroll[&0] > 0);
    }

    #[test]
    fn detail_scroll_up_does_not_go_negative() {
        let mut a = app(&["foo"]);
        a.detail_scroll_up();
        assert_eq!(a.detail_scroll[&0], 0);
    }

    /// A finished run whose output is far longer than any viewport, so the
    /// tail the detail view opens at is nowhere near line 0.
    fn ran_with_long_output(a: &mut App, lines: usize) {
        let run_id = a.begin_named_run("update".into(), vec![0]);
        a.on_task(
            run_id,
            TaskEvent::Finished {
                index: 0,
                steps: vec![StepResult {
                    label: "update".into(),
                    shape: summarize::Shape::Generic,
                    // A fixture can afford a `format!` per line, and the
                    // `fold` the lint asks for instead reads worse.
                    #[expect(clippy::format_collect)]
                    stdout: (1..=lines).map(|i| format!("line {i}\n")).collect(),
                    stderr: String::new(),
                    code: 0,
                }],
                exit_code: 0,
            },
        );
    }

    #[test]
    fn the_first_scroll_of_a_long_transcript_moves_from_the_tail_not_from_the_top() {
        let mut a = app(&["foo"]);
        a.terminal_height = 30;
        ran_with_long_output(&mut a, 400);
        a.detail_open = true;

        let total = a.transcript_lines().unwrap().len();
        let height = render::detail_content_height(a.terminal_height, false);
        let tail = a.detail_view_scroll(total, height);
        assert!(tail > a.half_page(), "the tail must be a long way down");

        a.detail_scroll_up();

        assert_eq!(
            a.detail_view_scroll(total, height),
            tail - a.half_page(),
            "one half page up from the tail, not from line 0"
        );
    }

    /// Pressing past the bottom used to bank the overshoot, so the same
    /// number of presses back up bought no movement at all.
    #[test]
    fn scrolling_past_the_end_stores_the_end_rather_than_a_distance_beyond_it() {
        let mut a = app(&["foo"]);
        a.terminal_height = 30;
        ran_with_long_output(&mut a, 400);
        a.detail_open = true;

        let total = a.transcript_lines().unwrap().len();
        let height = render::detail_content_height(a.terminal_height, false);
        let tail = a.detail_view_scroll(total, height);

        for _ in 0..5 {
            a.detail_scroll_down();
        }
        assert_eq!(a.detail_scroll[&0], tail, "the bottom is the bottom");

        a.detail_scroll_up();
        assert_eq!(
            a.detail_view_scroll(total, height),
            tail - a.half_page(),
            "one press off the bottom moves one half page"
        );
    }

    #[test]
    fn copying_before_anything_has_run_reports_nothing_to_copy() {
        let mut a = app(&["foo"]);
        a.copy_visible_step();
        assert_eq!(a.status_message.as_deref(), Some("nothing to copy yet"));
    }
}
