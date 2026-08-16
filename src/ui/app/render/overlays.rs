//! Everything drawn over the frame rather than in it: the help, palette,
//! set-picker and confirmation overlays, and the centring they share.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::footer::LEAD_IN;
use super::COL_GAP;
use crate::ui::app::actions::Source;
use crate::ui::app::keymap;
use crate::ui::app::state::{App, PendingRun};
use crate::ui::widgets::display_width;

/// The full keymap, centred over the table rather than replacing it.
///
/// It lists the detail view's keys alongside the list's, since the overlay
/// is the one place both sets can be read at once: inside the detail view
/// only its own footer is on screen. Two bindings to a row: one each reads
/// more easily but runs off the bottom of a short terminal, and a help
/// screen that crops is worse than one that packs.
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
            // A note longer than the box wraps rather than losing its tail
            // off the right edge.
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
/// trustworthy before you run it (section 08).
pub(super) fn draw_palette(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered_rect(60, 60, area);
    frame.render_widget(Clear, popup);

    let repo_count = app.repos.len();
    let items: Vec<ListItem> = app
        .palette_visible()
        .iter()
        .enumerate()
        .map(|(i, a)| {
            // A selection command's count is what it leaves selected, not
            // how many repos define it, so it is worded rather than shaped
            // to match.
            let text = match a.source {
                Source::Selection => {
                    format!("{}  leaves {} of {} selected", a.name, a.repos, repo_count)
                }
                Source::Builtin => format!("{}  builtin, {} of {}", a.name, a.repos, repo_count),
                Source::Default => format!("{}  every repo, {} of {}", a.name, a.repos, repo_count),
                Source::PerRepo => format!("{}  per-repo, {} of {}", a.name, a.repos, repo_count),
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

/// Shown when `q`/`Ctrl-C` is pressed while a run is still live (section 03,
/// "prompts if a run is live").
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

/// The dirty-selection confirmation from section 11: shown before a run
/// touches any repo the last probe found dirty, unless `--force` skipped it.
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

/// The confirmation's reason clause: dirty and unprobed repos are both
/// worth pausing over (section 11), but they are not the same claim, so a
/// selection that is only unprobed says so rather than calling it dirty.
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
}
