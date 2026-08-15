//! Widgets shared between every view that draws a repo table: the row itself
//! and the spinner that animates a running one. Keeping this in one place is
//! what stops the one-shot view and a future resident app from drawing the
//! same repo two different ways.

use ratatui::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::state::RepoStatus;

const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn frame(tick: usize) -> char {
    FRAMES[tick % FRAMES.len()]
}

pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate `s` to fit in `max` display cells, appending `…` when shortened.
/// Width-aware: a single CJK or emoji glyph counts as 2 cells.
pub fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if display_width(s) <= max {
        return s.to_string();
    }
    if max == 1 {
        return "…".into();
    }
    let target = max - 1; // reserve 1 cell for the ellipsis
    let mut acc = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if acc + w > target {
            break;
        }
        out.push(ch);
        acc += w;
    }
    out.push('…');
    out
}

fn running_text(command: &str) -> String {
    match command {
        "update" => "pulling...".into(),
        "status" => "checking...".into(),
        "diff" => "diffing...".into(),
        "push" => "pushing...".into(),
        "fetch" => "fetching...".into(),
        "checkout" => "cloning...".into(),
        _ => "running...".into(),
    }
}

/// Icon, icon style, summary text, and summary style for a repo's current
/// status. Shared so a row means the same thing wherever it is drawn.
pub fn format_status(
    status: &RepoStatus,
    tick: usize,
    command_name: &str,
) -> (String, Style, String, Style) {
    match status {
        RepoStatus::Pending => (
            " ".into(),
            Style::default().fg(Color::Gray),
            "waiting...".into(),
            Style::default().fg(Color::Gray),
        ),
        RepoStatus::Running => (
            frame(tick).to_string(),
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

/// Column widths for the three-column repo table.
pub struct Columns {
    pub name: usize,
    pub branch: usize,
    pub status: usize,
}

/// Everything one repo row needs to draw itself: the status icon and summary
/// already resolved by [`format_status`], plus the identifying columns.
pub struct RepoRow<'a> {
    pub name: &'a str,
    pub branch: &'a str,
    pub icon: &'a str,
    pub icon_style: Style,
    pub summary: &'a str,
    pub summary_style: Style,
    pub selected: bool,
}

/// One repo row: selection marker, status icon, name, branch, and summary,
/// each truncated to fit `columns`. Every view draws a repo through this so
/// they can't drift apart.
pub fn repo_row(row: &RepoRow, columns: &Columns) -> Line<'static> {
    const COL_GAP: usize = 2;

    let selector = if row.selected { "▸" } else { " " };
    let selector_style = if row.selected {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default()
    };
    let name_style = if row.selected {
        Style::default().bold()
    } else {
        Style::default()
    };

    let name_disp = truncate(row.name, columns.name);
    let name_padding = columns.name.saturating_sub(display_width(&name_disp)) + COL_GAP;
    let branch_disp = truncate(row.branch, columns.branch);
    let branch_padding = columns.branch.saturating_sub(display_width(&branch_disp)) + COL_GAP;
    let summ_disp = truncate(row.summary, columns.status);

    Line::from(vec![
        Span::styled(format!("  {} ", selector), selector_style),
        Span::styled(row.icon.to_string(), row.icon_style),
        Span::raw(" "),
        Span::styled(name_disp, name_style),
        Span::raw(" ".repeat(name_padding)),
        Span::styled(branch_disp, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(branch_padding)),
        Span::styled(summ_disp, row.summary_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_is_lighter_grey_than_finished() {
        let (_, _, _, pending_style) = format_status(&RepoStatus::Pending, 0, "update");
        let done = RepoStatus::Done {
            summary: "clean".into(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        };
        let (_, _, _, done_style) = format_status(&done, 0, "update");

        assert_eq!(pending_style.fg, Some(Color::Gray));
        assert_eq!(done_style.fg, Some(Color::DarkGray));
        assert_ne!(
            pending_style.fg, done_style.fg,
            "waiting should be visually distinct from finished"
        );
    }

    #[test]
    fn truncate_preserves_short_strings() {
        assert_eq!(truncate("foo", 10), "foo");
        assert_eq!(truncate("foo", 3), "foo");
    }

    #[test]
    fn truncate_appends_ellipsis() {
        assert_eq!(truncate("feat/long-branch-name", 8), "feat/lo…");
        assert_eq!(display_width(&truncate("feat/long-branch-name", 8)), 8);
    }

    #[test]
    fn truncate_handles_wide_glyphs() {
        // CJK chars are 2 cells. "中文测试" = 8 cells.
        assert_eq!(display_width("中文测试"), 8);
        let t = truncate("中文测试", 5);
        // Want display width <= 5: take "中文" (4) + "…" (1) = 5.
        assert_eq!(display_width(&t), 5);
        assert_eq!(t, "中文…");
    }

    #[test]
    fn truncate_zero_max() {
        assert_eq!(truncate("anything", 0), "");
    }

    #[test]
    fn truncate_max_one() {
        assert_eq!(truncate("anything", 1), "…");
        assert_eq!(truncate("a", 1), "a");
    }
}
