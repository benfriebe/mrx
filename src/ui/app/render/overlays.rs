//! Everything drawn over the frame rather than in it: the help, palette,
//! set-picker and confirmation overlays, and the centring they share.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::footer::LEAD_IN;
use super::COL_GAP;
use crate::ui::app::actions::Source;
use crate::ui::app::keymap;
use crate::ui::app::state::{App, PendingRun, Sort};
use crate::ui::widgets::display_width;

/// The full keymap, centred over the table rather than replacing it. It lists
/// the detail view's keys alongside the list's, since the overlay is the one
/// place both sets can be read at once, two bindings to a row so a short
/// terminal doesn't crop the bottom off.
pub(super) fn draw_help(frame: &mut Frame, area: Rect) {
    let every = || keymap::LIST_KEYS.iter().chain(keymap::DETAIL_KEYS);
    let key_col = every().map(|b| display_width(b.keys)).max().unwrap_or(0);
    let label_col = every().map(|b| display_width(b.label)).max().unwrap_or(0);

    let bound = |bindings: &[keymap::Binding]| {
        bindings
            .chunks(2)
            .map(|pair| {
                let spans = pair
                    .iter()
                    .flat_map(|b| {
                        [
                            Span::styled(
                                format!("{LEAD_IN}{:>key_col$}  ", b.keys),
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::raw(format!("{:<label_col$}", b.label)),
                        ]
                    })
                    .collect::<Vec<_>>();
                Line::from(spans)
            })
            .collect::<Vec<_>>()
    };
    let heading = |text: &'static str| {
        Line::from(Span::styled(
            format!("  {text}"),
            Style::default().fg(Color::DarkGray).bold(),
        ))
    };

    let mut lines = vec![heading("IN THE LIST")];
    lines.extend(bound(keymap::LIST_KEYS));
    lines.push(Line::default());
    lines.push(heading("IN THE DETAIL VIEW"));
    lines.extend(bound(keymap::DETAIL_KEYS));
    lines.push(Line::default());
    lines.extend(
        keymap::NOTES
            .iter()
            .map(|n| Line::from(Span::styled(*n, Style::default().fg(Color::DarkGray)))),
    );

    // Wide enough for two key columns and for the widest note, so nothing
    // has to wrap to a second line and push the rest off the bottom.
    let columns = 2 * (LEAD_IN.len() + key_col + COL_GAP + label_col);
    let notes = keymap::NOTES
        .iter()
        .map(|n| display_width(n))
        .max()
        .unwrap_or(0);
    let width = u16::try_from(columns.max(notes) + 2).unwrap_or(u16::MAX);
    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let popup = centred(area, width, height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" keys · esc to close "),
            )
            // A note longer than the box wraps rather than losing its tail.
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// A `width` by `height` rect centred in `area`, clamped so an overlay
/// larger than the terminal is cropped rather than positioned off-screen.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let [_, band, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, centre, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width.min(area.width)),
        Constraint::Fill(1),
    ])
    .areas(band);
    centre
}

fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - pct_y) / 2),
        Constraint::Percentage(pct_y),
        Constraint::Percentage((100 - pct_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - pct_x) / 2),
        Constraint::Percentage(pct_x),
        Constraint::Percentage((100 - pct_x) / 2),
    ])
    .split(vertical[1])[1]
}

/// The action palette (`:`): every runnable action for the set, filtered as
/// you type, showing source and repo count so an unfamiliar name is
/// trustworthy before you run it.
pub(super) fn draw_palette(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered_rect(60, 60, area);
    frame.render_widget(Clear, popup);

    let repo_count = app.repos.len();
    let targets = app.effective_selection().len();
    let entries = app.palette_visible();
    // Names vary in width, so what follows them only reads as a column if
    // every name is padded out to the widest.
    let name_col = entries
        .iter()
        .map(|a| display_width(&a.name))
        .max()
        .unwrap_or(0);

    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let pad = name_col + COL_GAP - display_width(&a.name);
            let name = format!("{}{:pad$}", a.name, "");

            // A selection command's count is what it leaves selected, not
            // what it would run on, so it is worded against the whole set.
            let text = match a.source {
                Source::Selection => {
                    format!("{name}leaves {} of {} selected", a.repos, repo_count)
                }
                Source::Builtin => format!("{name}builtin, {}", runs_on(a.repos, targets)),
                Source::Default => format!("{name}every repo, {}", runs_on(a.repos, targets)),
                Source::PerRepo => format!("{name}per-repo, {}", runs_on(a.repos, targets)),
                Source::Prompt => format!("{name}prompt, {}", runs_on(a.repos, targets)),
            };
            let style = if i == app.palette_cursor {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(text).style(style)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" action: {} ", app.palette_filter));
    frame.render_widget(List::new(items).block(block), popup);
}

/// What a palette entry would run against right now. The `of` half appears
/// only when the two differ, which is exactly when a per-repo action would
/// skip part of the selection.
fn runs_on(defined: usize, targets: usize) -> String {
    if defined == targets {
        format!("runs on {targets}")
    } else {
        format!("runs on {defined} of {targets}")
    }
}

/// The run-command prompt (`r`): the body being typed, run as one `sh`
/// script against the selection once Ctrl-D closes it.
pub(super) fn draw_run_command(frame: &mut Frame, app: &App, area: Rect) {
    let targets = app.effective_selection().len();
    let (cursor_line, cursor_byte) = app.run_command.cursor_cell();

    let lines: Vec<Line> = app
        .run_command
        .lines()
        .enumerate()
        .map(|(i, text)| {
            if i == cursor_line {
                line_with_cursor(text, cursor_byte)
            } else {
                Line::from(text.to_string())
            }
        })
        .collect();

    let popup = centered_rect(60, 40, area);
    frame.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title(format!(
        " run on {targets} repo{} ",
        if targets == 1 { "" } else { "s" }
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // The hints keep their rows whatever the body does, so they are laid out
    // beside it rather than appended to it and scrolled away. The editing keys
    // are named here rather than under `?`: this is the one screen they work
    // on, and the help overlay has no room left for them.
    let [body, hints] = Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(inner);
    let cursor_column =
        display_width(&app.run_command.lines().nth(cursor_line).unwrap_or("")[..cursor_byte]);
    frame.render_widget(
        Paragraph::new(lines).scroll((
            scroll_to_show(cursor_line, body.height),
            scroll_to_show(cursor_column, body.width),
        )),
        body,
    );
    let dim = Style::default().fg(Color::DarkGray);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("enter newline   ^d run   esc cancel", dim)),
            Line::from(Span::styled(
                "^a/^e ends   ^w/^u/^k cut   alt-b/f word",
                dim,
            )),
        ]),
        hints,
    );
}

/// How far to scroll one axis so `at` is the last cell still on screen.
/// Nothing scrolls until the cursor would otherwise leave, so a body that
/// fits is drawn from its start.
fn scroll_to_show(at: usize, extent: u16) -> u16 {
    u16::try_from(at.saturating_sub(usize::from(extent).saturating_sub(1))).unwrap_or(u16::MAX)
}

/// A line with the cell at `byte` drawn as the cursor. Past the end of the
/// line that cell is a space, which is the only thing that makes the cursor
/// visible on an empty line or at the end of a full one.
fn line_with_cursor(text: &str, byte: usize) -> Line<'static> {
    let block = Style::default().bg(Color::Cyan).fg(Color::Black);
    let (before, rest) = text.split_at(byte.min(text.len()));
    let mut chars = rest.chars();
    match chars.next() {
        Some(c) => Line::from(vec![
            Span::raw(before.to_string()),
            Span::styled(c.to_string(), block),
            Span::raw(chars.as_str().to_string()),
        ]),
        None => Line::from(vec![
            Span::raw(before.to_string()),
            Span::styled(" ", block),
        ]),
    }
}

/// The set picker (`tab`): every set `sets::discover()` finds, plus the
/// active config appended as `(unnamed)` when it isn't one of them, with a
/// `*` marking whichever entry is actually on screen right now.
pub(super) fn draw_set_picker(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered_rect(50, 50, area);
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = app
        .set_entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let marker = if entry.path == app.config_path {
                "* "
            } else {
                "  "
            };
            let style = if i == app.set_picker_cursor {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(format!("{marker}{}", entry.name)).style(style)
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(" switch set ");
    frame.render_widget(List::new(items).block(block), popup);
}

/// The sort menu (`S`): one row per column, each with the key that orders the
/// table by it. The active column carries its arrow, so the menu also says
/// which way pressing that key again would flip it.
pub(super) fn draw_sort_menu(frame: &mut Frame, app: &App, area: Rect) {
    let rows = u16::try_from(Sort::ALL.len()).unwrap_or(u16::MAX);
    let popup = centred(area, 24, rows + 2);
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = Sort::ALL
        .iter()
        .map(|&column| {
            let arrow = app.sort_arrow(column).unwrap_or(" ");
            let style = if app.sort == column {
                Style::default().fg(Color::Cyan).bold()
            } else {
                Style::default()
            };
            ListItem::new(format!(" {}  {:7} {arrow}", column.key(), column.header())).style(style)
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(" sort by ");
    frame.render_widget(List::new(items).block(block), popup);
}

/// Shown when `q`/`Ctrl-C` is pressed while a run is still live.
pub(super) fn draw_quit_confirm(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(50, 20, area);
    frame.render_widget(Clear, popup);

    let text = vec![
        Line::from("a run is still live, quit anyway?"),
        Line::default(),
        Line::from(Span::styled(
            "y/enter quit   anything else cancels",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let block = Block::default().borders(Borders::ALL).title(" quit? ");
    frame.render_widget(Paragraph::new(text).block(block), popup);
}

/// The dirty-selection confirmation: shown before a run touches any repo the
/// last probe found dirty, unless `--force` skipped it.
pub(super) fn draw_confirm(frame: &mut Frame, app: &App, pending: &PendingRun, area: Rect) {
    let mut text = vec![
        Line::from(format!(
            "run '{}' on {} repo{}, {}?",
            pending.action,
            pending.targets.len(),
            if pending.targets.len() == 1 { "" } else { "s" },
            confirm_reason(pending),
        )),
        Line::default(),
        Line::from(Span::styled(
            "y/enter confirm   n/esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    // Named, not "the cursor row": the prompt covers the whole screen, and
    // the row it would narrow to is behind it.
    if let Some(cursor) = pending.cursor_only {
        text.push(Line::from(Span::styled(
            format!("c  just {}", app.repos[cursor].name),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let width = text.iter().map(Line::width).max().unwrap_or(0) + 4;
    let popup = centred(
        area,
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(text.len() + 2).unwrap_or(u16::MAX),
    );
    frame.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title(" confirm ");
    frame.render_widget(Paragraph::new(text).block(block), popup);
}

/// The confirmation's reason clause. Unprobed is worth pausing over too, but
/// it is not the same claim as dirty, so a selection that is only unprobed
/// says so rather than being called dirty.
fn confirm_reason(pending: &PendingRun) -> String {
    match (pending.dirty_count, pending.unknown_count) {
        (0, 0) => "confirm".to_string(),
        (dirty, 0) => format!("{dirty} of them dirty"),
        (0, unknown) => format!("{unknown} of them not yet checked"),
        (dirty, unknown) => format!("{dirty} of them dirty, {unknown} not yet checked"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// The overlay grows with the keymap, and `centred` crops rather than
    /// scrolls, so the notes at the bottom are what a terminal one row too
    /// short silently loses.
    #[test]
    fn the_help_overlay_fits_a_terminal_no_taller_than_thirty_rows() {
        let mut a = app(vec![repo("bill-api")]);
        a.help_open = true;
        let rows = frame_rows(&a, 100, 30);
        let last = keymap::NOTES.last().unwrap().trim();
        assert!(
            rows.iter().any(|line| line.contains(last)),
            "the last note was cropped off the bottom: {rows:#?}"
        );
    }

    /// Width crops the same way height does, one step removed: the overlay is
    /// as wide as its widest label and its widest note, and anything past the
    /// terminal wraps to a second row rather than truncating, pushing the
    /// notes off the bottom.
    #[test]
    fn the_help_overlay_fits_a_terminal_no_wider_than_eighty_columns() {
        let mut a = app(vec![repo("bill-api")]);
        a.help_open = true;
        let rows = frame_rows(&a, 80, 30);
        let last = keymap::NOTES.last().unwrap().trim();
        assert!(
            rows.iter().any(|line| line.contains(last)),
            "a wrapped line pushed the last note off the bottom: {rows:#?}"
        );
    }

    /// The menu is the only place the column keys are written down, so a
    /// column missing from it is a column nothing can reach.
    #[test]
    fn the_sort_menu_lists_every_column_and_marks_the_active_one() {
        let mut a = app(vec![repo("bill-api")]);
        a.choose_sort(Sort::State);
        a.sort_menu_open = true;
        let rows = frame_rows(&a, 100, 30);
        let shown = |needle: &str| rows.iter().any(|line| line.contains(needle));

        for &column in Sort::ALL {
            assert!(
                shown(&format!("{}  {}", column.key(), column.header())),
                "{column:?} is not offered with its key: {rows:#?}"
            );
        }
        assert!(
            shown(&format!("{} ↓", Sort::State.header())),
            "the active column carries its arrow: {rows:#?}"
        );
    }

    #[test]
    fn the_run_command_overlay_shows_the_typed_body_and_how_to_run_it() {
        let mut a = app(vec![repo("bill-api"), repo("crew")]);
        a.open_run_command();
        for code in [
            KeyCode::Char('l'),
            KeyCode::Char('s'),
            KeyCode::Enter,
            KeyCode::Char('p'),
            KeyCode::Char('w'),
            KeyCode::Char('d'),
        ] {
            a.run_command
                .on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        let rows = frame_rows(&a, 100, 30);

        assert!(
            rows.iter().any(|row| row.contains("run on 2 repos")),
            "the title says what the body would run against: {rows:#?}"
        );
        for line in ["ls", "pwd"] {
            assert!(
                rows.iter().any(|row| row.contains(line)),
                "the body is drawn a line at a time, missing {line:?}: {rows:#?}"
            );
        }
        assert!(
            rows.iter()
                .any(|row| row.contains("enter newline   ^d run   esc cancel")),
            "the hint is the only place Ctrl-D is named: {rows:#?}"
        );
    }

    /// The cursor is the only thing on screen saying where the next keystroke
    /// lands, so it has to follow the buffer rather than sit at the end.
    #[test]
    fn the_run_command_cursor_is_drawn_where_the_buffer_says_it_is() {
        let mut a = app(vec![repo("bill-api")]);
        a.open_run_command();
        for code in [KeyCode::Char('l'), KeyCode::Char('s'), KeyCode::Left] {
            a.run_command
                .on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, &a))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        let cursor = (0..20)
            .flat_map(|y| (0..60).map(move |x| (x, y)))
            .find(|&(x, y)| buffer[(x, y)].style().bg == Some(Color::Cyan))
            .expect("the cursor cell is drawn");
        assert_eq!(
            buffer[cursor].symbol(),
            "s",
            "one left of the end is the `s`, not the cell after it"
        );
    }

    /// The palette's names run from `push` to `select-visible`, so the source
    /// and counts land wherever the name happens to end unless they are given
    /// a column of their own.
    #[test]
    fn every_palette_row_starts_its_source_at_the_same_column() {
        let mut a = app(vec![repo("bill-api")]);
        a.palette_open = true;
        let rows = frame_rows(&a, 100, 40);

        let phrases = ["builtin,", "every repo,", "per-repo,", "prompt,", "leaves"];
        let columns: Vec<usize> = rows
            .iter()
            .filter_map(|row| phrases.iter().find_map(|p| col_of(row, p)))
            .collect();

        // Six built-in verbs, the prompt, and three selection commands.
        assert_eq!(columns.len(), 10, "not every row was found: {rows:#?}");
        assert!(
            columns.iter().all(|&c| c == columns[0]),
            "sources landed at {columns:?}: {rows:#?}"
        );
    }
}
