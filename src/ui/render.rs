use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::state::AppState;
use super::widgets::{self, Columns, RepoRow};

pub fn draw(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    // Natural widths (longest content) for each column, floored to header label width.
    let name_nat = state
        .repos
        .iter()
        .map(|r| widgets::display_width(&r.name))
        .max()
        .unwrap_or(0)
        .max(widgets::display_width("REPO"));
    let branch_nat = (0..state.total())
        .map(|i| widgets::display_width(state.branch_label(i)))
        .max()
        .unwrap_or(0)
        .max(widgets::display_width("BRANCH"));
    let status_nat = (0..state.total())
        .map(|i| {
            let (_, _, summ, _) =
                widgets::format_status(&state.statuses[i], state.tick, &state.command_name);
            widgets::display_width(&summ)
        })
        .max()
        .unwrap_or(0)
        .max(widgets::display_width("STATUS"));

    // Row prefix is "  ▸ ✓ " (2 spaces + selector + space + icon + space) = 6 cells.
    // Two 2-space gaps separate the three columns.
    const PREFIX_W: usize = 6;
    const COL_GAP: usize = 2;
    let avail = (area.width as usize).saturating_sub(PREFIX_W + COL_GAP * 2);
    let (name_col, branch_col, status_col) =
        compute_column_widths(name_nat, branch_nat, status_nat, avail);

    // Calculate visible area for repo list
    let list_height = area.height.saturating_sub(5) as usize; // header + 2 separators + column headers + footer

    // Determine scroll window
    let (view_start, expanded_rows) = calculate_scroll(state, list_height);

    let mut lines: Vec<Line> = Vec::new();

    // Header
    let summary = state.summary_line();
    let title = format!("  mrx {}", state.command_name);
    let gap = (area.width as usize).saturating_sub(title.len() + summary.len());
    lines.push(Line::from(vec![
        Span::styled(&title, Style::default().bold()),
        Span::raw(" ".repeat(gap)),
        Span::styled(&summary, Style::default().fg(Color::DarkGray)),
    ]));

    // Separator
    lines.push(Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(Color::DarkGray),
    )));

    // Column headers (labels truncate alongside their column on narrow viewports)
    let repo_label = widgets::truncate("REPO", name_col);
    let branch_label = widgets::truncate("BRANCH", branch_col);
    let status_label = widgets::truncate("STATUS", status_col);
    let header_name_padding =
        name_col.saturating_sub(widgets::display_width(&repo_label)) + COL_GAP;
    let header_branch_padding =
        branch_col.saturating_sub(widgets::display_width(&branch_label)) + COL_GAP;
    let header_style = Style::default().fg(Color::DarkGray).bold();
    lines.push(Line::from(vec![
        Span::raw("      "),
        Span::styled(repo_label, header_style),
        Span::raw(" ".repeat(header_name_padding)),
        Span::styled(branch_label, header_style),
        Span::raw(" ".repeat(header_branch_padding)),
        Span::styled(status_label, header_style),
    ]));

    // Repo rows
    let visible_end = state
        .total()
        .min(view_start + list_height.saturating_sub(expanded_rows));
    let columns = Columns {
        name: name_col,
        branch: branch_col,
        status: status_col,
    };
    for i in view_start..visible_end {
        let is_selected = i == state.selected;
        let name = &state.repos[i].name;
        let status = &state.statuses[i];

        let (icon, icon_style, summ, summ_style) =
            widgets::format_status(status, state.tick, &state.command_name);

        lines.push(widgets::repo_row(
            &RepoRow {
                name,
                branch: state.branch_label(i),
                icon: &icon,
                icon_style,
                summary: &summ,
                summary_style: summ_style,
                selected: is_selected,
            },
            &columns,
        ));

        // Expanded content right after the selected row
        if state.expanded == Some(i) {
            if let Some(content) = state.expanded_content() {
                let content_lines: Vec<&str> = content.lines().collect();
                let max_visible = list_height.saturating_sub(3).max(3);
                let start = state
                    .scroll_offset
                    .min(content_lines.len().saturating_sub(1));
                let end = (start + max_visible).min(content_lines.len());

                let box_width = area.width.saturating_sub(6) as usize;

                // Top border
                lines.push(Line::from(Span::styled(
                    format!("    ┌{}┐", "─".repeat(box_width)),
                    Style::default().fg(Color::DarkGray),
                )));

                for cl in &content_lines[start..end] {
                    let truncated: String = cl.chars().take(box_width.saturating_sub(2)).collect();
                    lines.push(Line::from(vec![
                        Span::styled("    │ ", Style::default().fg(Color::DarkGray)),
                        Span::raw(truncated),
                    ]));
                }

                // Bottom border (with scroll indicator if needed)
                if content_lines.len() > max_visible {
                    let indicator = format!(" [{}-{}/{}] ", start + 1, end, content_lines.len());
                    let dash_len = box_width.saturating_sub(indicator.len());
                    lines.push(Line::from(Span::styled(
                        format!("    └{}{}┘", "─".repeat(dash_len), indicator),
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        format!("    └{}┘", "─".repeat(box_width)),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        }
    }

    // Separator
    lines.push(Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(Color::DarkGray),
    )));

    // Footer
    let footer = if state.expanded.is_some() {
        "  [↑↓] scroll  [esc] collapse  [q] quit"
    } else {
        "  [↑↓/jk] navigate  [enter] expand  [r] re-run  [q] quit"
    };
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

fn calculate_scroll(state: &AppState, list_height: usize) -> (usize, usize) {
    let expanded_rows = if state.expanded.is_some() {
        if let Some(content) = state.expanded_content() {
            let content_lines = content.lines().count();
            let max_visible = list_height.saturating_sub(3).max(3);
            content_lines.min(max_visible) + 2 // +2 for borders
        } else {
            0
        }
    } else {
        0
    };

    let effective_height = list_height.saturating_sub(expanded_rows);

    let view_start = if state.selected < effective_height {
        0
    } else {
        state.selected - effective_height + 1
    };

    (view_start, expanded_rows)
}

/// Allocate widths for the REPO, BRANCH, and STATUS columns to fit `avail`.
/// When natural widths exceed the budget, shrink BRANCH first (long branch names
/// are the common offender), then STATUS, finally REPO. Each column has a soft
/// floor equal to its header label width so the header reads cleanly. On very
/// narrow viewports where even the floors do not fit, a final pass shrinks
/// columns below their floors (BRANCH, then STATUS, then REPO) so the row never
/// overflows the viewport.
fn compute_column_widths(
    name_nat: usize,
    branch_nat: usize,
    status_nat: usize,
    avail: usize,
) -> (usize, usize, usize) {
    let name_min = widgets::display_width("REPO");
    let branch_min = widgets::display_width("BRANCH");
    let status_min = widgets::display_width("STATUS");

    let total = name_nat + branch_nat + status_nat;
    if total <= avail {
        return (name_nat, branch_nat, status_nat);
    }

    let mut name = name_nat;
    let mut branch = branch_nat;
    let mut status = status_nat;

    // Pass 1: shrink toward floors in priority order (branch, status, name).
    let over = total - avail;
    let give = (branch.saturating_sub(branch_min)).min(over);
    branch -= give;

    let total = name + branch + status;
    if total > avail {
        let over = total - avail;
        let give = (status.saturating_sub(status_min)).min(over);
        status -= give;
    }

    let total = name + branch + status;
    if total > avail {
        let over = total - avail;
        let give = (name.saturating_sub(name_min)).min(over);
        name -= give;
    }

    // Pass 2: viewport is narrower than the sum of floors. Shrink unconditionally
    // (down to 0) so the row fits, even if header labels truncate.
    let total = name + branch + status;
    if total > avail {
        let mut over = total - avail;
        let give = branch.min(over);
        branch -= give;
        over -= give;
        if over > 0 {
            let give = status.min(over);
            status -= give;
            over -= give;
        }
        if over > 0 {
            let give = name.min(over);
            name -= give;
        }
    }

    (name, branch, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_widths_used_when_they_fit() {
        let (n, b, s) = compute_column_widths(10, 20, 15, 100);
        assert_eq!((n, b, s), (10, 20, 15));
    }

    #[test]
    fn shrinks_branch_first_when_over_budget() {
        // total nat = 60, avail = 40 → branch absorbs the full 20 over.
        let (n, b, s) = compute_column_widths(10, 40, 10, 40);
        assert_eq!((n, b, s), (10, 20, 10));
    }

    #[test]
    fn shrinks_status_after_branch_floor() {
        // total nat = 50, avail = 16. branch shrinks 30→6 (floor), still 6 over,
        // status shrinks 10→4… wait status_min = 6, so status stays 6 and name absorbs.
        // name_nat=10 → name_min=4, name absorbs 6: 4 + 6 + 6 = 16. ✓
        let (n, b, s) = compute_column_widths(10, 30, 10, 16);
        assert_eq!(n + b + s, 16);
        assert_eq!(b, 6); // floored at "BRANCH"
        assert_eq!(s, 6); // floored at "STATUS"
        assert_eq!(n, 4); // floored at "REPO"
    }

    #[test]
    fn fits_below_floor_sum_on_narrow_viewports() {
        // avail = 10, sum of floors = 16 → must shrink unconditionally.
        let (n, b, s) = compute_column_widths(20, 30, 20, 10);
        assert!(n + b + s <= 10, "got {} {} {}", n, b, s);
    }

    #[test]
    fn fits_zero_avail() {
        let (n, b, s) = compute_column_widths(10, 20, 15, 0);
        assert_eq!((n, b, s), (0, 0, 0));
    }
}
