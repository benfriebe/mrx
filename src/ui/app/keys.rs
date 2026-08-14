//! Keymap and mouse dispatch for the resident app. Input arrives as a
//! `crossterm::Event` rather than a `KeyEvent`, so mouse and resize land on
//! this same entry point; keys.rs owns both so a click and Enter can't
//! disagree about what opening a row means.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use super::render;
use super::state::App;

/// How many rows/lines one wheel tick moves, versus the half-page jump
/// `Ctrl-D`/`Ctrl-U` use.
const WHEEL_STEP: isize = 3;

/// Dispatch one input event. Returns true when the app should quit.
pub fn on_input(app: &mut App, event: Event) -> bool {
    app.status_message = None;
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => on_key(app, key),
        Event::Mouse(mouse) => on_mouse(app, mouse),
        _ => false,
    }
}

fn on_key(app: &mut App, key: KeyEvent) -> bool {
    if app.pending_run.is_some() {
        return on_confirm_key(app, key);
    }
    if app.palette_open {
        on_palette_key(app, key);
        return false;
    }
    if app.filtering {
        on_filter_key(app, key);
        return false;
    }
    if app.detail_open {
        return on_detail_key(app, key);
    }
    on_list_key(app, key)
}

fn on_list_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return true;
    }

    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('j') | KeyCode::Down => app.move_cursor(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_cursor(-1),
        KeyCode::Char('g') => app.move_to_first(),
        KeyCode::Char('G') => app.move_to_last(),
        KeyCode::Char(' ') => app.toggle_selection_at_cursor(),
        KeyCode::Char('a') => app.select_all_visible(),
        KeyCode::Char('A') => app.clear_selection(),
        KeyCode::Char('i') => app.invert_selection(),
        KeyCode::Char('/') => app.start_filter(),
        KeyCode::Char('r') => app.probe_requested = true,
        KeyCode::Char('u') => app.request_run("update"),
        KeyCode::Char('s') => app.request_run("status"),
        KeyCode::Char('f') => app.request_run("fetch"),
        KeyCode::Char('d') => app.request_run("diff"),
        KeyCode::Char(':') => app.open_palette(),
        KeyCode::Char('m') => app.toggle_mouse_capture(),
        KeyCode::Enter => app.open_detail(),
        _ => {}
    }
    false
}

/// Keys while `/` is capturing text. Everything but Esc, Enter, and
/// Backspace is literal filter text, including letters that are shortcuts in
/// the normal mode.
fn on_filter_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_filter(),
        KeyCode::Enter => app.commit_filter(),
        KeyCode::Backspace => app.filter_backspace(),
        KeyCode::Char(c) => app.filter_push(c),
        _ => {}
    }
}

/// Keys while the action palette is open. Same shape as filter capture:
/// only navigation and the exits are special, everything else is text.
fn on_palette_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.close_palette(),
        KeyCode::Enter => app.palette_confirm(),
        KeyCode::Backspace => app.palette_backspace(),
        KeyCode::Up => app.palette_move(-1),
        KeyCode::Down => app.palette_move(1),
        KeyCode::Char(c) => app.palette_push(c),
        _ => {}
    }
}

/// Keys while the dirty-selection confirmation is up (section 11): a modal
/// that swallows everything but yes/no.
fn on_confirm_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => app.confirm_pending_run(),
        KeyCode::Char('n') | KeyCode::Esc => app.cancel_pending_run(),
        _ => {}
    }
    false
}

/// Keys while the detail view is open: the cursor still moves the
/// underlying selection (the view follows it), plus scrolling, copying, and
/// the exit back to the full-width list.
fn on_detail_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return true,
            KeyCode::Char('d') => {
                app.detail_scroll_down();
                return false;
            }
            KeyCode::Char('u') => {
                app.detail_scroll_up();
                return false;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('j') | KeyCode::Down => app.move_cursor(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_cursor(-1),
        KeyCode::Esc => app.close_detail(),
        KeyCode::Char('y') => app.copy_visible_step(),
        KeyCode::Char('m') => app.toggle_mouse_capture(),
        _ => {}
    }
    false
}

fn on_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => on_click(app, mouse.column, mouse.row),
        MouseEventKind::ScrollUp => on_scroll(app, mouse.column, -1),
        MouseEventKind::ScrollDown => on_scroll(app, mouse.column, 1),
        MouseEventKind::Drag(_) => on_drag_swallowed(app),
        _ => {}
    }
    false
}

/// Click a row to move the cursor to it, click the row already under the
/// cursor to open its detail view. A click inside the detail pane itself,
/// or while a modal overlay is up, has no target (section 03: "no click
/// target without a key").
fn on_click(app: &mut App, column: u16, row: u16) {
    if app.pending_run.is_some() || app.palette_open {
        return;
    }

    if app.detail_open {
        let in_sidebar = matches!(
            super::detail::layout_for_width(app.terminal_width),
            super::detail::DetailLayout::Split
        ) && column < super::detail::sidebar_width(app.terminal_width);
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
    let cursor_pos = visible.iter().position(|&i| i == app.cursor).unwrap_or(0);
    let scroll = render::scroll_offset(cursor_pos, visible.len(), list_h);
    app.repo_at_row(body_row, scroll)
}

/// Scroll whichever region the pointer is over: the list (moving the
/// cursor) or, once the detail view is open, the output under it.
fn on_scroll(app: &mut App, column: u16, dir: isize) {
    if app.detail_open {
        let over_detail = match super::detail::layout_for_width(app.terminal_width) {
            super::detail::DetailLayout::FullScreen => true,
            super::detail::DetailLayout::Split => {
                column >= super::detail::sidebar_width(app.terminal_width)
            }
        };
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
    use crate::config::Repo;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn repo(name: &str) -> Repo {
        Repo {
            name: name.to_string(),
            path: PathBuf::from(format!("/nonexistent/{}", name)),
            clone_url: None,
            keys: Default::default(),
        }
    }

    fn app(names: &[&str]) -> App {
        App::new(
            names.iter().map(|n| repo(n)).collect(),
            "work".into(),
            4,
            BTreeMap::new(),
            PathBuf::from("/dev/null"),
            false,
        )
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::CONTROL))
    }

    #[test]
    fn q_quits_in_list_mode() {
        let mut a = app(&["foo"]);
        assert!(on_input(&mut a, press(KeyCode::Char('q'))));
    }

    #[test]
    fn q_is_literal_text_while_filtering() {
        let mut a = app(&["foo"]);
        a.start_filter();
        assert!(!on_input(&mut a, press(KeyCode::Char('q'))));
        assert_eq!(a.filter, "q");
    }

    #[test]
    fn slash_enters_filter_mode_and_typed_chars_narrow_it() {
        let mut a = app(&["foo", "bar"]);
        on_input(&mut a, press(KeyCode::Char('/')));
        assert!(a.filtering);
        on_input(&mut a, press(KeyCode::Char('b')));
        on_input(&mut a, press(KeyCode::Char('a')));
        assert_eq!(a.visible_indices(), vec![1]);
        on_input(&mut a, press(KeyCode::Enter));
        assert!(!a.filtering);
        assert_eq!(a.filter, "ba");
    }

    #[test]
    fn esc_clears_the_filter() {
        let mut a = app(&["foo", "bar"]);
        a.start_filter();
        a.filter_push('b');
        on_input(&mut a, press(KeyCode::Esc));
        assert!(a.filter.is_empty());
        assert!(!a.filtering);
    }

    #[test]
    fn space_selects_and_advances() {
        let mut a = app(&["foo", "bar"]);
        on_input(&mut a, press(KeyCode::Char(' ')));
        assert!(a.selected.contains(&0));
        assert_eq!(a.cursor, 1);
    }

    #[test]
    fn r_requests_a_reprobe() {
        let mut a = app(&["foo"]);
        assert!(!on_input(&mut a, press(KeyCode::Char('r'))));
        assert!(a.probe_requested);
    }

    #[test]
    fn u_requests_an_update_run_on_a_clean_selection() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('u')));
        assert_eq!(a.run_requested.unwrap().action, "update");
    }

    #[test]
    fn colon_opens_the_action_palette() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char(':')));
        assert!(a.palette_open);
    }

    #[test]
    fn palette_letters_are_literal_filter_text() {
        let mut a = app(&["foo"]);
        a.open_palette();
        on_input(&mut a, press(KeyCode::Char('u')));
        assert_eq!(a.palette_filter, "u");
        assert!(
            a.palette_open,
            "typing must not also trigger the u shortcut"
        );
    }

    #[test]
    fn enter_opens_the_detail_view() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Enter));
        assert!(a.detail_open);
    }

    #[test]
    fn esc_closes_the_detail_view() {
        let mut a = app(&["foo"]);
        a.open_detail();
        on_input(&mut a, press(KeyCode::Esc));
        assert!(!a.detail_open);
    }

    #[test]
    fn jk_still_move_the_cursor_while_the_detail_view_is_open() {
        let mut a = app(&["foo", "bar"]);
        a.open_detail();
        on_input(&mut a, press(KeyCode::Char('j')));
        assert_eq!(a.cursor, 1, "the detail view follows the cursor");
    }

    #[test]
    fn ctrl_d_scrolls_the_detail_view_down() {
        let mut a = app(&["foo"]);
        a.open_detail();
        on_input(&mut a, ctrl(KeyCode::Char('d')));
        assert!(a.detail_scroll[&0] > 0);
    }

    #[test]
    fn m_toggles_mouse_capture_in_list_and_detail_modes() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('m')));
        assert!(!a.mouse_captured);
        a.open_detail();
        on_input(&mut a, press(KeyCode::Char('m')));
        assert!(a.mouse_captured);
    }

    #[test]
    fn y_confirms_a_pending_run() {
        let mut a = app(&["foo"]);
        let mut dirty = crate::ui::app::probe::RepoState {
            index: 0,
            branch: Some("main".into()),
            upstream: None,
            ahead: 0,
            behind: 0,
            changed: 0,
            present: true,
            timed_out: false,
        };
        dirty.changed = 1;
        a.on_probe(0, dirty);
        a.request_run("update");
        assert!(a.pending_run.is_some());

        on_input(&mut a, press(KeyCode::Char('y')));
        assert!(a.pending_run.is_none());
        assert!(a.run_requested.is_some());
    }

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
}
