//! The repo table in both its forms, full width and sidebar, and the filter
//! line that sits under it.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::footer::status_line;
use super::layout::{column_widths, list_height, list_start, sidebar_column_widths};
use super::{column_header, header_line, separator, BRANCH_LABEL, COL_GAP};
use super::{LIST_HEADER_ROWS, PREFIX_W};
use super::{REPO_LABEL, RESULT_LABEL, SIDEBAR_PREFIX_W, STATE_LABEL, SYNC_LABEL};
use crate::ui::app::probe;
use crate::ui::app::state::{App, RunStatus, Sort};
use crate::ui::widgets::{display_width, frame as spinner_frame, truncate};

pub(super) fn draw_list(frame: &mut Frame, app: &App, area: Rect, sidebar: bool) {
    let width = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    lines.push(header_line(app, width, sidebar));
    lines.push(Line::default());

    let visible = app.visible_indices();
    // As a sidebar the area already stops above the shared footer, so the body
    // is just what's left under the header. Either way the row count matches,
    // which is what lets click resolution use one formula for both.
    let lh = if sidebar {
        (area.height as usize).saturating_sub(LIST_HEADER_ROWS)
    } else {
        list_height(app, area.height)
    };
    let start = list_start(app, &visible, lh);
    let end = visible.len().min(start + lh);

    if sidebar {
        let (name_col, state_col) = sidebar_column_widths(app, width.saturating_sub(PREFIX_W));
        lines.push(column_label_line(
            SIDEBAR_PREFIX_W,
            &[
                (column_header(app, Sort::Repo, REPO_LABEL), name_col),
                (column_header(app, Sort::State, STATE_LABEL), state_col),
            ],
        ));
        lines.push(separator(width));
        for &idx in &visible[start..end] {
            lines.push(sidebar_repo_line(app, idx, name_col, state_col));
        }
    } else {
        let (name_col, branch_col, state_col, sync_col, result_col) =
            column_widths(app, width.saturating_sub(PREFIX_W));
        let mut columns = vec![
            (column_header(app, Sort::Repo, REPO_LABEL), name_col),
            (column_header(app, Sort::Branch, BRANCH_LABEL), branch_col),
            (column_header(app, Sort::State, STATE_LABEL), state_col),
        ];
        // A zero-width SYNC is one no row has a count for, and a header with
        // nothing under it reads as a claim that every repo is level.
        if sync_col > 0 {
            columns.push((column_header(app, Sort::Sync, SYNC_LABEL), sync_col));
        }
        columns.push((column_header(app, Sort::Result, RESULT_LABEL), result_col));
        lines.push(column_label_line(PREFIX_W, &columns));
        lines.push(separator(width));
        // Sized once for the whole table: the fields are what align the arrows
        // between rows, so they cannot be decided a row at a time.
        let sync_fields = app.sync_widths();
        for &idx in &visible[start..end] {
            lines.push(repo_line(
                app,
                idx,
                name_col,
                branch_col,
                state_col,
                (sync_col, sync_fields),
                result_col,
            ));
        }
    }

    if !sidebar {
        lines.push(separator(width));
        if app.filtering {
            lines.push(filter_line(app));
        }
        lines.push(status_line(app, width));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Column labels laid out on the same widths and gaps the data rows use, so
/// a label sits over its own column at every terminal width. Labels truncate
/// with their column rather than pushing the ones after them out of line.
fn column_label_line(prefix_w: usize, columns: &[(String, usize)]) -> Line<'static> {
    let style = Style::default().fg(Color::DarkGray).bold();
    let mut spans = vec![Span::raw(" ".repeat(prefix_w))];
    for (i, (label, col)) in columns.iter().enumerate() {
        let text = truncate(label, *col);
        // The last column has nothing after it to stay aligned with.
        if i + 1 < columns.len() {
            let padding = col.saturating_sub(display_width(&text)) + COL_GAP;
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(" ".repeat(padding)));
        } else {
            spans.push(Span::styled(text, style));
        }
    }
    Line::from(spans)
}

/// Whether the row is waiting on anything, a probe or a run alike: from the
/// outside both are just "this repo is busy".
fn is_busy(app: &App, idx: usize) -> bool {
    app.probing.contains(&idx)
        || matches!(
            app.run_results.get(idx).and_then(|r| r.as_ref()),
            Some(RunStatus::Running | RunStatus::Step { .. })
        )
}

/// The cell left of the repo name, where the one-shot progress view puts its
/// status icon. It normally holds the selection dot, and a busy row's spinner
/// borrows it: the spinner is always temporary, and one shared cell keeps the
/// gutter as narrow as the markers alone need.
fn select_cell(app: &App, idx: usize) -> Span<'static> {
    if is_busy(app, idx) {
        return Span::styled(
            format!("{} ", spinner_frame(app.tick)),
            Style::default().fg(Color::Yellow),
        );
    }
    if app.selected.contains(&idx) {
        Span::styled("● ", Style::default().fg(Color::Cyan).bold())
    } else {
        Span::raw("  ")
    }
}

fn repo_line(
    app: &App,
    idx: usize,
    name_col: usize,
    branch_col: usize,
    state_col: usize,
    (sync_col, sync_fields): (usize, (usize, usize)),
    result_col: usize,
) -> Line<'static> {
    let repo = &app.repos[idx];
    let is_cursor = idx == app.cursor;

    let cursor_marker = if is_cursor { "▸" } else { " " };
    let name_style = if is_cursor {
        Style::default().bold()
    } else {
        Style::default()
    };

    let name = truncate(&repo.name, name_col);
    let name_padding = name_col.saturating_sub(display_width(&name)) + COL_GAP;

    let probe = app.probe_display(idx);
    let branch = truncate(&probe.branch, branch_col);
    let branch_padding = branch_col.saturating_sub(display_width(&branch)) + COL_GAP;
    let state = truncate(&probe.state, state_col);
    let state_padding = state_col.saturating_sub(display_width(&state)) + COL_GAP;

    // A column the header does not draw contributes no gap either, or every
    // row's RESULT sits two cells right of the label over it.
    let (sync, sync_padding) = match sync_col {
        0 => (String::new(), 0),
        col => {
            let text = app.sync_text(idx, sync_fields);
            let padding = col.saturating_sub(display_width(&text)) + COL_GAP;
            (text, padding)
        }
    };

    let result_text = app.result_text(idx);
    let result_style = result_style(app, idx);
    let result = truncate(&result_text, result_col);

    Line::from(vec![
        Span::styled(
            format!(" {cursor_marker} "),
            Style::default().fg(Color::Cyan),
        ),
        select_cell(app, idx),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(name_padding)),
        Span::styled(branch, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(branch_padding)),
        Span::styled(state, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(state_padding)),
        Span::styled(sync, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(sync_padding)),
        Span::styled(result, result_style),
    ])
}

/// The result column's colour: green/red once a run has finished, yellow
/// while one is live, grey for a skip or a repo that has never run.
fn result_style(app: &App, idx: usize) -> Style {
    match app.run_results.get(idx).and_then(|r| r.as_ref()) {
        None | Some(RunStatus::Skipped { .. }) => Style::default().fg(Color::DarkGray),
        Some(RunStatus::Running | RunStatus::Step { .. }) => Style::default().fg(Color::Yellow),
        Some(RunStatus::Finished { exit_code, .. }) => {
            if *exit_code == 0 {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            }
        }
    }
}

/// The detail sidebar's row: name and one working-tree-state column, since
/// branch and ahead/behind are detail about a repo you're no longer scanning.
fn sidebar_repo_line(app: &App, idx: usize, name_col: usize, state_col: usize) -> Line<'static> {
    let repo = &app.repos[idx];
    let is_cursor = idx == app.cursor;
    let cursor_marker = if is_cursor { "▸" } else { " " };
    let name_style = if is_cursor {
        Style::default().bold()
    } else {
        Style::default()
    };
    let name = truncate(&repo.name, name_col);
    let name_padding = name_col.saturating_sub(display_width(&name)) + COL_GAP;

    // No selection dot here for the spinner to share, so it takes the state
    // cell instead, which a row this pane can only reach through the cursor
    // has usually already filled in.
    let state_text = match app.probes.get(idx).and_then(|p| p.as_ref()) {
        Some(state) => probe::dirty_text(state),
        None if is_busy(app, idx) => spinner_frame(app.tick).to_string(),
        None => String::new(),
    };
    let state = truncate(&state_text, state_col);

    Line::from(vec![
        Span::styled(
            format!(" {cursor_marker} "),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(name_padding)),
        Span::styled(state, Style::default().fg(Color::DarkGray)),
    ])
}

fn filter_line(app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled("  filter: ", Style::default().fg(Color::Yellow)),
        Span::raw(app.filter.clone()),
        Span::styled("▏", Style::default().fg(Color::Yellow)),
    ])
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;

    /// The row as it is drawn at `width`, at the same column widths `draw`
    /// would hand it.
    fn repo_line_at(a: &App, idx: usize, width: usize) -> Line<'static> {
        let (name, branch, state, sync, result) = column_widths(a, width - PREFIX_W);
        repo_line(a, idx, name, branch, state, (sync, a.sync_widths()), result)
    }

    fn row_at(a: &App, idx: usize, width: usize) -> String {
        flatten(&repo_line_at(a, idx, width))
    }

    /// Span offsets in a drawn row, so the assertions below name their cell
    /// instead of indexing past the padding spans between them.
    const MARKER: usize = 1;
    const BRANCH: usize = 4;
    const STATE: usize = 6;
    const SYNC: usize = 8;

    fn spinner(a: &App) -> String {
        format!("{} ", spinner_frame(a.tick))
    }

    /// The whole spinner contract in one pass, driven through the real probe
    /// lifecycle rather than by planting `probing` and `probes` by hand.
    #[test]
    fn the_spinner_contract_holds_from_begin_probe_through_to_the_drawn_row() {
        let mut a = app(vec![repo("alpha")]);
        let changes = probe::Changes {
            modified: 2,
            untracked: 0,
            deleted: 0,
        };

        let generation = a.begin_probe(&[0]);
        a.on_probe(generation, probed("main", changes, 0));
        let settled = repo_line_at(&a, 0, 155);
        assert_eq!(settled.spans[MARKER].content.as_ref(), "  ");
        assert_eq!(settled.spans[BRANCH].content.as_ref(), "main");
        assert_eq!(settled.spans[STATE].content.as_ref(), "2 modified");

        let generation = a.begin_probe(&[0]);
        let reprobing = repo_line_at(&a, 0, 155);
        assert_eq!(
            reprobing.spans[MARKER].content.as_ref(),
            spinner(&a),
            "the activity cell takes the spinner, got {:?}",
            flatten(&reprobing)
        );
        assert_eq!(
            reprobing.spans[BRANCH].content.as_ref(),
            "main",
            "BRANCH no longer gives its cell up, got {:?}",
            flatten(&reprobing)
        );
        assert_eq!(
            reprobing.spans[STATE].content.as_ref(),
            "2 modified",
            "STATE keeps the text state.rs handed it, got {:?}",
            flatten(&reprobing)
        );

        a.on_probe(generation, probed("main", changes, 0));
        let done = repo_line_at(&a, 0, 155);
        assert_eq!(
            done.spans[MARKER].content.as_ref(),
            "  ",
            "the spinner stops once the result lands"
        );
    }

    /// The reason the cell exists: a run is the longer wait of the two, and
    /// before this it was announced by static text alone.
    #[test]
    fn a_row_with_a_run_in_flight_spins_the_same_way_a_probe_does() {
        let mut a = app(vec![repo("alpha")]);
        a.run_results[0] = Some(RunStatus::Running);
        assert_eq!(
            repo_line_at(&a, 0, 155).spans[MARKER].content.as_ref(),
            spinner(&a)
        );

        a.run_results[0] = Some(RunStatus::Step {
            label: "git pull".into(),
        });
        assert_eq!(
            repo_line_at(&a, 0, 155).spans[MARKER].content.as_ref(),
            spinner(&a)
        );

        a.run_results[0] = Some(RunStatus::Finished {
            steps: vec![],
            exit_code: 0,
        });
        assert_eq!(
            repo_line_at(&a, 0, 155).spans[MARKER].content.as_ref(),
            "  ",
            "a finished run has nothing left to wait on"
        );
    }

    /// The spinner and the selection dot share one cell, so a selected row
    /// that starts working must get its dot back when it stops.
    #[test]
    fn a_busy_row_borrows_the_selection_dots_cell_and_gives_it_back() {
        let mut a = app(vec![repo("alpha")]);
        a.selected.insert(0);
        assert_eq!(
            repo_line_at(&a, 0, 155).spans[MARKER].content.as_ref(),
            "● "
        );

        a.probing.insert(0);
        assert_eq!(
            repo_line_at(&a, 0, 155).spans[MARKER].content.as_ref(),
            spinner(&a),
            "the spinner is the more urgent of the two"
        );

        a.probing.remove(&0);
        assert_eq!(
            repo_line_at(&a, 0, 155).spans[MARKER].content.as_ref(),
            "● "
        );
    }

    /// Whatever the cell is holding it is the row's only moving part, so it
    /// has to be the same width in all three states or the name column jitters.
    #[test]
    fn the_marker_cell_is_the_same_width_selected_spinning_or_idle() {
        let mut a = app(vec![repo("alpha")]);
        let idle = display_width(&flatten(&repo_line_at(&a, 0, 155)));

        a.selected.insert(0);
        assert_eq!(idle, display_width(&flatten(&repo_line_at(&a, 0, 155))));

        a.probing.insert(0);
        assert_eq!(idle, display_width(&flatten(&repo_line_at(&a, 0, 155))));
    }

    #[test]
    fn the_ahead_counter_survives_a_state_column_narrower_than_its_row() {
        let mut a = app(vec![repo("alpha")]);
        a.probes[0] = Some(probed(
            "main",
            probe::Changes {
                modified: 12,
                untracked: 34,
                deleted: 1,
            },
            1,
        ));

        let row = row_at(&a, 0, 155);
        assert!(
            row.contains("↑1"),
            "the ahead counter must not be the first thing truncation loses, got {row:?}"
        );
        assert!(
            row.contains("12 modified"),
            "the working-tree text is what gives up cells, got {row:?}"
        );
    }

    /// A long state text must not buy itself room the column does not have,
    /// or every column after STATE shifts right.
    #[test]
    fn a_row_keeps_its_state_and_sync_cells_inside_their_columns() {
        let mut a = app(vec![repo("alpha")]);
        a.probes[0] = Some(probed(
            "main",
            probe::Changes {
                modified: 12,
                untracked: 34,
                deleted: 1,
            },
            1,
        ));

        for width in 10..=200 {
            let (name, branch, state_col, sync_col, result) = column_widths(&a, width - PREFIX_W);
            let row = repo_line(
                &a,
                0,
                name,
                branch,
                state_col,
                (sync_col, a.sync_widths()),
                result,
            );
            for (label, at, col) in [("STATE", STATE, state_col), ("SYNC", SYNC, sync_col)] {
                let cell = row.spans[at].content.as_ref();
                assert!(
                    display_width(cell) <= col,
                    "width {width}: {label} cell {cell:?} overflows its {col} cells"
                );
            }
        }
    }

    #[test]
    fn every_column_label_starts_where_its_data_starts() {
        let mut a = app(vec![repo("bill-api"), repo("menu-api")]);
        a.probes[0] = Some(crate::ui::app::probe::RepoState {
            index: 0,
            branch: Some("master".into()),
            upstream: Some("origin/master".into()),
            ahead: 2,
            behind: 3,
            changed: 0,
            changes: probe::Changes::default(),
            present: true,
            timed_out: false,
            fetched: true,
            fetch_head: None,
        });
        a.fetched_repos.insert(0);
        let (name, branch, state, sync, result) = column_widths(&a, 80 - PREFIX_W);
        let labels = flatten(&column_label_line(
            PREFIX_W,
            &[
                (REPO_LABEL.into(), name),
                (BRANCH_LABEL.into(), branch),
                (STATE_LABEL.into(), state),
                (SYNC_LABEL.into(), sync),
                (RESULT_LABEL.into(), result),
            ],
        ));
        let row = flatten(&repo_line(
            &a,
            0,
            name,
            branch,
            state,
            (sync, a.sync_widths()),
            result,
        ));

        assert_eq!(
            col_of(&labels, REPO_LABEL),
            col_of(&row, "bill-api"),
            "REPO sits over the repo name"
        );
        assert_eq!(
            col_of(&labels, BRANCH_LABEL),
            col_of(&row, "master"),
            "BRANCH sits over the branch"
        );
        assert_eq!(
            col_of(&labels, STATE_LABEL),
            col_of(&row, "clean"),
            "STATE sits over the working-tree state"
        );
        assert_eq!(
            col_of(&labels, SYNC_LABEL),
            col_of(&row, "↑2"),
            "SYNC sits over the distance from upstream"
        );
    }

    /// A set where nothing is ahead or behind draws no SYNC column at all, and
    /// the row has to drop the gap with it or every RESULT lands two cells
    /// right of the label over it.
    #[test]
    fn a_set_with_no_distance_to_report_draws_neither_the_column_nor_its_gap() {
        let mut a = app(vec![repo("bill-api")]);
        a.probes[0] = Some(probed("main", probe::Changes::default(), 0));

        let rows = frame_rows(&a, 80, 10);
        let labels = rows
            .iter()
            .find(|row| row.contains(REPO_LABEL))
            .expect("the column labels are drawn");
        let row = rows
            .iter()
            .find(|row| row.contains("bill-api"))
            .expect("the repo is drawn");

        assert!(!labels.contains(SYNC_LABEL), "got {labels:?}");
        assert_eq!(
            col_of(labels, RESULT_LABEL),
            col_of(row, "·"),
            "RESULT still sits over its own column, got {row:?}"
        );
    }

    /// The header line that says which way the table reads, and the one
    /// place a wrong answer is invisible: every other row looks the same
    /// whatever the order is.
    #[test]
    fn the_sorted_columns_header_carries_the_arrow_and_no_other_does() {
        let mut a = app(vec![repo("alpha")]);
        a.choose_sort(Sort::Branch);
        let header = frame_rows(&a, 100, 12)
            .into_iter()
            .find(|row| row.contains(REPO_LABEL))
            .expect("the column labels are drawn");

        assert!(header.contains("BRANCH ↑"), "got {header:?}");
        for label in [REPO_LABEL, STATE_LABEL, RESULT_LABEL] {
            assert!(!header.contains(&format!("{label} ↑")), "got {header:?}");
            assert!(!header.contains(&format!("{label} ↓")), "got {header:?}");
        }
    }

    #[test]
    fn a_column_label_truncates_with_its_column_instead_of_shifting_the_next_one() {
        let line = column_label_line(0, &[("BRANCH".into(), 3), ("STATE".into(), 5)]);
        let text = flatten(&line);
        assert_eq!(
            display_width(&text[..text.find("  ").unwrap()]),
            3,
            "the label fits its column, got {text:?}"
        );
        assert_eq!(
            col_of(&text, "STATE"),
            Some(3 + COL_GAP),
            "the next label keeps its own offset, got {text:?}"
        );
    }
}
