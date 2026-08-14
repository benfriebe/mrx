//! Layout for the resident app: header, repo table, and status bar. Branch
//! and working-tree state come from the background probe; the result column
//! arrives with the executor in a later phase.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::state::App;
use crate::ui::widgets::{display_width, frame as spinner_frame, truncate};

const COL_GAP: usize = 2;
/// Width of the leading " ▸ ● " cursor and selection markers.
const PREFIX_W: usize = 5;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    lines.push(header_line(app, width));
    lines.push(separator(width));

    let visible = app.visible_indices();
    let chrome_rows = if app.filtering { 5 } else { 4 };
    let list_height = (area.height as usize).saturating_sub(chrome_rows);
    let cursor_pos = visible.iter().position(|&i| i == app.cursor).unwrap_or(0);
    let start = scroll_offset(cursor_pos, visible.len(), list_height);
    let end = visible.len().min(start + list_height);

    let (name_col, branch_col, state_col) = column_widths(app, width.saturating_sub(PREFIX_W));
    for &idx in &visible[start..end] {
        lines.push(repo_line(app, idx, name_col, branch_col, state_col));
    }

    lines.push(separator(width));
    if app.filtering {
        lines.push(filter_line(app));
    }
    lines.push(status_line(app));

    frame.render_widget(Paragraph::new(lines), area);
}

fn header_line(app: &App, width: usize) -> Line<'static> {
    let title = format!("  mrx · {}", app.set_label);
    let counts = format!(
        "{} repos · {} selected  ",
        app.repos.len(),
        app.effective_selection().len()
    );
    let gap = width.saturating_sub(display_width(&title) + display_width(&counts));
    Line::from(vec![
        Span::styled(title, Style::default().bold()),
        Span::raw(" ".repeat(gap)),
        Span::styled(counts, Style::default().fg(Color::DarkGray)),
    ])
}

fn separator(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(Color::DarkGray),
    ))
}

fn repo_line(
    app: &App,
    idx: usize,
    name_col: usize,
    branch_col: usize,
    state_col: usize,
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
    let (branch_text, state_text) = if probe.spinner {
        (spinner_frame(app.tick).to_string(), String::new())
    } else {
        (probe.branch, probe.state)
    };
    let branch = truncate(&branch_text, branch_col);
    let branch_padding = branch_col.saturating_sub(display_width(&branch)) + COL_GAP;
    let state = truncate(&state_text, state_col);

    Line::from(vec![
        Span::styled(
            format!(" {} {} ", cursor_marker, select_marker),
            marker_style,
        ),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(name_padding)),
        Span::styled(branch, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(branch_padding)),
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

fn status_line(app: &App) -> Line<'static> {
    let keys = if app.filtering {
        "  esc clear  enter keep"
    } else {
        "  j/k move  g/G top/bottom  space select  a all  A none  i invert  / filter  r reprobe  q quit"
    };
    Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray)))
}

/// Column widths for the three-column repo table: NAME and BRANCH each get
/// their natural width up to a third of `avail`, STATE gets whatever is left.
fn column_widths(app: &App, avail: usize) -> (usize, usize, usize) {
    let name_nat = app
        .repos
        .iter()
        .map(|r| display_width(&r.name))
        .max()
        .unwrap_or(0)
        .max(display_width("NAME"));
    let branch_nat = (0..app.repos.len())
        .map(|i| display_width(&app.probe_display(i).branch))
        .max()
        .unwrap_or(0)
        .max(display_width("BRANCH"));

    let name = name_nat.min(avail / 3);
    let branch = branch_nat.min(avail / 3);
    let state = avail.saturating_sub(name + branch + 2 * COL_GAP);
    (name, branch, state)
}

/// First visible-list index to draw, so the cursor stays on screen once it
/// scrolls past the bottom of `list_height` rows.
fn scroll_offset(cursor_pos: usize, visible_len: usize, list_height: usize) -> usize {
    if list_height == 0 || visible_len == 0 {
        return 0;
    }
    if cursor_pos < list_height {
        0
    } else {
        cursor_pos - list_height + 1
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

    #[test]
    fn scroll_offset_stays_zero_while_the_cursor_fits_on_screen() {
        assert_eq!(scroll_offset(0, 10, 5), 0);
        assert_eq!(scroll_offset(4, 10, 5), 0);
    }

    #[test]
    fn scroll_offset_follows_the_cursor_past_the_bottom() {
        assert_eq!(scroll_offset(5, 10, 5), 1);
        assert_eq!(scroll_offset(9, 10, 5), 5);
    }

    #[test]
    fn scroll_offset_handles_an_empty_list() {
        assert_eq!(scroll_offset(0, 0, 5), 0);
    }

    #[test]
    fn name_column_never_exceeds_a_third_of_the_available_width() {
        let long_name = "a-name-that-is-extremely-long-past-any-reasonable-column-width";
        let app = App::new(vec![repo(long_name)], "work".into(), 4);
        let (name, branch, state) = column_widths(&app, 60);
        assert!(name <= 20, "got name width {name}");
        assert_eq!(name + branch + state + 2 * COL_GAP, 60);
    }
}
