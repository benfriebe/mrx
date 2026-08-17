//! The repo table in both its forms, full width and sidebar, and the filter
//! line that sits under it.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::footer::status_line;
use super::layout::{column_widths, list_height, list_start, sidebar_column_widths};
use super::{header_line, separator, BRANCH_LABEL, COL_GAP, LIST_HEADER_ROWS, PREFIX_W};
use super::{REPO_LABEL, RESULT_LABEL, SIDEBAR_PREFIX_W, STATE_LABEL};
use crate::ui::app::probe;
use crate::ui::app::state::{App, RunStatus};
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
            &[(REPO_LABEL, name_col), (STATE_LABEL, state_col)],
        ));
        lines.push(separator(width));
        for &idx in &visible[start..end] {
            lines.push(sidebar_repo_line(app, idx, name_col, state_col));
        }
    } else {
        let (name_col, branch_col, state_col, result_col) =
            column_widths(app, width.saturating_sub(PREFIX_W));
        lines.push(column_label_line(
            PREFIX_W,
            &[
                (REPO_LABEL, name_col),
                (BRANCH_LABEL, branch_col),
                (STATE_LABEL, state_col),
                (RESULT_LABEL, result_col),
            ],
        ));
        lines.push(separator(width));
        for &idx in &visible[start..end] {
            lines.push(repo_line(
                app, idx, name_col, branch_col, state_col, result_col,
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
fn column_label_line(prefix_w: usize, columns: &[(&str, usize)]) -> Line<'static> {
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

/// The one-cell spinner column that sits between the markers and the repo
/// name, matching where the one-shot progress view puts its status icon. It
/// spins for anything the row is waiting on, a probe or a run alike, since
/// from the outside both are just "this repo is busy". Blank otherwise, so
/// the name column never moves.
fn activity_cell(app: &App, idx: usize) -> Span<'static> {
    let running = matches!(
        app.run_results.get(idx).and_then(|r| r.as_ref()),
        Some(RunStatus::Running | RunStatus::Step { .. })
    );
    if app.probing.contains(&idx) || running {
        Span::styled(
            format!("{} ", spinner_frame(app.tick)),
            Style::default().fg(Color::Yellow),
        )
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
    result_col: usize,
) -> Line<'static> {
    let repo = &app.repos[idx];
    let is_cursor = idx == app.cursor;
    let is_selected = app.selected.contains(&idx);

    let cursor_marker = if is_cursor { "▸" } else { " " };
    let select_marker = if is_selected { "●" } else { " " };
    let marker_style = if is_selected {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default().fg(Color::Cyan)
    };
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
    let state = fit_state(&probe.state, state_col);
    let state_padding = state_col.saturating_sub(display_width(&state)) + COL_GAP;

    let result_text = app.result_text(idx);
    let result_style = result_style(app, idx);
    let result = truncate(&result_text, result_col);

    Line::from(vec![
        Span::styled(
            format!(" {} {} ", cursor_marker, select_marker),
            marker_style,
        ),
        activity_cell(app, idx),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(name_padding)),
        Span::styled(branch, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(branch_padding)),
        Span::styled(state, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(state_padding)),
        Span::styled(result, result_style),
    ])
}

/// STATE's working-tree half and its trailing `↑n`/`↓n` counters, which
/// [`probe::dirty_text`] appends after a gap. Splitting on the arrows is safe
/// because nothing else the column can hold uses them.
fn split_counters(text: &str) -> (&str, &str) {
    match text.find(['↑', '↓']) {
        Some(at) => {
            let head = text[..at].trim_end();
            (head, &text[head.len()..])
        }
        None => (text, ""),
    }
}

/// STATE fitted to `max` cells with the ahead/behind counters kept, so the
/// working-tree text absorbs the truncation instead. A missing `↓` is how the
/// row says nothing has fetched this repo yet (the help notes say so), and a
/// naive truncation, which drops the trailing counters first, makes it lie.
fn fit_state(text: &str, max: usize) -> String {
    let (head, counters) = split_counters(text);
    match max.checked_sub(display_width(counters)) {
        // No room for the counters and any working-tree text, so truncate both.
        None | Some(0) => truncate(text, max),
        Some(head_max) => format!("{}{}", truncate(head, head_max), counters),
    }
}

/// The result column's colour: green/red once a run has finished, yellow
/// while one is live, grey for a skip or a repo that has never run.
fn result_style(app: &App, idx: usize) -> Style {
    match app.run_results.get(idx).and_then(|r| r.as_ref()) {
        None | Some(RunStatus::Skipped { .. }) => Style::default().fg(Color::DarkGray),
        Some(RunStatus::Running) | Some(RunStatus::Step { .. }) => {
            Style::default().fg(Color::Yellow)
        }
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

    let state_text = match app.probes.get(idx).and_then(|p| p.as_ref()) {
        Some(state) => probe::dirty_text_brief(state),
        None => String::new(),
    };
    let state = truncate(&state_text, state_col);

    Line::from(vec![
        Span::styled(
            format!(" {} ", cursor_marker),
            Style::default().fg(Color::Cyan),
        ),
        activity_cell(app, idx),
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
        let (name, branch, state, result) = column_widths(a, width - PREFIX_W);
        repo_line(a, idx, name, branch, state, result)
    }

    fn row_at(a: &App, idx: usize, width: usize) -> String {
        flatten(&repo_line_at(a, idx, width))
    }

    /// Span offsets in a drawn row, so the assertions below name their cell
    /// instead of indexing past the padding spans between them.
    const ACTIVITY: usize = 1;
    const BRANCH: usize = 4;
    const STATE: usize = 6;

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
        assert_eq!(settled.spans[ACTIVITY].content.as_ref(), "  ");
        assert_eq!(settled.spans[BRANCH].content.as_ref(), "main");
        assert_eq!(settled.spans[STATE].content.as_ref(), "2 modified");

        let generation = a.begin_probe(&[0]);
        let reprobing = repo_line_at(&a, 0, 155);
        assert_eq!(
            reprobing.spans[ACTIVITY].content.as_ref(),
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
            done.spans[ACTIVITY].content.as_ref(),
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
            repo_line_at(&a, 0, 155).spans[ACTIVITY].content.as_ref(),
            spinner(&a)
        );

        a.run_results[0] = Some(RunStatus::Step {
            label: "git pull".into(),
        });
        assert_eq!(
            repo_line_at(&a, 0, 155).spans[ACTIVITY].content.as_ref(),
            spinner(&a)
        );

        a.run_results[0] = Some(RunStatus::Finished {
            steps: vec![],
            exit_code: 0,
        });
        assert_eq!(
            repo_line_at(&a, 0, 155).spans[ACTIVITY].content.as_ref(),
            "  ",
            "a finished run has nothing left to wait on"
        );
    }

    /// The cell is the row's only moving part, so it has to be exactly as
    /// wide when idle as when spinning or the name column jitters.
    #[test]
    fn the_activity_cell_is_the_same_width_spinning_or_idle() {
        let mut a = app(vec![repo("alpha")]);
        let idle = display_width(&flatten(&repo_line_at(&a, 0, 155)));
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

    /// Keeping the counters must not buy them room the column does not have,
    /// or every column after STATE shifts right.
    #[test]
    fn a_row_with_counters_keeps_its_state_cell_inside_its_column() {
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
            let (name, branch, state_col, result) = column_widths(&a, width - PREFIX_W);
            let row = repo_line(&a, 0, name, branch, state_col, result);
            let cell = row.spans[STATE].content.as_ref();
            assert!(
                display_width(cell) <= state_col,
                "width {width}: STATE cell {cell:?} overflows its {state_col} cells"
            );
        }
    }

    #[test]
    fn every_column_label_starts_where_its_data_starts() {
        let mut a = app(vec![repo("bill-api"), repo("menu-api")]);
        a.probes[0] = Some(crate::ui::app::probe::RepoState {
            index: 0,
            branch: Some("master".into()),
            upstream: Some("origin/master".into()),
            ahead: 0,
            behind: 0,
            changed: 0,
            changes: Default::default(),
            present: true,
            timed_out: false,
            fetched: true,
            fetch_head: None,
        });
        let (name, branch, state, result) = column_widths(&a, 80 - PREFIX_W);
        let labels = flatten(&column_label_line(
            PREFIX_W,
            &[
                (REPO_LABEL, name),
                (BRANCH_LABEL, branch),
                (STATE_LABEL, state),
                (RESULT_LABEL, result),
            ],
        ));
        let row = flatten(&repo_line(&a, 0, name, branch, state, result));

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
    }

    #[test]
    fn a_column_label_truncates_with_its_column_instead_of_shifting_the_next_one() {
        let line = column_label_line(0, &[("BRANCH", 3), ("STATE", 5)]);
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
