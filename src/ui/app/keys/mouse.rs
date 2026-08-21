//! Mouse dispatch: turning a pointer position into a repo row or a transcript
//! line, and the hint shown when a drag has nowhere to land.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::ui::app::render::{self, Panes};
use crate::ui::app::state::{App, Pane};

/// Rows, or transcript lines, that one wheel tick moves.
const WHEEL_STEP: isize = 3;

pub(super) fn on_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    // A modal draws a `Clear`ed popup over the table, so every gesture under
    // it would act on something the user cannot see: a click resolving to a
    // row, the wheel moving a cursor behind the popup. Guarding here rather
    // than in each handler is what stops the next gesture arriving unguarded.
    if app.mode().is_modal() {
        return false;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => on_click(app, mouse.column, mouse.row),
        MouseEventKind::ScrollUp => on_scroll(app, mouse.column, -1),
        MouseEventKind::ScrollDown => on_scroll(app, mouse.column, 1),
        MouseEventKind::Drag(MouseButton::Left) => on_drag(app, mouse.column, mouse.row),
        MouseEventKind::Up(MouseButton::Left) => app.finish_output_selection(),
        MouseEventKind::Drag(_) => on_drag_swallowed(app),
        _ => {}
    }
    false
}

/// A drag over the output pane selects transcript lines; anywhere else it
/// is still swallowed with a hint.
fn on_drag(app: &mut App, column: u16, row: u16) {
    match output_line_at(app, column, row) {
        Some(line) => app.extend_output_selection(line),
        None => on_drag_swallowed(app),
    }
}

/// The transcript line under a pointer in the output pane, or `None` when the
/// pointer is elsewhere or past the end of the output.
fn output_line_at(app: &App, column: u16, row: u16) -> Option<usize> {
    let panes = Panes::last_known(app);
    if !panes.over_detail(column) {
        return None;
    }
    let content_row = panes.detail_body_row(row)?;
    let lines = app.transcript_lines()?;
    let line = app.detail_view_scroll(lines.len(), panes.detail_rows) + content_row;
    (line < lines.len()).then_some(line)
}

/// Click a row to move the cursor to it, click the row already under the
/// cursor to open its detail view. A click inside the detail pane itself has
/// no target.
fn on_click(app: &mut App, column: u16, row: u16) {
    if app.detail_open {
        // Both panes are on screen, so a press on one is the clearest
        // statement there is of which one `j`/`k` should reach: pointing at
        // a pane hands it the keys, without a `tab` first.
        let over_output = Panes::last_known(app).over_detail(column);
        app.focus = if over_output {
            Pane::Output
        } else {
            Pane::List
        };
        if over_output {
            // A press in the output starts a text selection: the pane has
            // no click targets of its own.
            if let Some(line) = output_line_at(app, column, row) {
                app.begin_output_selection(line);
            }
        } else if let Some(repo) = resolve_row(app, row) {
            app.cursor = repo;
        }
        return;
    }

    if let Some(repo) = resolve_row(app, row) {
        if repo == app.cursor {
            app.open_detail();
        } else {
            app.cursor = repo;
        }
    }
}

/// The repo a click at on-screen `row` lands on, off the same rects and row
/// counts the table was drawn from.
fn resolve_row(app: &App, row: u16) -> Option<usize> {
    let panes = Panes::last_known(app);
    let body_row = panes.list_body_row(row)?;
    let visible = app.visible_indices();
    let scroll = render::list_start(app, &visible, panes.list_rows);
    app.repo_at_row(body_row, scroll)
}

/// Scroll whichever region the pointer is over: the list (moving the
/// cursor) or, once the detail view is open, the output under it.
fn on_scroll(app: &mut App, column: u16, dir: isize) {
    if Panes::last_known(app).over_detail(column) {
        app.detail_scroll_by(dir * WHEEL_STEP);
        return;
    }
    app.move_cursor(dir * WHEEL_STEP);
}

/// The first drag with nowhere to land tells you, once, how to get the
/// terminal's own selection back from mouse capture.
fn on_drag_swallowed(app: &mut App) {
    if !app.drag_hint_shown {
        app.drag_hint_shown = true;
        app.status_message = Some(
            "drag ignored while the mouse is captured: hold ⌥/⇧ to select text, or press m to release it"
                .into(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::keys::on_input;
    use crate::ui::app::keys::testkit::{app, press};
    use crate::ui::app::state::Mode;
    use crossterm::event::{Event, KeyCode, KeyModifiers};

    /// [`render::LIST_HEADER_ROWS`] as a screen row: the first table row, and
    /// the output pane's first content row.
    fn header_row() -> u16 {
        u16::try_from(render::LIST_HEADER_ROWS).unwrap_or(u16::MAX)
    }

    #[test]
    fn a_click_on_the_cursor_row_opens_the_detail_view() {
        let mut a = app(&["foo", "bar"]);
        a.terminal_height = 24;
        a.cursor = 0;
        let ev = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: header_row(), // first table row
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, &ev);
        assert!(a.detail_open);
    }

    /// A modal covers the table, so no gesture under one may reach the row it
    /// happens to be drawn over. Swept over every mode rather than the four
    /// this test used to list, which is how it missed the help overlay.
    #[test]
    fn no_pointer_gesture_reaches_the_table_behind_a_modal() {
        for &mode in Mode::ALL.iter().filter(|m| m.is_modal()) {
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollUp,
            ] {
                let mut a = app(&["foo", "bar"]);
                a.terminal_height = 24;
                a.cursor = 0;
                a.enter_mode(mode);
                let ev = Event::Mouse(MouseEvent {
                    kind,
                    column: 5,
                    row: header_row(),
                    modifiers: KeyModifiers::NONE,
                });
                on_input(&mut a, &ev);
                assert!(!a.detail_open, "{mode:?} let {kind:?} open the detail view");
                assert_eq!(a.cursor, 0, "{mode:?} let {kind:?} move the cursor");
            }
        }
    }

    #[test]
    fn a_click_on_a_different_row_moves_the_cursor_without_opening_detail() {
        let mut a = app(&["foo", "bar"]);
        a.terminal_height = 24;
        a.cursor = 0;
        let ev = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: header_row() + 1, // second table row
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, &ev);
        assert_eq!(a.cursor, 1);
        assert!(!a.detail_open);
    }

    /// Pinned to literal rows rather than to `LIST_HEADER_ROWS`, so that
    /// adding or removing a chrome row without adjusting click resolution
    /// fails here instead of silently shifting every click by one.
    #[test]
    fn clicking_the_title_label_or_rule_rows_never_reaches_a_repo() {
        for row in 0..3u16 {
            let mut a = app(&["foo", "bar", "baz"]);
            a.terminal_height = 24;
            a.cursor = 2;
            let ev = Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row,
                modifiers: KeyModifiers::NONE,
            });
            on_input(&mut a, &ev);
            assert_eq!(a.cursor, 2, "row {row} moved the cursor");
            assert!(!a.detail_open, "row {row} opened the detail view");
        }
    }

    #[test]
    fn the_first_row_below_the_chrome_is_the_first_repo() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.terminal_height = 24;
        a.cursor = 2;
        let ev = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: header_row(),
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, &ev);
        assert_eq!(a.cursor, 0);
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// An app showing a finished three-line transcript in the split, with
    /// the output pane's first content row at `LIST_HEADER_ROWS`.
    fn app_with_output() -> (App, u16) {
        let mut a = app(&["foo", "bar"]);
        a.terminal_width = 120;
        a.terminal_height = 24;
        a.run_results[0] = Some(crate::ui::app::state::RunStatus::Finished {
            steps: vec![crate::executor::StepResult {
                label: "git pull".into(),
                shape: crate::summarize::Shape::Generic,
                stdout: "one\ntwo\nthree\n".into(),
                stderr: String::new(),
                code: 0,
            }],
            exit_code: 0,
        });
        a.open_detail();
        let column = crate::ui::app::detail::sidebar_width(
            a.terminal_width,
            render::sidebar_natural_width(&a),
        ) + 2;
        (a, column)
    }

    /// Mouse capture takes the terminal's own drag-to-select away, so the
    /// output pane has to offer one of its own.
    #[test]
    fn dragging_across_the_output_pane_selects_the_lines_it_covers() {
        let (mut a, column) = app_with_output();
        let top = header_row();

        on_input(
            &mut a,
            &mouse(MouseEventKind::Down(MouseButton::Left), column, top),
        );
        on_input(
            &mut a,
            &mouse(MouseEventKind::Drag(MouseButton::Left), column, top + 2),
        );
        assert_eq!(a.output_selection_range(), Some(0..=2));

        // Dragging back above the anchor selects the same way.
        on_input(
            &mut a,
            &mouse(MouseEventKind::Drag(MouseButton::Left), column, top),
        );
        assert_eq!(a.output_selection_range(), Some(0..=0));
    }

    #[test]
    fn a_click_in_the_output_pane_clears_the_last_selection_rather_than_making_one() {
        let (mut a, column) = app_with_output();
        let top = header_row();
        on_input(
            &mut a,
            &mouse(MouseEventKind::Down(MouseButton::Left), column, top),
        );
        on_input(
            &mut a,
            &mouse(MouseEventKind::Up(MouseButton::Left), column, top),
        );
        assert!(a.output_selection_range().is_none());
        assert!(a.status_message.is_none(), "a click copies nothing");
    }

    #[test]
    fn a_selection_does_not_follow_the_cursor_onto_another_repo() {
        let (mut a, column) = app_with_output();
        let top = header_row();
        on_input(
            &mut a,
            &mouse(MouseEventKind::Down(MouseButton::Left), column, top),
        );
        on_input(
            &mut a,
            &mouse(MouseEventKind::Drag(MouseButton::Left), column, top + 1),
        );
        assert!(a.output_selection_range().is_some());

        // Tab first: the press that started the drag gave the output the keys.
        on_input(&mut a, &press(KeyCode::Tab));
        on_input(&mut a, &press(KeyCode::Char('j')));
        assert!(
            a.output_selection_range().is_none(),
            "the indices only mean anything against the transcript they were taken on"
        );
    }

    /// Both panes are on screen at once, so pointing at one has to be enough
    /// to make `j` mean what it looks like it means.
    #[test]
    fn clicking_a_pane_hands_it_the_keys() {
        let (mut a, column) = app_with_output();
        let top = header_row();
        assert_eq!(
            a.focus,
            Pane::List,
            "the view opens on the row it was asked about"
        );

        on_input(
            &mut a,
            &mouse(MouseEventKind::Down(MouseButton::Left), column, top),
        );
        assert_eq!(a.focus, Pane::Output);

        on_input(
            &mut a,
            &mouse(MouseEventKind::Down(MouseButton::Left), 2, top),
        );
        assert_eq!(a.focus, Pane::List);
    }

    /// A transcript rarely fills the pane, and the blank under it is still
    /// the pane.
    #[test]
    fn clicking_past_the_end_of_the_output_still_focuses_it() {
        let (mut a, column) = app_with_output();
        let below = header_row() + 8;
        on_input(
            &mut a,
            &mouse(MouseEventKind::Down(MouseButton::Left), column, below),
        );
        assert_eq!(a.focus, Pane::Output);
        assert!(
            a.output_selection_range().is_none(),
            "there is no line down there to select"
        );
    }

    #[test]
    fn a_drag_over_the_sidebar_is_still_swallowed_with_a_hint() {
        let (mut a, _) = app_with_output();
        on_input(
            &mut a,
            &mouse(MouseEventKind::Drag(MouseButton::Left), 2, header_row()),
        );
        assert!(a.output_selection_range().is_none());
        assert!(a.status_message.is_some());
    }

    #[test]
    fn wheel_scroll_moves_the_cursor_when_the_detail_view_is_closed() {
        let mut a = app(&["foo", "bar", "baz"]);
        let ev = Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, &ev);
        assert!(a.cursor > 0);
    }

    #[test]
    fn a_swallowed_drag_sets_the_hint_once() {
        let mut a = app(&["foo"]);
        let ev = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, &ev);
        assert!(a.status_message.is_some());
        assert!(a.drag_hint_shown);

        on_input(&mut a, &press(KeyCode::Char(' '))); // any other key clears it
        let ev2 = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, &ev2);
        assert!(
            a.status_message.is_none(),
            "the hint is shown once, not every drag"
        );
    }

    /// A drag is never one event, so the hint only reaches the screen if it
    /// survives the rest of the gesture.
    #[test]
    fn the_drag_hint_survives_the_rest_of_the_gesture() {
        let mut a = app(&["foo", "bar"]);
        on_input(
            &mut a,
            &mouse(MouseEventKind::Down(MouseButton::Left), 4, 6),
        );
        on_input(
            &mut a,
            &mouse(MouseEventKind::Drag(MouseButton::Left), 5, 6),
        );
        on_input(
            &mut a,
            &mouse(MouseEventKind::Drag(MouseButton::Left), 6, 6),
        );
        on_input(&mut a, &mouse(MouseEventKind::Up(MouseButton::Left), 6, 6));
        assert!(
            a.status_message.is_some(),
            "the release wiped the hint before it could be painted"
        );
    }

    #[test]
    fn pointer_motion_after_a_swallowed_drag_leaves_the_hint_up() {
        let mut a = app(&["foo", "bar"]);
        on_input(
            &mut a,
            &mouse(MouseEventKind::Drag(MouseButton::Left), 5, 6),
        );
        assert!(a.status_message.is_some());

        on_input(&mut a, &mouse(MouseEventKind::Moved, 7, 8));
        on_input(&mut a, &mouse(MouseEventKind::Moved, 9, 8));
        assert!(
            a.status_message.is_some(),
            "passive pointer motion is not a user action and must not clear the status line"
        );
    }

    /// Clears the message by hand between the two drags, so the latch is
    /// the only thing that can be keeping the second one quiet.
    #[test]
    fn a_second_swallowed_drag_does_not_set_the_hint_again() {
        let mut a = app(&["foo", "bar"]);
        on_input(
            &mut a,
            &mouse(MouseEventKind::Drag(MouseButton::Left), 5, 6),
        );
        assert!(a.status_message.is_some());
        assert!(a.drag_hint_shown);

        a.status_message = None;
        on_input(
            &mut a,
            &mouse(MouseEventKind::Drag(MouseButton::Left), 6, 6),
        );
        assert!(a.status_message.is_none());
    }
}
