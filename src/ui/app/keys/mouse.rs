//! Mouse dispatch: turning a pointer position into a repo row or a transcript
//! line, and the hint shown when a drag has nowhere to land.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::ui::app::detail;
use crate::ui::app::render;
use crate::ui::app::state::App;

/// How many rows/lines one wheel tick moves, versus the half-page jump
/// `Ctrl-D`/`Ctrl-U` use.
const WHEEL_STEP: isize = 3;

pub(super) fn on_mouse(app: &mut App, mouse: MouseEvent) -> bool {
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

/// The transcript line under a pointer in the output pane, or `None` when
/// the pointer is elsewhere or past the end of the output. Repeats the
/// geometry `draw_detail` lays out with: four rows of chrome above the
/// content, and the shared footer below it whichever layout is up.
fn output_line_at(app: &App, column: u16, row: u16) -> Option<usize> {
    if !app.detail_open {
        return None;
    }
    if !detail::pointer_over_output(
        app.terminal_width,
        render::sidebar_natural_width(app),
        column,
    ) {
        return None;
    }
    let content_row = (row as usize).checked_sub(render::LIST_HEADER_ROWS)?;
    // Same count draw_detail lands on for either layout: see
    // detail_content_height's doc for why passing `false` here still
    // matches a split pane.
    let content_height = render::detail_content_height(app.terminal_height, false);
    if content_row >= content_height {
        return None;
    }
    let lines = app.transcript_lines()?;
    let line = app.detail_view_scroll(lines.len(), content_height) + content_row;
    (line < lines.len()).then_some(line)
}

/// Click a row to move the cursor to it, click the row already under the
/// cursor to open its detail view. A click inside the detail pane itself,
/// or while a modal overlay is up, has no target (section 03: "no click
/// target without a key").
fn on_click(app: &mut App, column: u16, row: u16) {
    if app.pending_run.is_some() || app.palette_open || app.set_picker_open || app.quit_pending {
        return;
    }

    if app.detail_open {
        // A press in the output is the start of a text selection, not a
        // click on anything: the pane has no click targets of its own.
        if let Some(line) = output_line_at(app, column, row) {
            app.begin_output_selection(line);
            return;
        }
        let in_sidebar = !detail::pointer_over_output(
            app.terminal_width,
            render::sidebar_natural_width(app),
            column,
        );
        if in_sidebar {
            if let Some(repo) = resolve_row(app, row) {
                app.cursor = repo;
            }
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

/// The repo a click at on-screen `row` lands on, using the same header and
/// scroll math the table was just drawn with.
fn resolve_row(app: &App, row: u16) -> Option<usize> {
    let row = row as usize;
    if row < render::LIST_HEADER_ROWS {
        return None;
    }
    let body_row = row - render::LIST_HEADER_ROWS;
    let list_h = render::list_height(app, app.terminal_height);
    if body_row >= list_h {
        return None;
    }
    let visible = app.visible_indices();
    let scroll = render::list_start(app, &visible, list_h);
    app.repo_at_row(body_row, scroll)
}

/// Scroll whichever region the pointer is over: the list (moving the
/// cursor) or, once the detail view is open, the output under it.
fn on_scroll(app: &mut App, column: u16, dir: isize) {
    if app.detail_open {
        let over_detail = detail::pointer_over_output(
            app.terminal_width,
            render::sidebar_natural_width(app),
            column,
        );
        if over_detail {
            app.detail_scroll_by(dir * WHEEL_STEP);
            return;
        }
    }
    app.move_cursor(dir * WHEEL_STEP);
}

/// There's no drag support (section 03); the first one swallowed while the
/// mouse is captured tells you how to get native selection back instead.
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
    use crossterm::event::{Event, KeyCode, KeyModifiers};

    #[test]
    fn a_click_on_the_cursor_row_opens_the_detail_view() {
        let mut a = app(&["foo", "bar"]);
        a.terminal_height = 24;
        a.cursor = 0;
        let ev = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: render::LIST_HEADER_ROWS as u16, // first table row
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, ev);
        assert!(a.detail_open);
    }

    #[test]
    fn a_click_on_a_different_row_moves_the_cursor_without_opening_detail() {
        let mut a = app(&["foo", "bar"]);
        a.terminal_height = 24;
        a.cursor = 0;
        let ev = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: render::LIST_HEADER_ROWS as u16 + 1, // second table row
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, ev);
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
            on_input(&mut a, ev);
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
            row: render::LIST_HEADER_ROWS as u16,
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, ev);
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
        let top = render::LIST_HEADER_ROWS as u16;

        on_input(
            &mut a,
            mouse(MouseEventKind::Down(MouseButton::Left), column, top),
        );
        on_input(
            &mut a,
            mouse(MouseEventKind::Drag(MouseButton::Left), column, top + 2),
        );
        assert_eq!(a.output_selection_range(), Some(0..=2));

        // Dragging back above the anchor selects the same way.
        on_input(
            &mut a,
            mouse(MouseEventKind::Drag(MouseButton::Left), column, top),
        );
        assert_eq!(a.output_selection_range(), Some(0..=0));
    }

    #[test]
    fn a_click_in_the_output_pane_clears_the_last_selection_rather_than_making_one() {
        let (mut a, column) = app_with_output();
        let top = render::LIST_HEADER_ROWS as u16;
        on_input(
            &mut a,
            mouse(MouseEventKind::Down(MouseButton::Left), column, top),
        );
        on_input(
            &mut a,
            mouse(MouseEventKind::Up(MouseButton::Left), column, top),
        );
        assert!(a.output_selection_range().is_none());
        assert!(a.status_message.is_none(), "a click copies nothing");
    }

    #[test]
    fn a_selection_does_not_follow_the_cursor_onto_another_repo() {
        let (mut a, column) = app_with_output();
        let top = render::LIST_HEADER_ROWS as u16;
        on_input(
            &mut a,
            mouse(MouseEventKind::Down(MouseButton::Left), column, top),
        );
        on_input(
            &mut a,
            mouse(MouseEventKind::Drag(MouseButton::Left), column, top + 1),
        );
        assert!(a.output_selection_range().is_some());

        on_input(&mut a, press(KeyCode::Char('j')));
        assert!(
            a.output_selection_range().is_none(),
            "the indices only mean anything against the transcript they were taken on"
        );
    }

    #[test]
    fn a_drag_over_the_sidebar_is_still_swallowed_with_a_hint() {
        let (mut a, _) = app_with_output();
        on_input(
            &mut a,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                2,
                render::LIST_HEADER_ROWS as u16,
            ),
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
        on_input(&mut a, ev);
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
        on_input(&mut a, ev);
        assert!(a.status_message.is_some());
        assert!(a.drag_hint_shown);

        on_input(&mut a, press(KeyCode::Char(' '))); // any other key clears it
        let ev2 = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        on_input(&mut a, ev2);
        assert!(
            a.status_message.is_none(),
            "the hint is shown once, not every drag"
        );
    }

    /// A drag is never one event. The terminal sends a press, one motion
    /// per cell crossed, and a release, so the hint is only ever on screen
    /// if it survives everything the gesture sends after the motion that
    /// set it.
    #[test]
    fn the_drag_hint_survives_the_rest_of_the_gesture() {
        let mut a = app(&["foo", "bar"]);
        on_input(&mut a, mouse(MouseEventKind::Down(MouseButton::Left), 4, 6));
        on_input(&mut a, mouse(MouseEventKind::Drag(MouseButton::Left), 5, 6));
        on_input(&mut a, mouse(MouseEventKind::Drag(MouseButton::Left), 6, 6));
        on_input(&mut a, mouse(MouseEventKind::Up(MouseButton::Left), 6, 6));
        assert!(
            a.status_message.is_some(),
            "the release wiped the hint before it could be painted"
        );
    }

    #[test]
    fn pointer_motion_after_a_swallowed_drag_leaves_the_hint_up() {
        let mut a = app(&["foo", "bar"]);
        on_input(&mut a, mouse(MouseEventKind::Drag(MouseButton::Left), 5, 6));
        assert!(a.status_message.is_some());

        on_input(&mut a, mouse(MouseEventKind::Moved, 7, 8));
        on_input(&mut a, mouse(MouseEventKind::Moved, 9, 8));
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
        on_input(&mut a, mouse(MouseEventKind::Drag(MouseButton::Left), 5, 6));
        assert!(a.status_message.is_some());
        assert!(a.drag_hint_shown);

        a.status_message = None;
        on_input(&mut a, mouse(MouseEventKind::Drag(MouseButton::Left), 6, 6));
        assert!(a.status_message.is_none());
    }
}
