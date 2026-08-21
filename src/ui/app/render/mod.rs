//! Layout for ui mode: header, repo table, status bar, and the detail view's
//! split and full-screen forms. Which overlays draw comes straight off `App`,
//! not a separate render-time mode.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::detail::{self, DetailLayout};
use super::state::{App, Pane, Sort, SEGMENT_SEP};
use crate::ui::widgets::{display_width, truncate};

mod detail_pane;
mod footer;
mod layout;
mod list;
mod overlays;
#[cfg(test)]
mod testkit;

use detail_pane::draw_detail;
use footer::{status_line, LEAD_IN};
/// Glob so every `render::` geometry path the crate already uses keeps working.
pub(crate) use layout::*;
use list::draw_list;
use overlays::{
    draw_confirm, draw_help, draw_palette, draw_quit_confirm, draw_run_command, draw_set_picker,
    draw_sort_menu,
};

const COL_GAP: usize = 2;
/// Width of the leading " ▸ ● " cursor and selection markers.
const PREFIX_W: usize = 5;
/// The sidebar drops the selection marker, so its rows start " ▸ ".
const SIDEBAR_PREFIX_W: usize = 3;

/// A column's header as it is drawn: its label, carrying the direction arrow
/// when the table is ordered by that column. The layout sizes columns from
/// this and the table draws it, so a sorted column is never left too narrow
/// for the arrow that is the whole point of it.
pub(super) fn column_header(app: &App, column: Sort, label: &str) -> String {
    match app.sort_arrow(column) {
        Some(arrow) => format!("{label} {arrow}"),
        None => label.to_string(),
    }
}

const REPO_LABEL: &str = "REPO";
const BRANCH_LABEL: &str = "BRANCH";
/// Working-tree and upstream state, distinct from [`RESULT_LABEL`], which is
/// what the last run reported.
const STATE_LABEL: &str = "STATE";
const SYNC_LABEL: &str = "SYNC";
const RESULT_LABEL: &str = "RESULT";
/// Title, a blank row, the column labels, and the rule under them: the chrome
/// above the body of every pane, list and detail alike, so their rules meet
/// across a split. Click resolution derives its row offset from this.
pub(crate) const LIST_HEADER_ROWS: usize = 4;
/// The rule and key line under the body, drawn once per frame: by the pane
/// itself when it owns the whole width, by the split when it doesn't.
pub(crate) const FOOTER_ROWS: u16 = 2;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    if app.detail_open {
        match detail::layout_for_width(area.width) {
            DetailLayout::FullScreen => draw_detail(frame, app, area, false),
            DetailLayout::Split => draw_split(frame, app, area),
        }
    } else {
        draw_list(frame, app, area, false);
    }

    if app.palette_open {
        draw_palette(frame, app, area);
    }
    if app.run_command_open {
        draw_run_command(frame, app, area);
    }
    if app.sort_menu_open {
        draw_sort_menu(frame, app, area);
    }
    if app.set_picker_open {
        draw_set_picker(frame, app, area);
    }
    if let Some(pending) = &app.pending_run {
        draw_confirm(frame, app, pending, area);
    }
    if app.quit_pending {
        draw_quit_confirm(frame, area);
    }
    if app.help_open {
        draw_help(frame, area);
    }
}

/// The split: the list narrowed to a sidebar, a rule down the middle, and one
/// footer under both, so the chrome reads as one frame divided rather than as
/// two windows side by side.
fn draw_split(frame: &mut Frame, app: &App, area: Rect) {
    let [panes, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(FOOTER_ROWS)]).areas(area);
    let [list, rule, output] = Layout::horizontal([
        Constraint::Length(detail::sidebar_width(
            area.width,
            sidebar_natural_width(app),
        )),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(panes);

    draw_list(frame, app, list, true);
    draw_detail(frame, app, output, true);
    draw_split_rule(frame, rule);
    draw_split_footer(frame, app, footer, list.width as usize);
}

/// The vertical rule between the split's panes, notched where the two
/// panes' header rules run into it.
fn draw_split_rule(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line> = (0..area.height as usize)
        .map(|row| {
            let glyph = if row + 1 == LIST_HEADER_ROWS {
                "┼"
            } else {
                "│"
            };
            Line::from(Span::styled(glyph, Style::default().fg(Color::DarkGray)))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The split's shared footer: one key line under the whole frame, since every
/// keystroke reaches the same handler whichever side the pointer is on.
fn draw_split_footer(frame: &mut Frame, app: &App, area: Rect, rule_col: usize) {
    let width = area.width as usize;
    let lines = vec![joined_separator(width, rule_col), status_line(app, width)];
    frame.render_widget(Paragraph::new(lines), area);
}

/// A horizontal rule that meets the split's vertical one at `at`.
fn joined_separator(width: usize, at: usize) -> Line<'static> {
    if at >= width {
        return separator(width);
    }
    Line::from(Span::styled(
        format!("{}┴{}", "─".repeat(at), "─".repeat(width - at - 1)),
        Style::default().fg(Color::DarkGray),
    ))
}

/// The list pane's title, already carrying the margin its focus marker
/// occupies (two columns wide either way).
fn header_title(app: &App, split: bool) -> String {
    format!(
        "{}mrx · {}",
        focus_marker(app, Pane::List, split),
        app.set_label
    )
}

/// Marks the status pieces a header too narrow for all of them left off.
const STATUS_ELLIPSIS: &str = "…";

fn header_line(app: &App, width: usize, split: bool) -> Line<'static> {
    let title = header_title(app, split);
    // What is left once the title and the status's own trailing margin are
    // paid for, which is the room `styled_two_column_line` will have.
    let budget = width
        .saturating_sub(display_width(&title))
        .saturating_sub(LEAD_IN.len() + COL_GAP);
    styled_two_column_line(
        &title,
        &fitted_status(&app.header_right_segments(), budget),
        width,
        title_style(app, Pane::List, split),
    )
}

/// The longest run of `segments`, in order, that fits in `budget` cells, with
/// an ellipsis where the rest would have been. A piece is kept whole or not at
/// all: half of "checked 5m ago" is a different claim, not a shorter one.
///
/// The alternative, and what this replaced, is all or nothing, which threw a
/// whole header's worth of room away to save two words off the end.
fn fitted_status(segments: &[String], budget: usize) -> String {
    let whole = segments.join(SEGMENT_SEP);
    if display_width(&whole) <= budget {
        return whole;
    }
    // The marker's own room comes out of the budget before anything is kept.
    let budget = budget.saturating_sub(display_width(SEGMENT_SEP) + display_width(STATUS_ELLIPSIS));
    let mut kept: Vec<&str> = Vec::new();
    let mut spent = 0;
    for segment in segments {
        let sep = if kept.is_empty() {
            0
        } else {
            display_width(SEGMENT_SEP)
        };
        let cost = sep + display_width(segment);
        if spent + cost > budget {
            break;
        }
        spent += cost;
        kept.push(segment);
    }
    if kept.is_empty() {
        return String::new();
    }
    format!("{}{SEGMENT_SEP}{STATUS_ELLIPSIS}", kept.join(SEGMENT_SEP))
}

/// Whether this pane has the keys: a bar in the margin where the other pane
/// has blank indent. [`title_style`] says the same in colour, since one cue
/// alone is easy to miss on a dark theme.
fn focus_marker(app: &App, pane: Pane, split: bool) -> &'static str {
    if split && app.focus == pane {
        "▌ "
    } else {
        LEAD_IN
    }
}

fn title_style(app: &App, pane: Pane, split: bool) -> Style {
    match (split, app.focus == pane) {
        (false, _) => Style::default().bold(),
        (true, true) => Style::default().fg(Color::Cyan).bold(),
        (true, false) => Style::default().fg(Color::DarkGray).bold(),
    }
}

/// `left` at the table's indent and `right` against the far edge, dimmed.
fn two_column_line(left: &str, right: &str, width: usize) -> Line<'static> {
    styled_two_column_line(
        &format!("{LEAD_IN}{left}"),
        right,
        width,
        Style::default().fg(Color::DarkGray),
    )
}

/// `left` arrives already indented, since the margin is where the split's
/// focus marker goes. A width too narrow for both halves keeps `left` and
/// drops `right`, rather than letting them collide or spill past the pane.
fn styled_two_column_line(
    left: &str,
    right: &str,
    width: usize,
    left_style: Style,
) -> Line<'static> {
    let left = left.to_string();
    let right = if right.is_empty() {
        String::new()
    } else {
        format!("{right}{LEAD_IN}")
    };
    let dim = Style::default().fg(Color::DarkGray);
    match width.checked_sub(display_width(&left) + display_width(&right)) {
        Some(gap) => Line::from(vec![
            Span::styled(left, left_style),
            Span::raw(" ".repeat(gap)),
            Span::styled(right, dim),
        ]),
        None => Line::from(Span::styled(truncate(&left, width), left_style)),
    }
}

fn separator(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(Color::DarkGray),
    ))
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    /// The counts are the half `styled_two_column_line` drops first, so the
    /// sidebar buys room for its title and lets them go. Reserving their width
    /// here would spend the output pane's columns on the least important text
    /// on screen, and the cap can take those columns back off the header
    /// anyway, leaving the width bought and the counts gone.
    #[test]
    fn the_sidebar_keeps_its_title_and_lets_the_counts_go() {
        let mut a = app(vec![repo("ab")]);
        a.set_label = "a-rather-long-set-name".into();

        let width = detail::sidebar_width(200, sidebar_natural_width(&a)) as usize;
        assert!(
            width < display_width(&header_title(&a, true)) + display_width(&a.header_right_text()),
            "the sidebar reserved room for counts it will not draw"
        );

        let header = flatten(&header_line(&a, width, true));
        assert!(header.contains("a-rather-long-set-name"), "got {header:?}");
        assert!(!header.contains("1 repo"), "got {header:?}");
    }

    /// A full status line, in the order the header builds it.
    fn status_segments() -> Vec<String> {
        [
            "40 repos",
            "poll 6m · auto",
            "checked 1m ago",
            "sort SYNC ↓",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    /// The header used to be all or nothing: one column short of the whole
    /// status dropped every word of it and left the room blank.
    #[test]
    fn a_header_one_column_short_keeps_what_fits_and_marks_the_rest() {
        let segments = status_segments();
        let whole = segments.join(SEGMENT_SEP);
        assert_eq!(fitted_status(&segments, display_width(&whole)), whole);

        let fitted = fitted_status(&segments, display_width(&whole) - 1);
        assert!(
            fitted.starts_with("40 repos · poll 6m · auto"),
            "got {fitted:?}"
        );
        assert!(fitted.ends_with(STATUS_ELLIPSIS), "got {fitted:?}");
        assert!(!fitted.contains("sort"), "got {fitted:?}");
    }

    #[test]
    fn a_header_with_room_for_one_piece_keeps_the_count() {
        let fitted = fitted_status(&status_segments(), 16);
        assert_eq!(fitted, format!("40 repos{SEGMENT_SEP}{STATUS_ELLIPSIS}"));
    }

    /// Half of "checked 1m ago" is a different claim, not a shorter one.
    #[test]
    fn a_header_with_no_room_for_a_whole_piece_says_nothing() {
        assert_eq!(fitted_status(&status_segments(), 6), "");
    }

    #[test]
    fn a_fitted_status_never_overflows_the_room_it_was_given() {
        let segments = status_segments();
        for budget in 0..60 {
            let fitted = fitted_status(&segments, budget);
            assert!(
                display_width(&fitted) <= budget,
                "budget {budget} drew {} cells: {fitted:?}",
                display_width(&fitted)
            );
        }
    }

    /// The budget `header_line` works out has to match what
    /// `styled_two_column_line` actually has room for, or the line overflows
    /// and ratatui clips it wherever it lands.
    #[test]
    fn the_header_line_never_overflows_the_width_it_was_given() {
        let mut a = app(vec![repo("bill-api"), repo("crew")]);
        a.poll_enabled = true;
        for width in 0..90 {
            let text = flatten(&header_line(&a, width, false));
            assert!(
                display_width(&text) <= width,
                "width {width} drew {} cells: {text:?}",
                display_width(&text)
            );
        }
    }

    /// Only the split gives the counts up. A full-width list has the room,
    /// and they are how it says what the poll and the last check are doing.
    #[test]
    fn the_full_width_list_still_carries_its_counts() {
        let a = app(vec![repo("ab")]);
        let header = flatten(&header_line(&a, 120, false));
        assert!(header.contains("1 repo"), "got {header:?}");
    }

    /// The split's two panes are drawn as separate widgets, so nothing but
    /// this test makes their header rules meet.
    #[test]
    fn both_panes_of_the_split_rule_off_their_header_on_the_same_row() {
        let mut a = app(vec![repo("bill-api"), repo("crew")]);
        a.detail_open = true;
        let (list, output) = split_panes(&a, 140, 20);

        assert!(
            list[LIST_HEADER_ROWS - 1].starts_with('─'),
            "the list rules off row {}, got {:?}",
            LIST_HEADER_ROWS - 1,
            list[LIST_HEADER_ROWS - 1]
        );
        assert!(
            output[LIST_HEADER_ROWS - 1].starts_with('─'),
            "the detail pane rules off the same row, got {:?}",
            output[LIST_HEADER_ROWS - 1]
        );
    }

    #[test]
    fn the_split_draws_one_footer_under_both_panes_not_one_each() {
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        let (list, output) = split_panes(&a, 140, 20);
        for pane in [&list, &output] {
            assert!(
                !pane.iter().any(|line| line.contains("? help")),
                "a pane drew its own footer: {pane:?}"
            );
        }
    }

    #[test]
    fn the_vertical_rule_notches_where_the_header_rules_meet_it() {
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        let rows = frame_rows(&a, 140, 20);
        let col = detail::sidebar_width(140, sidebar_natural_width(&a)) as usize;
        assert_eq!(
            rows[LIST_HEADER_ROWS - 1].chars().nth(col),
            Some('┼'),
            "got {:?}",
            rows[LIST_HEADER_ROWS - 1]
        );
        assert_eq!(rows[0].chars().nth(col), Some('│'), "got {:?}", rows[0]);
    }

    #[test]
    fn the_shared_footers_rule_meets_the_vertical_one() {
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        let rows = frame_rows(&a, 140, 20);
        let col = detail::sidebar_width(140, sidebar_natural_width(&a)) as usize;
        assert_eq!(rows[rows.len() - 2].chars().nth(col), Some('┴'));
    }

    #[test]
    fn the_focused_pane_is_the_one_wearing_the_marker() {
        let mut a = app(vec![repo("bill-api")]);
        a.open_detail();
        let (list, output) = split_panes(&a, 140, 20);
        assert!(list[0].starts_with('▌'), "got {:?}", list[0]);
        assert!(!output[0].starts_with('▌'), "got {:?}", output[0]);

        a.toggle_focus();
        let (list, output) = split_panes(&a, 140, 20);
        assert!(!list[0].starts_with('▌'), "got {:?}", list[0]);
        assert!(output[0].starts_with('▌'), "got {:?}", output[0]);
    }

    /// Nothing has focus when only one pane is on screen, so a marker would
    /// point at a distinction that isn't being made.
    #[test]
    fn the_full_width_list_wears_no_focus_marker() {
        let a = app(vec![repo("bill-api")]);
        let rows = frame_rows(&a, 90, 12);
        assert!(!rows[0].starts_with('▌'), "got {:?}", rows[0]);
    }

    #[test]
    fn a_blank_row_separates_the_app_title_from_the_column_labels() {
        let a = app(vec![repo("bill-api")]);
        let rows = frame_rows(&a, 90, 12);
        assert!(rows[0].contains("mrx · "), "got {:?}", rows[0]);
        assert!(rows[1].is_empty(), "got {:?}", rows[1]);
        assert!(rows[2].contains(REPO_LABEL), "got {:?}", rows[2]);
    }
}
