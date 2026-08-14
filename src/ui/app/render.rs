//! Layout for the resident app: header, repo table, and status bar. This
//! phase's table shows name and path only; the branch, dirty, and result
//! columns arrive with the probe and the executor in later phases.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::state::App;
use crate::ui::widgets::{display_width, truncate};

const COL_GAP: usize = 2;

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

    let (name_col, path_col) = column_widths(app, width.saturating_sub(4 + COL_GAP));
    for &idx in &visible[start..end] {
        lines.push(repo_line(app, idx, name_col, path_col));
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

fn repo_line(app: &App, idx: usize, name_col: usize, path_col: usize) -> Line<'static> {
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
    let path = truncate(&repo.path.display().to_string(), path_col);

    Line::from(vec![
        Span::styled(
            format!(" {} {} ", cursor_marker, select_marker),
            marker_style,
        ),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(name_padding)),
        Span::styled(path, Style::default().fg(Color::DarkGray)),
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
        "  j/k move  g/G top/bottom  space select  a all  A none  i invert  / filter  q quit"
    };
    Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray)))
}

/// Column widths for the two-column repo table: NAME gets its natural width
/// up to half of `avail`, PATH gets whatever is left.
fn column_widths(app: &App, avail: usize) -> (usize, usize) {
    let name_nat = app
        .repos
        .iter()
        .map(|r| display_width(&r.name))
        .max()
        .unwrap_or(0)
        .max(display_width("NAME"));
    let name = name_nat.min(avail / 2);
    let path = avail.saturating_sub(name + COL_GAP);
    (name, path)
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
    fn name_column_never_exceeds_half_the_available_width() {
        let long_name = "a-name-that-is-extremely-long-past-any-reasonable-column-width";
        let app = App::new(vec![repo(long_name)], "work".into(), 4);
        let (name, path) = column_widths(&app, 40);
        assert!(name <= 20, "got name width {name}");
        assert_eq!(name + path + COL_GAP, 40);
    }
}
