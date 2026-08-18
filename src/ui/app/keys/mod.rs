//! Keymap and mouse dispatch for ui mode. Input arrives as a
//! `crossterm::Event` rather than a `KeyEvent`, so mouse and resize land on
//! this same entry point, and a click and Enter can't disagree about what
//! opening a row means.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};

use super::state::{App, Pane, Sort};

mod mouse;
#[cfg(test)]
mod testkit;

use mouse::on_mouse;

/// Dispatch one input event. Returns true when the app should quit.
pub fn on_input(app: &mut App, event: Event) -> bool {
    if clears_status_message(&event) {
        app.status_message = None;
    }
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

/// The status line is transient: the user's next action replaces it with the
/// footer. The tail of a mouse gesture is not that action, and treating it as
/// one wiped the swallowed-drag hint before it could be painted. Capture also
/// asks for all motion (`?1003h`), so `Moved` arrives on a mere twitch.
fn clears_status_message(event: &Event) -> bool {
    !matches!(
        event,
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved | MouseEventKind::Drag(_) | MouseEventKind::Up(_),
            ..
        })
    )
}

/// `Terminal::draw` resizes its own buffers (ratatui's `autoresize`), so all
/// this does is keep `App`'s cached width/height, used for click and scroll
/// geometry between draws, from lagging a frame behind a real resize.
fn on_resize(app: &mut App, width: u16, height: u16) {
    app.terminal_width = width;
    app.terminal_height = height;
}

fn on_key(app: &mut App, key: KeyEvent) -> bool {
    // Quit is bound in every mode, and handled here rather than in each of
    // them: a mode-local handler that forgets to wire it (the set picker, the
    // dirty-run confirm) would strand the user with no way out. A second
    // Ctrl-C at the quit prompt confirms rather than declining, like `y`.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return if app.quit_pending {
            app.confirm_quit()
        } else {
            app.request_quit()
        };
    }
    if app.quit_pending {
        return on_quit_confirm_key(app, key);
    }
    // Dismissed by any key: the overlay reads rather than does, so there is
    // nothing to hunt for an exit key over.
    if app.help_open {
        app.help_open = false;
        return false;
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
    if app.run_command_open {
        on_run_command_key(app, key);
        return false;
    }
    if app.sort_menu_open {
        on_sort_menu_key(app, key);
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
        // crossterm reports Ctrl-U as `Char('u')` with the modifier set, so an
        // unmatched Ctrl chord must return rather than fall through to the
        // plain-letter shortcuts and run `update` on a readline keystroke.
        return match key.code {
            KeyCode::Char('d') => {
                app.move_cursor_half_page(1);
                false
            }
            KeyCode::Char('u') => {
                app.move_cursor_half_page(-1);
                false
            }
            KeyCode::Char('r') => {
                app.reload_config();
                false
            }
            KeyCode::Char('a') => {
                app.toggle_auto_update();
                false
            }
            _ => false,
        };
    }

    match key.code {
        KeyCode::Char('q') => return app.request_quit(),
        KeyCode::Char('j') | KeyCode::Down => app.move_cursor(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_cursor(-1),
        KeyCode::Char('g') => app.move_to_first(),
        KeyCode::Char('G') => app.move_to_last(),
        KeyCode::Char(' ') => app.toggle_selection_at_cursor(),
        KeyCode::Char('a') => app.select_all_visible(),
        KeyCode::Char('A') => app.select_all_in_set(),
        KeyCode::Char('c') => app.clear_selection(),
        KeyCode::Char('i') => app.invert_selection(),
        KeyCode::Char('!') => app.request_shell(),
        KeyCode::Char('/') => app.start_filter(),
        KeyCode::Char('r') => app.open_run_command(),
        KeyCode::Char('R') => app.probe_requested = true,
        KeyCode::Char('u') => app.request_run("update"),
        KeyCode::Char('s') => app.request_run("status"),
        KeyCode::Char('f') => app.request_run("fetch"),
        KeyCode::Char('d') => app.request_run("diff"),
        KeyCode::Char(':') => app.open_palette(),
        KeyCode::Char('m') => app.toggle_mouse_capture(),
        KeyCode::Char('?') => app.help_open = true,
        KeyCode::Char('F') => app.toggle_poll(),
        KeyCode::Char('S') => app.sort_menu_open = true,
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

/// Keys while the set picker is open: just navigation and the two exits. No
/// text capture, unlike the palette, since the list of sets is short enough to
/// scan without filtering it.
/// Keys while the sort menu is open: one column key, or anything else to
/// leave the order alone. It swallows every key it does not bind, since the
/// menu covers the list it would otherwise reach: `s` behind it means "sort
/// by STATE", never "run status".
fn on_sort_menu_key(app: &mut App, key: KeyEvent) {
    app.sort_menu_open = false;
    if let KeyCode::Char(c) = key.code {
        if let Some(sort) = Sort::from_key(c) {
            app.choose_sort(sort);
        }
    }
}

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

/// Keys while the run-command prompt is open: the two the prompt itself owns,
/// then whatever [`TextArea`](crate::ui::textarea::TextArea) makes of the
/// rest. Ctrl-D is taken before the buffer sees it, so a body is ended by the
/// chord that ends input everywhere else rather than by Enter, which the body
/// needs for its own newlines.
fn on_run_command_key(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
        app.run_command_confirm();
        return;
    }
    if key.code == KeyCode::Esc {
        app.close_run_command();
        return;
    }
    app.run_command.on_key(key);
}

/// Keys while the dirty-selection confirmation is up: a modal that swallows
/// everything but yes/no.
fn on_confirm_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => app.confirm_pending_run(),
        KeyCode::Char('c') => app.confirm_pending_run_at_cursor(),
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
        // `j`/`k` follow the focus. Both panes are on screen either way, so
        // which one moves has to be something the user chose.
        KeyCode::Char('j') | KeyCode::Down => match app.focus {
            Pane::List => app.move_cursor(1),
            Pane::Output => app.detail_scroll_by(1),
        },
        KeyCode::Char('k') | KeyCode::Up => match app.focus {
            Pane::List => app.move_cursor(-1),
            Pane::Output => app.detail_scroll_by(-1),
        },
        KeyCode::Tab => app.toggle_focus(),
        KeyCode::Enter => app.focus_output(),
        KeyCode::Esc => app.close_detail(),
        KeyCode::Char('y') => app.copy_visible_step(),
        KeyCode::Char('m') => app.toggle_mouse_capture(),
        KeyCode::Char('?') => app.help_open = true,
        KeyCode::Char('o') => app.request_open_editor(),
        KeyCode::Char('!') => app.request_shell(),
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::testkit::{app, ctrl, press, probed};
    use super::*;

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
    fn shift_s_opens_the_sort_menu_and_a_column_key_orders_by_it() {
        let mut a = app(&["foo", "bar"]);
        on_input(&mut a, press(KeyCode::Char('S')));
        assert!(a.sort_menu_open);

        on_input(&mut a, press(KeyCode::Char(Sort::State.key())));
        assert!(!a.sort_menu_open);
        assert_eq!(a.sort, Sort::State);
    }

    /// The menu covers the list, so every key it does not bind has to stop
    /// there: `s` behind it would otherwise start a status run at the same
    /// time as ordering the table by STATE.
    #[test]
    fn the_sort_menu_swallows_the_keys_it_does_not_bind() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('S')));
        on_input(&mut a, press(KeyCode::Char('z')));

        assert!(!a.sort_menu_open, "an unbound key closes it");
        assert_eq!(a.sort, Sort::default(), "and leaves the order alone");
        assert!(a.run_requested.is_none());
        assert!(a.pending_run.is_none());
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

    /// Esc in the list cancels a run, not a filter, so a committed filter has
    /// no key of its own that drops it; `/` starts over instead of resuming
    /// the search already narrowing the list.
    #[test]
    fn slash_starts_a_fresh_search_after_a_committed_filter() {
        let mut a = app(&["alpha", "bravo"]);
        on_input(&mut a, press(KeyCode::Char('/')));
        on_input(&mut a, press(KeyCode::Char('a')));
        on_input(&mut a, press(KeyCode::Char('l')));
        on_input(&mut a, press(KeyCode::Enter));
        assert_eq!(a.visible_indices(), vec![0]);

        on_input(&mut a, press(KeyCode::Char('/')));
        assert!(a.filtering);
        assert!(a.filter.is_empty());
        assert_eq!(a.visible_indices(), vec![0, 1]);
    }

    #[test]
    fn slash_is_literal_text_while_filtering() {
        let mut a = app(&["foo"]);
        a.start_filter();
        on_input(&mut a, press(KeyCode::Char('/')));
        assert_eq!(a.filter, "/", "a repo name can contain a slash");
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

    /// The two halves of one swap: `r` now types a command and `R` is what
    /// re-probes, so pinning either alone would let them drift back together.
    #[test]
    fn r_opens_the_run_command_prompt_and_shift_r_requests_a_reprobe() {
        let mut a = app(&["foo"]);
        assert!(!on_input(&mut a, press(KeyCode::Char('r'))));
        assert!(a.run_command_open);
        assert!(!a.probe_requested);

        a.close_run_command();
        assert!(!on_input(&mut a, press(KeyCode::Char('R'))));
        assert!(a.probe_requested);
    }

    #[test]
    fn typed_keys_enter_and_backspace_build_a_multi_line_body() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('r')));
        for c in "ls".chars() {
            on_input(&mut a, press(KeyCode::Char(c)));
        }
        on_input(&mut a, press(KeyCode::Enter));
        for c in "pwdx".chars() {
            on_input(&mut a, press(KeyCode::Char(c)));
        }
        on_input(&mut a, press(KeyCode::Backspace));

        assert_eq!(a.run_command.text(), "ls\npwd");
        assert!(a.run_command_open, "enter is a newline, not a confirm");
    }

    #[test]
    fn a_list_shortcut_letter_is_literal_text_in_the_run_command_prompt() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('r')));
        assert!(!on_input(&mut a, press(KeyCode::Char('q'))));
        on_input(&mut a, press(KeyCode::Char('u')));
        assert_eq!(a.run_command.text(), "qu");
        assert!(a.run_requested.is_none() && a.pending_run.is_none());
    }

    #[test]
    fn an_unmatched_ctrl_chord_is_not_typed_into_the_run_command_prompt() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('r')));
        on_input(&mut a, ctrl(KeyCode::Char('u')));
        assert!(a.run_command.text().is_empty());
        assert!(a.run_command_open);
    }

    #[test]
    fn ctrl_d_runs_the_typed_body_over_a_probed_clean_selection() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main"));
        on_input(&mut a, press(KeyCode::Char('r')));
        for c in "git fetch".chars() {
            on_input(&mut a, press(KeyCode::Char(c)));
        }
        on_input(&mut a, ctrl(KeyCode::Char('d')));

        assert!(!a.run_command_open);
        assert_eq!(a.run_requested.unwrap().body.as_deref(), Some("git fetch"));
    }

    #[test]
    fn ctrl_d_on_a_dirty_selection_confirms_first_and_keeps_the_body() {
        let mut a = app(&["foo"]);
        let mut dirty = probed(0, "main");
        dirty.changed = 1;
        a.on_probe(0, dirty);

        on_input(&mut a, press(KeyCode::Char('r')));
        for c in "ls".chars() {
            on_input(&mut a, press(KeyCode::Char(c)));
        }
        on_input(&mut a, ctrl(KeyCode::Char('d')));
        assert!(a.run_requested.is_none());
        assert!(a.pending_run.is_some());

        on_input(&mut a, press(KeyCode::Char('y')));
        assert_eq!(a.run_requested.unwrap().body.as_deref(), Some("ls"));
    }

    #[test]
    fn esc_closes_the_run_command_prompt_without_running() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main"));
        on_input(&mut a, press(KeyCode::Char('r')));
        on_input(&mut a, press(KeyCode::Char('x')));
        on_input(&mut a, press(KeyCode::Esc));

        assert!(!a.run_command_open);
        assert!(a.run_requested.is_none() && a.pending_run.is_none());
    }

    #[test]
    fn u_requests_an_update_run_on_a_clean_selection() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main"));
        on_input(&mut a, press(KeyCode::Char('u')));
        assert_eq!(a.run_requested.unwrap().action, "update");
    }

    #[test]
    fn ctrl_u_does_not_trigger_the_update_shortcut() {
        let mut a = app(&["foo"]);
        on_input(&mut a, ctrl(KeyCode::Char('u')));
        assert!(
            a.run_requested.is_none() && a.pending_run.is_none(),
            "Ctrl-U must not fall through to the plain u shortcut"
        );
    }

    #[test]
    fn ctrl_d_pages_the_list_down_and_ctrl_u_brings_it_back() {
        let names: Vec<String> = (0..40).map(|i| format!("repo-{i:02}")).collect();
        let mut a = app(&names.iter().map(String::as_str).collect::<Vec<_>>());
        a.terminal_height = 26;

        on_input(&mut a, ctrl(KeyCode::Char('d')));
        let paged = a.cursor;
        assert!(paged > 1, "Ctrl-D moves a page, not a line, got {paged}");

        on_input(&mut a, ctrl(KeyCode::Char('u')));
        assert_eq!(a.cursor, 0, "Ctrl-U comes back the same distance");
    }

    #[test]
    fn ctrl_f_does_not_trigger_the_fetch_shortcut() {
        let mut a = app(&["foo"]);
        on_input(&mut a, ctrl(KeyCode::Char('f')));
        assert!(a.run_requested.is_none() && a.pending_run.is_none());
    }

    #[test]
    fn ctrl_s_does_not_trigger_the_status_shortcut() {
        let mut a = app(&["foo"]);
        on_input(&mut a, ctrl(KeyCode::Char('s')));
        assert!(a.run_requested.is_none() && a.pending_run.is_none());
    }

    #[test]
    fn ctrl_d_does_not_trigger_the_diff_shortcut() {
        let mut a = app(&["foo"]);
        on_input(&mut a, ctrl(KeyCode::Char('d')));
        assert!(a.run_requested.is_none() && a.pending_run.is_none());
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
    fn ctrl_c_quits_while_the_palette_is_open() {
        let mut a = app(&["foo"]);
        a.open_palette();
        assert!(
            on_input(&mut a, ctrl(KeyCode::Char('c'))),
            "Ctrl-C is the one binding the plan calls universal"
        );
    }

    #[test]
    fn ctrl_c_quits_while_filtering() {
        let mut a = app(&["foo"]);
        a.start_filter();
        assert!(on_input(&mut a, ctrl(KeyCode::Char('c'))));
    }

    #[test]
    fn enter_opens_the_detail_view() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Enter));
        assert!(a.detail_open);
    }

    #[test]
    fn a_second_enter_hands_the_keys_to_the_output_pane() {
        let mut a = app(&["foo", "bar"]);
        on_input(&mut a, press(KeyCode::Enter));
        assert_eq!(a.focus, Pane::List, "opening a row keeps the keys on it");

        on_input(&mut a, press(KeyCode::Enter));
        assert_eq!(a.focus, Pane::Output);
        on_input(&mut a, press(KeyCode::Char('j')));
        assert_eq!(a.cursor, 0, "j now scrolls the output, not the list");
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
        let mut dirty = probed(0, "main");
        dirty.changed = 1;
        a.on_probe(0, dirty);
        a.request_run("update");
        assert!(a.pending_run.is_some());

        on_input(&mut a, press(KeyCode::Char('y')));
        assert!(a.pending_run.is_none());
        assert!(a.run_requested.is_some());
    }

    #[test]
    fn c_confirms_a_pending_run_against_the_cursor_row_alone() {
        let mut a = app(&["foo", "bar"]);
        for i in 0..2 {
            let mut dirty = probed(i, "main");
            dirty.changed = 1;
            a.on_probe(0, dirty);
        }
        a.cursor = 1;
        a.request_run("update");

        on_input(&mut a, press(KeyCode::Char('c')));
        assert_eq!(a.run_requested.as_ref().unwrap().targets, vec![1]);
    }

    #[test]
    fn question_mark_opens_the_help_overlay_and_the_next_key_closes_it() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('?')));
        assert!(a.help_open);
        on_input(&mut a, press(KeyCode::Esc));
        assert!(!a.help_open);
    }

    #[test]
    fn a_key_that_closes_the_help_overlay_does_nothing_else() {
        let mut a = app(&["foo", "bar"]);
        a.cursor = 0;
        on_input(&mut a, press(KeyCode::Char('?')));
        on_input(&mut a, press(KeyCode::Char('j')));
        assert!(!a.help_open);
        assert_eq!(a.cursor, 0, "j dismissed the overlay rather than moving");
    }

    #[test]
    fn question_mark_is_filter_text_rather_than_help_while_filtering() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('/')));
        on_input(&mut a, press(KeyCode::Char('?')));
        assert!(!a.help_open);
        assert_eq!(a.filter, "?");
    }

    #[test]
    fn ctrl_c_still_quits_from_the_help_overlay() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('?')));
        let ev = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(
            on_input(&mut a, ev),
            "ctrl-c should quit, not just close help"
        );
    }

    #[test]
    fn o_requests_the_editor_from_the_list() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('o')));
        assert!(a.foreground.is_some());
    }

    #[test]
    fn bang_requests_a_shell_from_either_mode() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Char('!')));
        assert!(a.foreground.is_some());
        a.foreground = None;

        a.open_detail();
        on_input(&mut a, press(KeyCode::Char('!')));
        assert!(a.foreground.is_some());
    }

    /// The split shows both panes at once, so `j` has two plausible
    /// meanings and `tab` is what picks between them.
    #[test]
    fn j_moves_the_cursor_on_the_list_and_scrolls_the_output_on_the_other_pane() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.open_detail();

        on_input(&mut a, press(KeyCode::Char('j')));
        assert_eq!(a.cursor, 1, "the list has the keys when the view opens");
        assert!(a.detail_scroll.is_empty(), "and the output did not move");

        on_input(&mut a, press(KeyCode::Tab));
        on_input(&mut a, press(KeyCode::Char('j')));
        assert_eq!(
            a.cursor, 1,
            "the cursor stays put once the output has focus"
        );
        assert_eq!(a.detail_scroll.get(&1), Some(&1));
    }

    #[test]
    fn tab_still_opens_the_set_picker_from_the_plain_list() {
        let mut a = app(&["foo"]);
        on_input(&mut a, press(KeyCode::Tab));
        assert!(a.set_picker_open);
    }

    #[test]
    fn c_clears_the_selection_and_shift_a_takes_the_whole_set() {
        let mut a = app(&["foo", "bar", "baz"]);
        a.filter = "ba".into();

        on_input(&mut a, press(KeyCode::Char('A')));
        assert_eq!(a.selected.len(), 3, "the whole set, not just what shows");

        on_input(&mut a, press(KeyCode::Char('c')));
        assert!(a.selected.is_empty());
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
    fn ctrl_c_quits_while_the_set_picker_is_open() {
        let mut a = app(&["foo"]);
        a.open_set_picker();
        assert!(
            on_input(&mut a, ctrl(KeyCode::Char('c'))),
            "the set picker has no dedicated exit key for Ctrl-C, so a mode-local \
             handler that never sees it can't be trusted to quit"
        );
    }

    #[test]
    fn ctrl_c_quits_while_the_dirty_run_confirmation_is_up() {
        let mut a = app(&["foo"]);
        let mut dirty = probed(0, "main");
        dirty.changed = 1;
        a.on_probe(0, dirty);
        a.request_run("update");
        assert!(a.pending_run.is_some());

        assert!(on_input(&mut a, ctrl(KeyCode::Char('c'))));
    }

    #[test]
    fn a_second_ctrl_c_at_the_quit_prompt_quits_instead_of_dismissing_it() {
        use crate::executor::TaskEvent;

        let mut a = app(&["foo"]);
        let run_id = a.begin_named_run("update".into(), vec![0]);
        a.on_task(run_id, TaskEvent::Started { index: 0 });

        assert!(
            !on_input(&mut a, ctrl(KeyCode::Char('c'))),
            "must not quit immediately while a run is live"
        );
        assert!(a.quit_pending);
        assert!(
            on_input(&mut a, ctrl(KeyCode::Char('c'))),
            "a second Ctrl-C at the prompt confirms, like y/Enter, rather than declining"
        );
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
