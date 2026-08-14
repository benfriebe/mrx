//! Keymap and mouse dispatch for the resident app. Input arrives as a
//! `crossterm::Event` rather than a `KeyEvent`, so mouse and resize land on
//! this same entry point; only keys do anything this phase.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::state::App;

/// Dispatch one input event. Returns true when the app should quit.
pub fn on_input(app: &mut App, event: Event) -> bool {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => on_key(app, key),
        _ => false,
    }
}

fn on_key(app: &mut App, key: KeyEvent) -> bool {
    if app.filtering {
        on_filter_key(app, key);
        return false;
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Repo;
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
        App::new(names.iter().map(|n| repo(n)).collect(), "work".into(), 4)
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
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
}
