//! Pure geometry for the panes: how many rows the body of a pane gets, how
//! far the list has scrolled, and how wide each column ends up.

use super::footer::LEAD_IN;
use super::{column_header, header_title, BRANCH_LABEL, COL_GAP, FOOTER_ROWS};
use super::{LIST_HEADER_ROWS, PREFIX_W, REPO_LABEL, STATE_LABEL, SYNC_LABEL};
use crate::ui::app::detail::{self, DetailLayout};
use crate::ui::app::probe;
use crate::ui::app::state::{sync_width, App, Sort};
use crate::ui::widgets::display_width;
use ratatui::layout::Rect;

/// Where everything in one frame sits, worked out once.
///
/// The pointer has no `Frame` to ask and the scroll keys have none either, so
/// both used to rebuild the layout from the terminal size with their own
/// arithmetic. Everything reads the rects and row counts from here instead.
pub(crate) struct Panes {
    /// The repo table, full width or as the split's sidebar. `None` while the
    /// detail view has the whole frame.
    pub(crate) list: Option<Rect>,
    /// The output pane, `None` while the table has the whole frame.
    pub(crate) detail: Option<Rect>,
    /// The split's vertical rule, between the two panes.
    pub(crate) rule: Option<Rect>,
    /// The footer under both panes of a split. A pane owning the whole frame
    /// draws its own inside its rect instead, which is why this is an option
    /// rather than always a rect.
    pub(crate) shared_footer: Option<Rect>,
    /// Body rows the table draws.
    pub(crate) list_rows: usize,
    /// Transcript rows the output pane draws.
    pub(crate) detail_rows: usize,
}

impl Panes {
    /// Lay out `area` for the mode `app` is in.
    pub(crate) fn new(area: Rect, app: &App) -> Self {
        if !app.detail_open {
            return Self {
                list: Some(area),
                detail: None,
                rule: None,
                shared_footer: None,
                // The filter line is chrome the table pays for, and only the
                // full-width table ever draws one.
                list_rows: body_rows(area.height, chrome_rows(app)),
                detail_rows: 0,
            };
        }

        if detail::layout_for_width(area.width) == DetailLayout::FullScreen {
            return Self {
                list: None,
                detail: Some(area),
                rule: None,
                shared_footer: None,
                list_rows: 0,
                detail_rows: body_rows(area.height, LIST_HEADER_ROWS + FOOTER_ROWS as usize),
            };
        }

        // One footer under the whole frame rather than one per pane, so the
        // chrome reads as a divided frame rather than two windows. Clamped, so
        // a frame too short to hold one still lays out inside itself.
        let footer_height = FOOTER_ROWS.min(area.height);
        let body_height = area.height - footer_height;
        let sidebar = detail::sidebar_width(area.width, sidebar_natural_width(app));
        let rows = body_rows(body_height, LIST_HEADER_ROWS);
        Self {
            list: Some(Rect::new(area.x, area.y, sidebar, body_height)),
            rule: Some(Rect::new(area.x + sidebar, area.y, 1, body_height)),
            detail: Some(Rect::new(
                area.x + sidebar + 1,
                area.y,
                area.width.saturating_sub(sidebar + 1),
                body_height,
            )),
            shared_footer: Some(Rect::new(
                area.x,
                area.y + body_height,
                area.width,
                footer_height,
            )),
            list_rows: rows,
            detail_rows: rows,
        }
    }

    /// The frame as `App` last recorded it. Click resolution and the scroll
    /// keys have no `Frame` to measure, so they lay out against the size the
    /// resize handler stored, which trails the real one only while a redraw
    /// is in flight.
    pub(crate) fn last_known(app: &App) -> Self {
        Self::new(
            Rect::new(0, 0, app.terminal_width, app.terminal_height),
            app,
        )
    }

    /// The table body row screen `row` lands on.
    pub(crate) fn list_body_row(&self, row: u16) -> Option<usize> {
        body_row_at(self.list?, self.list_rows, row)
    }

    /// The output pane's content row screen `row` lands on.
    pub(crate) fn detail_body_row(&self, row: u16) -> Option<usize> {
        body_row_at(self.detail?, self.detail_rows, row)
    }

    /// Whether `column` is over the output pane rather than the table beside
    /// it. The output is always the rightmost pane, so its left edge decides.
    pub(crate) fn over_detail(&self, column: u16) -> bool {
        self.detail.is_some_and(|pane| column >= pane.x)
    }
}

/// Half a pane's body, floored at one row so a very short terminal still
/// scrolls. Approximate by design: it measures the last known frame rather
/// than the exact viewport, which is close enough for a "half page" key.
pub(crate) fn half_page(rows: usize) -> usize {
    (rows / 2).max(1)
}

/// A pane's body height: what is left of it once its chrome is taken out.
fn body_rows(height: u16, chrome: usize) -> usize {
    (height as usize).saturating_sub(chrome)
}

/// The body row `row` lands on inside `pane`: `None` above its header, past
/// the last body row, or off the pane entirely.
fn body_row_at(pane: Rect, rows: usize, row: u16) -> Option<usize> {
    let within = usize::from(row.checked_sub(pane.y)?);
    let body = within.checked_sub(LIST_HEADER_ROWS)?;
    (body < rows).then_some(body)
}

/// Total chrome rows above and below the table body: the header (fixed at
/// [`LIST_HEADER_ROWS`]) plus a bottom separator, status line, and (while
/// `/` is capturing text) the filter line.
fn chrome_rows(app: &App) -> usize {
    LIST_HEADER_ROWS + FOOTER_ROWS as usize + usize::from(app.filtering)
}

/// First visible-list position on screen: the same computation the table
/// is drawn with, so click resolution and cursor following can't drift.
pub(crate) fn list_start(app: &App, visible: &[usize], height: usize) -> usize {
    let cursor_pos = visible.iter().position(|&i| i == app.cursor).unwrap_or(0);
    scroll_offset(app.list_scroll, cursor_pos, visible.len(), height)
}

/// Name and state column widths for the two-column sidebar. Name shrink-wraps
/// the longest repo name so the state text sits beside the names rather than
/// across a field of empty cells, capped at two thirds of `avail` so one long
/// name can't squeeze the state out; state takes what is left.
pub(super) fn sidebar_column_widths(app: &App, avail: usize) -> (usize, usize) {
    if avail == 0 {
        return (0, 0);
    }
    let name = natural_name_width(app).clamp(1, (avail * 2 / 3).max(1));
    let state = avail.saturating_sub(name + COL_GAP);
    (name, state)
}

/// How wide the sidebar wants to be: its prefix and two columns at the widths
/// their text needs, and never narrower than its own title.
///
/// The counts beside that title are not part of the demand. They are the first
/// thing [`styled_two_column_line`] sheds, and [`detail::sidebar_width`] caps
/// this at a third of the frame anyway, which can drop them and keep the width.
pub(crate) fn sidebar_natural_width(app: &App) -> u16 {
    let columns = PREFIX_W + natural_name_width(app) + COL_GAP + natural_state_width(app);
    let title = display_width(&header_title(app, true)) + LEAD_IN.len();
    u16::try_from(columns.max(title)).unwrap_or(u16::MAX)
}

fn natural_name_width(app: &App) -> usize {
    app.repos
        .iter()
        .map(|r| display_width(&r.name))
        .max()
        .unwrap_or(0)
        .max(header_width(app, Sort::Repo, REPO_LABEL))
}

/// How wide a column's header needs the column to be, arrow included.
fn header_width(app: &App, column: Sort, label: &str) -> usize {
    display_width(&column_header(app, column, label))
}

/// The widest state text any row could be showing, from the same source
/// [`sidebar_repo_line`] draws from. A repo still being probed shows a
/// one-column spinner, which never sets the width.
fn natural_state_width(app: &App) -> usize {
    app.probes
        .iter()
        .flatten()
        .map(|state| display_width(&probe::dirty_text(state)))
        .max()
        .unwrap_or(0)
        .max(header_width(app, Sort::State, STATE_LABEL))
}

/// Column widths for the repo table: NAME, BRANCH, STATE and SYNC each get
/// their natural width up to a share of `avail`; RESULT takes the rest.
///
/// SYNC is never capped: it is already as narrow as the counts on screen allow,
/// and squeezing it would truncate a number into a different number.
pub(super) fn column_widths(app: &App, avail: usize) -> (usize, usize, usize, usize, usize) {
    let name_nat = app
        .repos
        .iter()
        .map(|r| display_width(&r.name))
        .max()
        .unwrap_or(0)
        .max(header_width(app, Sort::Repo, REPO_LABEL));
    let branch_nat = (0..app.repos.len())
        .map(|i| display_width(&app.probe_display(i).branch))
        .max()
        .unwrap_or(0)
        .max(header_width(app, Sort::Branch, BRANCH_LABEL));
    let state_nat = (0..app.repos.len())
        .map(|i| display_width(&app.probe_display(i).state))
        .max()
        .unwrap_or(0)
        .max(header_width(app, Sort::State, STATE_LABEL));

    let sync = sync_nat(app);

    let name = name_nat.min(avail / 4);
    let branch = branch_nat.min(avail / 6);
    let state = state_nat.min(avail / 4);
    let result = avail.saturating_sub(name + branch + state + sync + 4 * COL_GAP);
    (name, branch, state, sync, result)
}

/// SYNC's width: its counts, or the header's, unless no row has a count at
/// all. An empty column still drawn as `SYNC` is four cells claiming a
/// distance nothing has measured, so it is dropped entirely instead.
fn sync_nat(app: &App) -> usize {
    match sync_width(app.sync_widths()) {
        0 => 0,
        counts => counts.max(header_width(app, Sort::Sync, SYNC_LABEL)),
    }
}

/// First visible-list index to draw: `prev` where it still shows the cursor,
/// otherwise the nearest offset that does, so the cursor travels within the
/// window instead of dragging the whole list along with it.
pub(crate) fn scroll_offset(
    prev: usize,
    cursor_pos: usize,
    visible_len: usize,
    list_height: usize,
) -> usize {
    if list_height == 0 || visible_len == 0 {
        return 0;
    }
    prev.min(cursor_pos)
        .max((cursor_pos + 1).saturating_sub(list_height))
        .min(visible_len.saturating_sub(list_height))
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::LIST_HEADER_ROWS;
    use super::*;
    use crate::ui::app::detail;

    #[test]
    fn scroll_offset_stays_zero_while_the_cursor_fits_on_screen() {
        assert_eq!(scroll_offset(0, 0, 10, 5), 0);
        assert_eq!(scroll_offset(0, 4, 10, 5), 0);
    }

    #[test]
    fn scroll_offset_follows_the_cursor_past_the_bottom() {
        assert_eq!(scroll_offset(0, 5, 10, 5), 1);
        assert_eq!(scroll_offset(1, 9, 10, 5), 5);
    }

    #[test]
    fn scroll_offset_holds_still_while_the_cursor_moves_inside_the_window() {
        // The window row 9 forced open, with the cursor walking back up it.
        assert_eq!(scroll_offset(5, 8, 10, 5), 5);
        assert_eq!(scroll_offset(5, 5, 10, 5), 5);
        // Only stepping off the top edge moves it, and only by the one row.
        assert_eq!(scroll_offset(5, 4, 10, 5), 4);
    }

    #[test]
    fn scroll_offset_pulls_a_window_left_past_the_end_back_into_range() {
        // A filter that shrinks the list under a scrolled window.
        assert_eq!(scroll_offset(20, 2, 6, 5), 1);
    }

    #[test]
    fn scroll_offset_handles_an_empty_list() {
        assert_eq!(scroll_offset(0, 0, 0, 5), 0);
    }

    #[test]
    fn name_column_never_exceeds_a_quarter_of_the_available_width() {
        let long_name = "a-name-that-is-extremely-long-past-any-reasonable-column-width";
        let a = app(vec![repo(long_name)]);
        let (name, branch, state, sync, result) = column_widths(&a, 80);
        assert!(name <= 20, "got name width {name}");
        assert_eq!(name + branch + state + sync + result + 4 * COL_GAP, 80);
    }

    #[test]
    fn the_sidebar_asks_for_its_columns_not_a_fixed_share_of_the_frame() {
        let a = app(vec![repo("bill-api"), repo("crew")]);
        let columns = PREFIX_W + display_width("bill-api") + COL_GAP + display_width(STATE_LABEL);
        assert!(
            sidebar_natural_width(&a) as usize >= columns,
            "the columns have to fit"
        );
        assert!(
            detail::sidebar_width(200, sidebar_natural_width(&a)) < 200 / 3,
            "a short list should leave the output more than the mockup's two thirds"
        );
    }

    #[test]
    fn sidebar_columns_reserve_room_for_the_state_text_instead_of_giving_it_all_to_name() {
        let a = app(vec![repo("a-very-long-repo-name-indeed-far-too-long")]);
        let (name, state) = sidebar_column_widths(&a, 30);
        assert!(
            state > 0,
            "the state column must not be squeezed to nothing"
        );
        assert_eq!(name + state + COL_GAP, 30);
    }

    #[test]
    fn the_sidebar_name_column_shrinks_to_the_names_rather_than_taking_a_fixed_share() {
        let a = app(vec![repo("bill-api"), repo("crew")]);
        let (name, _) = sidebar_column_widths(&a, 60);
        assert_eq!(
            name,
            display_width("bill-api"),
            "the longest name sets the column, so STATE sits beside it"
        );
    }

    #[test]
    fn sidebar_columns_handle_zero_available_width() {
        let a = app(vec![repo("bill-api")]);
        assert_eq!(sidebar_column_widths(&a, 0), (0, 0));
    }
    /// Three of the old derivations were right only because
    /// `(h - footer) - header` and `h - header - footer` are the same number,
    /// and one was right only because a mode pair happens to be unreachable.
    /// One layout means the rects have to account for the frame exactly.
    #[test]
    fn the_panes_tile_the_frame_at_every_size() {
        let mut a = app(vec![repo("bill-api"), repo("crew")]);
        a.detail_open = true;
        for width in [1u16, 20, 79, 80, 120, 200] {
            for height in 0..40u16 {
                let area = Rect::new(0, 0, width, height);
                let panes = Panes::new(area, &a);
                let at = format!("{width}x{height}");

                for rect in [panes.list, panes.detail, panes.rule, panes.shared_footer]
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        rect.x + rect.width <= area.width,
                        "{at}: {rect:?} is wider than the frame"
                    );
                    assert!(
                        rect.y + rect.height <= area.height,
                        "{at}: {rect:?} is taller than the frame"
                    );
                }

                let Some(rule) = panes.rule else { continue };
                let list = panes
                    .list
                    .expect("a split draws the table beside the output");
                let detail = panes.detail.expect("a split draws an output pane");
                let footer = panes.shared_footer.expect("a split shares one footer");

                assert_eq!(
                    list.width + rule.width + detail.width,
                    area.width,
                    "{at}: a column is unaccounted for"
                );
                assert_eq!(
                    rule.x,
                    list.x + list.width,
                    "{at}: the rule has drifted off the seam"
                );
                assert_eq!(
                    detail.x,
                    rule.x + rule.width,
                    "{at}: the output has drifted off the rule"
                );
                assert_eq!(
                    list.height + footer.height,
                    area.height,
                    "{at}: a row is unaccounted for"
                );
                assert_eq!(
                    list.height, detail.height,
                    "{at}: the panes' rules would not meet"
                );
            }
        }
    }

    /// The count the pointer resolves clicks against, checked against the rows
    /// the table puts on screen rather than against the arithmetic that
    /// produced it.
    #[test]
    fn the_table_draws_exactly_the_rows_the_layout_gave_it() {
        let names: Vec<String> = (0..60).map(|i| format!("repo-{i:02}")).collect();
        let a = app(names.iter().map(|n| repo(n)).collect());
        for height in 8..24u16 {
            let rows = Panes::new(Rect::new(0, 0, 100, height), &a).list_rows;
            let drawn = frame_rows(&a, 100, height);
            assert!(rows > 0, "height {height} should still draw a body");

            for offset in 0..rows {
                assert!(
                    drawn[LIST_HEADER_ROWS + offset].contains("repo-"),
                    "height {height}: body row {offset} is not a repo"
                );
            }
            // The renderer draws whatever count this hands it, so asserting
            // the body against that count proves nothing. What the frame can
            // still say is where the rule under the body ended up: too many
            // rows push the footer off the bottom, too few leave a gap.
            let rule = drawn
                .iter()
                .rposition(|line| line.starts_with('─'))
                .expect("the body has a rule under it");
            assert_eq!(
                rule,
                usize::from(height - FOOTER_ROWS),
                "height {height}: the table's footer is not the last thing on the frame"
            );
            assert_eq!(
                rule,
                LIST_HEADER_ROWS + rows,
                "height {height}: the row count does not reach the rule the table drew"
            );
        }
    }

    /// Click resolution used to subtract the filter line in the split too,
    /// where the sidebar has no such line, costing the sidebar its last
    /// clickable row.
    #[test]
    fn the_split_sidebar_pays_for_no_filter_line() {
        let mut a = app(vec![repo("bill-api"), repo("crew")]);
        a.detail_open = true;
        let area = Rect::new(0, 0, 120, 30);

        let without = Panes::new(area, &a).list_rows;
        a.filtering = true;
        assert_eq!(
            Panes::new(area, &a).list_rows,
            without,
            "the sidebar drew no filter line either way"
        );
    }
}
