use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::spinner;
use super::state::{AppState, RepoStatus};

pub fn draw(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    let max_name_len = state.repos.iter().map(|r| r.name.len()).max().unwrap_or(10);

    // Calculate visible area for repo list
    let list_height = area.height.saturating_sub(4) as usize; // header + 2 separators + footer

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

    // Repo rows
    let visible_end = state
        .total()
        .min(view_start + list_height.saturating_sub(expanded_rows));
    for i in view_start..visible_end {
        let is_selected = i == state.selected;
        let name = &state.repos[i].name;
        let status = &state.statuses[i];

        let (icon, icon_style, summ, summ_style) =
            format_status(status, state.tick, &state.command_name);

        let selector = if is_selected { "▸" } else { " " };
        let selector_style = if is_selected {
            Style::default().fg(Color::Cyan).bold()
        } else {
            Style::default()
        };

        let name_style = if is_selected {
            Style::default().bold()
        } else {
            Style::default()
        };

        let padding = max_name_len.saturating_sub(name.len()) + 2;

        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", selector), selector_style),
            Span::styled(icon, icon_style),
            Span::raw(" "),
            Span::styled(name.clone(), name_style),
            Span::raw(" ".repeat(padding)),
            Span::styled(summ, summ_style),
        ]));

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
        "  [↑↓/jk] navigate  [enter] expand  [q] quit"
    };
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

fn format_status(
    status: &RepoStatus,
    tick: usize,
    command_name: &str,
) -> (String, Style, String, Style) {
    match status {
        RepoStatus::Pending => (
            " ".into(),
            Style::default().fg(Color::DarkGray),
            "waiting...".into(),
            Style::default().fg(Color::DarkGray),
        ),
        RepoStatus::Running => (
            spinner::frame(tick).to_string(),
            Style::default().fg(Color::Yellow),
            running_text(command_name),
            Style::default().fg(Color::Yellow),
        ),
        RepoStatus::Done {
            summary, exit_code, ..
        } => {
            if *exit_code == 0 {
                (
                    "✓".into(),
                    Style::default().fg(Color::Green),
                    summary.clone(),
                    Style::default().fg(Color::DarkGray),
                )
            } else {
                (
                    "✗".into(),
                    Style::default().fg(Color::Red),
                    summary.clone(),
                    Style::default().fg(Color::Red),
                )
            }
        }
        RepoStatus::Skipped { reason } => (
            "-".into(),
            Style::default().fg(Color::DarkGray),
            reason.clone(),
            Style::default().fg(Color::DarkGray),
        ),
    }
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

fn running_text(command: &str) -> String {
    match command {
        "update" => "pulling...".into(),
        "status" => "checking...".into(),
        "diff" => "diffing...".into(),
        "push" => "pushing...".into(),
        "fetch" => "fetching...".into(),
        "checkout" => "cloning...".into(),
        "run" => "running...".into(),
        _ => "running...".into(),
    }
}
