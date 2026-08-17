//! Pure geometry for the panes: how many rows the body of a pane gets, how
//! far the list has scrolled, and how wide each column ends up.

use super::footer::LEAD_IN;
use super::{header_title, BRANCH_LABEL, COL_GAP, FOOTER_ROWS, LIST_HEADER_ROWS, PREFIX_W};
use super::{REPO_LABEL, STATE_LABEL};
use crate::ui::app::probe;
use crate::ui::app::state::App;
use crate::ui::widgets::display_width;

/// Total chrome rows above and below the table body: the header (fixed at
/// [`LIST_HEADER_ROWS`]) plus a bottom separator, status line, and (while
/// `/` is capturing text) the filter line.
pub(crate) fn chrome_rows(app: &App) -> usize {
    LIST_HEADER_ROWS + FOOTER_ROWS as usize + if app.filtering { 1 } else { 0 }
}

pub(crate) fn list_height(app: &App, area_height: u16) -> usize {
    (area_height as usize).saturating_sub(chrome_rows(app))
}

/// First visible-list position on screen: the same computation the table
/// is drawn with, so click resolution and cursor following can't drift.
pub(crate) fn list_start(app: &App, visible: &[usize], height: usize) -> usize {
    let cursor_pos = visible.iter().position(|&i| i == app.cursor).unwrap_or(0);
    scroll_offset(app.list_scroll, cursor_pos, visible.len(), height)
}

/// Transcript rows a detail pane shows: the pane minus its chrome, minus
/// its own footer when it draws one (a split pane shares the frame's,
/// which occupies the same rows, so both layouts land on the same count).
pub(crate) fn detail_content_height(pane_height: u16, split: bool) -> usize {
    let footer = if split { 0 } else { FOOTER_ROWS as usize };
    (pane_height as usize).saturating_sub(LIST_HEADER_ROWS + footer)
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

/// How wide the sidebar wants to be: its prefix and two columns at the
/// widths their text actually needs, but never so narrow that the header
/// has to drop the repo counts, which are the first thing
/// [`styled_two_column_line`] gives up. [`detail::sidebar_width`] caps it.
pub(crate) fn sidebar_natural_width(app: &App) -> u16 {
    let columns = PREFIX_W + natural_name_width(app) + COL_GAP + natural_state_width(app);
    // The header's columns need the same gap the rows use, or the title and
    // the counts meet with nothing between them.
    let header = display_width(&header_title(app, true))
        + COL_GAP
        + display_width(&app.header_right_text())
        + LEAD_IN.len();
    u16::try_from(columns.max(header)).unwrap_or(u16::MAX)
}

fn natural_name_width(app: &App) -> usize {
    app.repos
        .iter()
        .map(|r| display_width(&r.name))
        .max()
        .unwrap_or(0)
        .max(display_width(REPO_LABEL))
}

/// The widest state text any row could be showing, from the same source
/// [`sidebar_repo_line`] draws from. A repo still being probed has no state
/// text yet, so it never sets the width.
fn natural_state_width(app: &App) -> usize {
    app.probes
        .iter()
        .flatten()
        .map(|state| display_width(&probe::dirty_text_brief(state)))
        .max()
        .unwrap_or(0)
        .max(display_width(STATE_LABEL))
}

/// Column widths for the four-column repo table: NAME, BRANCH and STATE each
/// get their natural width up to a share of `avail`; RESULT takes the rest,
/// being the column whose text is usually worth reading in full.
pub(super) fn column_widths(app: &App, avail: usize) -> (usize, usize, usize, usize) {
    let name_nat = app
        .repos
        .iter()
        .map(|r| display_width(&r.name))
        .max()
        .unwrap_or(0)
        .max(display_width(REPO_LABEL));
    let branch_nat = (0..app.repos.len())
        .map(|i| display_width(&app.probe_display(i).branch))
        .max()
        .unwrap_or(0)
        .max(display_width(BRANCH_LABEL));
    let state_nat = (0..app.repos.len())
        .map(|i| display_width(&app.probe_display(i).state))
        .max()
        .unwrap_or(0)
        .max(display_width(STATE_LABEL));

    let name = name_nat.min(avail / 4);
    let branch = branch_nat.min(avail / 6);
    let state = state_nat.min(avail / 4);
    let result = avail.saturating_sub(name + branch + state + 3 * COL_GAP);
    (name, branch, state, result)
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
        let (name, branch, state, result) = column_widths(&a, 80);
        assert!(name <= 20, "got name width {name}");
        assert_eq!(name + branch + state + result + 3 * COL_GAP, 80);
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
}
