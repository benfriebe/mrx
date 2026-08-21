//! A multi-line text buffer with readline-style editing, for the overlays
//! that take typed input rather than a single keystroke.
//!
//! Holds the text and the cursor and nothing else: no widget, no area, no
//! terminal. [`TextArea::on_key`] applies the editing keys and declines
//! everything else, so a host overlay keeps its own exits and can bind the
//! keys the buffer leaves free.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

/// What counts as part of a word for `alt-b`/`alt-f`, and the whole
/// difference between the two backward deletes: `ctrl-w` takes everything
/// back to whitespace (a whole path), `alt-backspace` one word of it.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_not_whitespace(c: char) -> bool {
    !c.is_whitespace()
}

/// The byte offset in `line` at which display column `column` begins, or the
/// end of the line when it is shorter than that.
fn byte_at_column(line: &str, column: usize) -> usize {
    let mut acc = 0;
    for (i, c) in line.char_indices() {
        if acc >= column {
            return i;
        }
        acc += UnicodeWidthChar::width(c).unwrap_or(0);
    }
    line.len()
}

fn column_of(line: &str) -> usize {
    line.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

#[derive(Default)]
pub struct TextArea {
    text: String,
    /// Byte offset into `text`, always on a char boundary.
    cursor: usize,
    /// The display column vertical movement aims for, so running down through
    /// a short line and out the other side comes back to the column it
    /// started in. Cleared by [`on_key`](Self::on_key) after anything else.
    goal_column: Option<usize>,
}

impl TextArea {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.goal_column = None;
    }

    pub fn lines(&self) -> std::str::Split<'_, char> {
        self.text.split('\n')
    }

    /// The cursor as a line index and a byte offset into that line, which is
    /// what a renderer needs to split the line around it.
    pub fn cursor_cell(&self) -> (usize, usize) {
        let ranges = self.line_ranges();
        let line = self.line_at(&ranges);
        (line, self.cursor - ranges[line].0)
    }

    /// Apply `key` if it is an editing key, reporting whether it was one.
    ///
    /// Forward-delete is bound to `Delete` alone, not to readline's `ctrl-d`:
    /// a host needs one free chord to end input with, and that is the chord
    /// every shell already ends input with.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let vertical = matches!(key.code, KeyCode::Up | KeyCode::Down)
            || (ctrl && matches!(key.code, KeyCode::Char('p' | 'n')));

        let handled = self.apply(key.code, alt, ctrl);
        if handled && !vertical {
            self.goal_column = None;
        }
        handled
    }

    fn apply(&mut self, code: KeyCode, alt: bool, ctrl: bool) -> bool {
        match code {
            KeyCode::Left if alt => self.move_word(-1),
            KeyCode::Right if alt => self.move_word(1),
            KeyCode::Left => self.move_char(-1),
            KeyCode::Right => self.move_char(1),
            KeyCode::Up => self.move_line(-1),
            KeyCode::Down => self.move_line(1),
            KeyCode::Home => self.move_line_start(),
            KeyCode::End => self.move_line_end(),
            KeyCode::Enter => self.insert('\n'),
            KeyCode::Backspace if alt => self.delete_word_before(is_word_char),
            KeyCode::Backspace => self.delete_before(),
            KeyCode::Delete => self.delete_after(),
            KeyCode::Char(c) if alt => match c {
                'b' => self.move_word(-1),
                'f' => self.move_word(1),
                'd' => self.delete_word_after(),
                _ => return false,
            },
            KeyCode::Char(c) if ctrl => match c {
                'b' => self.move_char(-1),
                'f' => self.move_char(1),
                'p' => self.move_line(-1),
                'n' => self.move_line(1),
                'a' => self.move_line_start(),
                'e' => self.move_line_end(),
                'h' => self.delete_before(),
                'w' => self.delete_word_before(is_not_whitespace),
                'u' => self.delete_to_line_start(),
                'k' => self.delete_to_line_end(),
                _ => return false,
            },
            KeyCode::Char(c) => self.insert(c),
            _ => return false,
        }
        true
    }

    /// Byte range of every line, the newline itself excluded. Never empty:
    /// an empty buffer is one empty line.
    fn line_ranges(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut start = 0;
        for (i, b) in self.text.bytes().enumerate() {
            if b == b'\n' {
                out.push((start, i));
                start = i + 1;
            }
        }
        out.push((start, self.text.len()));
        out
    }

    /// Index of the line the cursor sits on. A cursor at a line's end and one
    /// at the next line's start are different positions, so the first range
    /// the cursor fits in is the right one.
    fn line_at(&self, ranges: &[(usize, usize)]) -> usize {
        ranges
            .iter()
            .position(|&(s, e)| self.cursor >= s && self.cursor <= e)
            .unwrap_or(0)
    }

    fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn move_char(&mut self, delta: isize) {
        if delta < 0 {
            if let Some((i, _)) = self.text[..self.cursor].char_indices().next_back() {
                self.cursor = i;
            }
        } else if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    fn move_word(&mut self, delta: isize) {
        self.cursor = if delta < 0 {
            self.word_start(is_word_char)
        } else {
            self.word_end(is_word_char)
        };
    }

    fn move_line(&mut self, delta: isize) {
        let ranges = self.line_ranges();
        let line = self.line_at(&ranges);
        let column = self
            .goal_column
            .unwrap_or_else(|| column_of(&self.text[ranges[line].0..self.cursor]));
        self.goal_column = Some(column);

        let target = (line.cast_signed() + delta)
            .clamp(0, ranges.len().cast_signed() - 1)
            .cast_unsigned();
        let (start, end) = ranges[target];
        self.cursor = start + byte_at_column(&self.text[start..end], column);
    }

    fn move_line_start(&mut self) {
        let ranges = self.line_ranges();
        self.cursor = ranges[self.line_at(&ranges)].0;
    }

    fn move_line_end(&mut self) {
        let ranges = self.line_ranges();
        self.cursor = ranges[self.line_at(&ranges)].1;
    }

    fn delete_before(&mut self) {
        if let Some((i, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.text.replace_range(i..self.cursor, "");
            self.cursor = i;
        }
    }

    fn delete_after(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            let end = self.cursor + c.len_utf8();
            self.text.replace_range(self.cursor..end, "");
        }
    }

    fn delete_word_before(&mut self, word: fn(char) -> bool) {
        let start = self.word_start(word);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    fn delete_word_after(&mut self) {
        let end = self.word_end(is_word_char);
        self.text.replace_range(self.cursor..end, "");
    }

    fn delete_to_line_start(&mut self) {
        let ranges = self.line_ranges();
        let start = ranges[self.line_at(&ranges)].0;
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    fn delete_to_line_end(&mut self) {
        let ranges = self.line_ranges();
        let end = ranges[self.line_at(&ranges)].1;
        self.text.replace_range(self.cursor..end, "");
    }

    /// Where a backward word move or delete lands: back over whatever does
    /// not count as a word, then back over what does. Newlines are not a
    /// boundary, matching readline, so a word move at a line's start carries
    /// on into the line above.
    fn word_start(&self, word: fn(char) -> bool) -> usize {
        let mut at = self.cursor;
        let mut chars = self.text[..at].char_indices().rev().peekable();
        while let Some(&(i, c)) = chars.peek() {
            if word(c) {
                break;
            }
            at = i;
            chars.next();
        }
        while let Some(&(i, c)) = chars.peek() {
            if !word(c) {
                break;
            }
            at = i;
            chars.next();
        }
        at
    }

    /// The forward twin of [`word_start`](Self::word_start).
    fn word_end(&self, word: fn(char) -> bool) -> usize {
        let from = self.cursor;
        let mut at = from;
        let mut chars = self.text[from..].char_indices().peekable();
        while let Some(&(i, c)) = chars.peek() {
            if word(c) {
                break;
            }
            at = from + i + c.len_utf8();
            chars.next();
        }
        while let Some(&(i, c)) = chars.peek() {
            if !word(c) {
                break;
            }
            at = from + i + c.len_utf8();
            chars.next();
        }
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> TextArea {
        let mut area = TextArea::default();
        for c in text.chars() {
            area.on_key(press(KeyCode::Char(c)));
        }
        area
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    /// The cursor as `text` with a `|` drawn at it, so a test reads as the
    /// buffer a user would be looking at.
    fn shown(area: &TextArea) -> String {
        let mut out = area.text().to_string();
        out.insert(area.cursor, '|');
        out
    }

    #[test]
    fn typing_inserts_at_the_cursor_rather_than_the_end() {
        let mut area = typed("git status");
        for _ in 0..6 {
            area.on_key(press(KeyCode::Left));
        }
        area.on_key(press(KeyCode::Char('-')));
        assert_eq!(shown(&area), "git -|status");
    }

    #[test]
    fn a_word_move_crosses_punctuation_and_stops_at_each_word() {
        let mut area = typed("git commit --amend");
        area.on_key(alt(KeyCode::Char('b')));
        assert_eq!(shown(&area), "git commit --|amend");
        area.on_key(alt(KeyCode::Char('b')));
        assert_eq!(shown(&area), "git |commit --amend");
        area.on_key(alt(KeyCode::Char('f')));
        assert_eq!(shown(&area), "git commit| --amend");
    }

    /// The one behavioural difference between the two backward deletes, and
    /// the reason both are bound.
    #[test]
    fn ctrl_w_takes_a_whole_path_where_alt_backspace_takes_one_segment() {
        let mut area = typed("cat src/ui/mod.rs");
        area.on_key(ctrl('w'));
        assert_eq!(shown(&area), "cat |");

        let mut area = typed("cat src/ui/mod.rs");
        area.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(shown(&area), "cat src/ui/mod.|");
    }

    #[test]
    fn ctrl_u_and_ctrl_k_cut_to_the_ends_of_the_current_line_only() {
        let mut area = typed("git fetch\ngit status");
        for _ in 0..6 {
            area.on_key(press(KeyCode::Left));
        }
        area.on_key(ctrl('u'));
        assert_eq!(shown(&area), "git fetch\n|status");
        area.on_key(ctrl('k'));
        assert_eq!(shown(&area), "git fetch\n|");
    }

    #[test]
    fn vertical_movement_keeps_the_column_it_started_in_across_a_short_line() {
        let mut area = typed("git fetch --all\nls\ngit status");
        area.on_key(ctrl('a'));
        area.on_key(press(KeyCode::Up));
        area.on_key(press(KeyCode::Up));
        for _ in 0..12 {
            area.on_key(press(KeyCode::Right));
        }
        assert_eq!(shown(&area), "git fetch --|all\nls\ngit status");

        area.on_key(press(KeyCode::Down));
        assert_eq!(shown(&area), "git fetch --all\nls|\ngit status");
        area.on_key(press(KeyCode::Down));
        assert_eq!(
            shown(&area),
            "git fetch --all\nls\ngit status|",
            "the short line did not cost the column"
        );
    }

    /// Anything else clearing the goal column is what stops a later Down from
    /// jumping back to a column the user has since moved away from.
    #[test]
    fn editing_between_two_vertical_moves_drops_the_remembered_column() {
        let mut area = typed("git fetch --all\nls\ngit status");
        area.on_key(ctrl('a'));
        area.on_key(press(KeyCode::Up));
        area.on_key(press(KeyCode::Up));
        area.on_key(ctrl('e'));

        area.on_key(press(KeyCode::Down));
        area.on_key(press(KeyCode::Left));
        area.on_key(press(KeyCode::Down));
        assert_eq!(shown(&area), "git fetch --all\nls\ng|it status");
    }

    #[test]
    fn a_cursor_move_is_bounded_by_the_ends_of_the_buffer() {
        let mut area = typed("ls");
        for _ in 0..5 {
            area.on_key(press(KeyCode::Left));
        }
        assert_eq!(shown(&area), "|ls");
        area.on_key(press(KeyCode::Backspace));
        assert_eq!(shown(&area), "|ls", "nothing to delete before the start");

        for _ in 0..5 {
            area.on_key(press(KeyCode::Right));
        }
        assert_eq!(shown(&area), "ls|");
        area.on_key(press(KeyCode::Delete));
        assert_eq!(shown(&area), "ls|", "nothing to delete after the end");
    }

    #[test]
    fn backspace_at_a_line_start_joins_it_to_the_line_above() {
        let mut area = typed("ls\n");
        area.on_key(press(KeyCode::Backspace));
        assert_eq!(shown(&area), "ls|");
    }

    #[test]
    fn the_cursor_reports_the_line_it_is_on_and_where_in_it() {
        let mut area = typed("git fetch\nls");
        assert_eq!(area.cursor_cell(), (1, 2));
        area.on_key(ctrl('a'));
        assert_eq!(area.cursor_cell(), (1, 0));
        area.on_key(press(KeyCode::Up));
        assert_eq!(area.cursor_cell(), (0, 0));
    }

    /// A char wider than one cell must not desync the column arithmetic from
    /// the byte offsets, or vertical movement lands mid-character.
    #[test]
    fn a_wide_char_counts_as_the_cells_it_occupies_when_moving_between_lines() {
        let mut area = typed("日本\nabcd");
        area.on_key(press(KeyCode::Up));
        assert_eq!(shown(&area), "日本|\nabcd");
        area.on_key(press(KeyCode::Left));
        area.on_key(press(KeyCode::Down));
        assert_eq!(shown(&area), "日本\nab|cd", "one wide char is two columns");
    }

    #[test]
    fn a_chord_the_buffer_does_not_bind_is_declined_rather_than_typed() {
        let mut area = typed("ls");
        assert!(!area.on_key(ctrl('d')), "the host binds ctrl-d, not this");
        assert!(!area.on_key(press(KeyCode::Esc)));
        assert_eq!(area.text(), "ls");
    }
}
