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
        Event::Resize(width, height) => {
            on_resize(app, width, height);
            false
        }
        _ => false,
    }
}

/// `Terminal::draw` re-queries the real terminal size and resizes its own
/// buffers on its own (ratatui's `autoresize`), so this just keeps `App`'s
/// cached width/height, used for click and scroll geometry between draws,
/// from lagging a frame behind an actual resize.
fn on_resize(app: &mut App, width: u16, height: u16) {
    app.terminal_width = width;
    app.terminal_height = height;
}

fn on_key(app: &mut App, key: KeyEvent) -> bool {
    if app.quit_pending {
        return on_quit_confirm_key(app, key);
    }
    if app.pending_run.is_some() {
        return on_confirm_key(app, key);
    }
    if app.set_picker_open {
        on_set_picker_key(app, key);
        return false;
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
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return app.request_quit(),
            KeyCode::Char('r') => {
                app.reload_config();
                return false;
            }
            KeyCode::Char('a') => {
                app.toggle_auto_update();
                return false;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Char('q') => return app.request_quit(),
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
        KeyCode::Char('F') => app.toggle_poll(),
        KeyCode::Char('o') => app.request_open_editor(),
        KeyCode::Tab => app.open_set_picker(),
        KeyCode::Esc => app.request_cancel(),
        KeyCode::Enter => app.open_detail(),
        _ => {}
    }
    false
}

/// Keys while `q`/`Ctrl-C` is waiting on a "quit while a run is live?"
/// confirmation: `y`/Enter confirms, everything else declines.
fn on_quit_confirm_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => app.confirm_quit(),
        _ => {
            app.cancel_quit();
            false
        }
    }
}

/// Keys while the set picker is open: just navigation and the two exits.
/// No text capture, unlike the action palette, since the list of sets is
/// short enough to scan without filtering it.
fn on_set_picker_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.close_set_picker(),
        KeyCode::Enter => app.confirm_set_picker(),
        KeyCode::Char('j') | KeyCode::Down => app.set_picker_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.set_picker_move(-1),
        _ => {}
    }
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
            KeyCode::Char('c') => return app.request_quit(),
            KeyCode::Char('d') => {
                app.detail_scroll_down();
                return false;
            }
            KeyCode::Char('u') => {
                app.detail_scroll_up();
                return false;
            }
            KeyCode::Char('r') => {
                app.reload_config();
                return false;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Char('q') => return app.request_quit(),
        KeyCode::Char('j') | KeyCode::Down => app.move_cursor(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_cursor(-1),
        KeyCode::Esc => app.close_detail(),
        KeyCode::Char('y') => app.copy_visible_step(),
        KeyCode::Char('m') => app.toggle_mouse_capture(),
        KeyCode::Char('o') => app.request_open_editor(),
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
    if app.pending_run.is_some() || app.palette_open || app.set_picker_open || app.quit_pending {
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
            None,
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

    #[test]
    fn o_requests_the_editor_in_list_and_detail_modes() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('o')));
        assert!(a.open_editor_requested);
        a.open_editor_requested = false;

        a.open_detail();
        on_input(&mut a, press(KeyCode::Char('o')));
        assert!(a.open_editor_requested);
    }

    #[test]
    fn resize_updates_the_cached_terminal_size() {
        let mut a = app(&["foo"]);
        assert!(!on_input(&mut a, Event::Resize(132, 43)));
        assert_eq!(a.terminal_width, 132);
        assert_eq!(a.terminal_height, 43);
    }

    #[test]
    fn tab_opens_the_set_picker() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Tab));
        assert!(a.set_picker_open);
    }

    #[test]
    fn ctrl_r_triggers_a_config_reload() {
        let mut a = app(&["foo"]);
        on_input(&mut a, ctrl(KeyCode::Char('r')));
        assert!(
            a.full_reprobe_requested,
            "reloading must ask for a fresh probe"
        );
    }

    #[test]
    fn esc_cancels_a_live_run_in_list_mode() {
        use crate::executor::TaskEvent;

        let mut a = app(&["foo", "bar"]);
        let run_id = a.begin_named_run("update".into(), vec![0, 1]);
        a.on_task(run_id, TaskEvent::Started { index: 0 });

        on_input(&mut a, press(KeyCode::Esc));
        assert!(
            a.status_message
                .as_deref()
                .is_some_and(|m| m.contains("cancelled")),
            "got {:?}",
            a.status_message
        );
    }

    #[test]
    fn q_prompts_before_quitting_while_a_run_is_live() {
        use crate::executor::TaskEvent;

        let mut a = app(&["foo"]);
        let run_id = a.begin_named_run("update".into(), vec![0]);
        a.on_task(run_id, TaskEvent::Started { index: 0 });

        assert!(
            !on_input(&mut a, press(KeyCode::Char('q'))),
            "must not quit immediately"
        );
        assert!(a.quit_pending);
        assert!(on_input(&mut a, press(KeyCode::Char('y'))), "y confirms");
    }

    #[test]
    fn declining_the_quit_prompt_leaves_the_app_open() {
        use crate::executor::TaskEvent;

        let mut a = app(&["foo"]);
        let run_id = a.begin_named_run("update".into(), vec![0]);
        a.on_task(run_id, TaskEvent::Started { index: 0 });

        on_input(&mut a, press(KeyCode::Char('q')));
        assert!(!on_input(&mut a, press(KeyCode::Char('n'))));
        assert!(!a.quit_pending);
    }
}
