//! Layout for the resident app: header, repo table, status bar, and the
//! detail view's split and full-screen forms. Branch and working-tree state
//! come from the background probe, the result column from the executor, and
//! which of these three overlays draws (palette, confirmation, detail) comes
//! straight off `App`, not a separate render-time mode.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use super::actions::Source;
use super::detail::{self, DetailLayout};
use super::state::{App, PendingRun, RunStatus};
use crate::ui::widgets::{display_width, frame as spinner_frame, truncate};

const COL_GAP: usize = 2;
/// Width of the leading " ▸ ● " cursor and selection markers.
const PREFIX_W: usize = 5;
/// Header line plus its separator, above the table body in every layout
/// that shows one (the plain list and the detail sidebar alike), so a
/// click's row and the row the table actually painted never disagree.
pub(crate) const LIST_HEADER_ROWS: usize = 2;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    if app.detail_open {
        match detail::layout_for_width(area.width) {
            DetailLayout::FullScreen => draw_detail(frame, app, area),
            DetailLayout::Split => {
                let sidebar_w = detail::sidebar_width(area.width);
                let cols = Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(0)])
                    .split(area);
                draw_list(frame, app, cols[0], true);
                draw_detail(frame, app, cols[1]);
            }
        }
    } else {
        draw_list(frame, app, area, false);
    }

    if app.palette_open {
        draw_palette(frame, app, area);
    }
    if app.set_picker_open {
        draw_set_picker(frame, app, area);
    }
    if let Some(pending) = &app.pending_run {
        draw_confirm(frame, pending, area);
    }
    if app.quit_pending {
        draw_quit_confirm(frame, area);
    }
}

/// Total chrome rows above and below the table body: the header (fixed at
/// [`LIST_HEADER_ROWS`]) plus a bottom separator, status line, and (while
/// `/` is capturing text) the filter line.
pub(crate) fn chrome_rows(app: &App) -> usize {
    LIST_HEADER_ROWS + if app.filtering { 3 } else { 2 }
}

pub(crate) fn list_height(app: &App, area_height: u16) -> usize {
    (area_height as usize).saturating_sub(chrome_rows(app))
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect, sidebar: bool) {
    let width = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    lines.push(header_line(app, width));
    lines.push(separator(width));

    let visible = app.visible_indices();
    let lh = list_height(app, area.height);
    let cursor_pos = visible.iter().position(|&i| i == app.cursor).unwrap_or(0);
    let start = scroll_offset(cursor_pos, visible.len(), lh);
    let end = visible.len().min(start + lh);

    if sidebar {
        let (name_col, state_col) = sidebar_column_widths(width.saturating_sub(PREFIX_W));
        for &idx in &visible[start..end] {
            lines.push(sidebar_repo_line(app, idx, name_col, state_col));
        }
    } else {
        let (name_col, branch_col, state_col, result_col) =
            column_widths(app, width.saturating_sub(PREFIX_W));
        for &idx in &visible[start..end] {
            lines.push(repo_line(
                app, idx, name_col, branch_col, state_col, result_col,
            ));
        }
    }

    lines.push(separator(width));
    if app.filtering {
        lines.push(filter_line(app));
    }
    lines.push(status_line(app, sidebar));

    frame.render_widget(Paragraph::new(lines), area);
}

/// The detail view for the cursor row: a title line, the selected run's
/// output as labelled step sections, and a status bar. Drawn identically
/// whether it's splitting beside the sidebar or filling the whole frame;
/// only `area`'s width differs between the two callers.
fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    let repo_name = app.repos.get(app.cursor).map(|r| r.name.as_str());
    let title = match (repo_name, &app.run_action) {
        (Some(name), Some(action)) => format!("  {name} · {action}"),
        (Some(name), None) => format!("  {name} · output"),
        (None, _) => "  (no repo)".into(),
    };
    lines.push(Line::from(Span::styled(title, Style::default().bold())));
    lines.push(separator(width));

    // One row for the status line reserved below the content.
    let content_height = (area.height as usize).saturating_sub(LIST_HEADER_ROWS + 1);

    match app.run_results.get(app.cursor).and_then(|r| r.as_ref()) {
        Some(RunStatus::Finished { steps, .. }) => {
            let detail_lines = detail::detail_lines(steps);
            let raw_scroll = app.detail_scroll.get(&app.cursor).copied().unwrap_or(0);
            let scroll = detail::clamp_scroll(raw_scroll, detail_lines.len(), content_height);
            for line in detail_lines.iter().skip(scroll).take(content_height) {
                lines.push(render_detail_line(line));
            }
        }
        Some(RunStatus::Running) => {
            lines.push(Line::from(Span::styled(
                "  running…",
                Style::default().fg(Color::Yellow),
            )));
        }
        Some(RunStatus::Step { label }) => {
            lines.push(Line::from(Span::styled(
                format!("  {label}…"),
                Style::default().fg(Color::Yellow),
            )));
        }
        Some(RunStatus::Skipped { reason }) => {
            lines.push(Line::from(Span::raw(format!("  skipped: {reason}"))));
        }
        None => {
            lines.push(Line::from(Span::styled(
                "  this repo hasn't run yet",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    while lines.len() + 1 < area.height as usize {
        lines.push(Line::default());
    }
    lines.push(status_line(app, false));

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_detail_line(line: &detail::DetailLine) -> Line<'static> {
    match line {
        detail::DetailLine::StepHeader { label, code, .. } => {
            let (marker, color) = if *code == 0 {
                ("✓", Color::Green)
            } else {
                ("✗", Color::Red)
            };
            Line::from(Span::styled(
                format!("  $ {label}  {marker}"),
                Style::default().fg(color).bold(),
            ))
        }
        detail::DetailLine::Stdout(s) => Line::from(Span::raw(format!("  {s}"))),
        detail::DetailLine::Stderr(s) => Line::from(Span::styled(
            format!("  {s}"),
            Style::default().fg(Color::Red),
        )),
        detail::DetailLine::Blank => Line::default(),
    }
}

fn header_line(app: &App, width: usize) -> Line<'static> {
    let title = format!("  mrx · {}", app.set_label);
    let right = match run_status_text(app) {
        Some(r) => format!("{}  ", r),
        None => format!(
            "{} repos · {} selected  ",
            app.repos.len(),
            app.effective_selection().len()
        ),
    };
    let gap = width.saturating_sub(display_width(&title) + display_width(&right));
    Line::from(vec![
        Span::styled(title, Style::default().bold()),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, Style::default().fg(Color::DarkGray)),
    ])
}

/// The live run's summary for the header: action name, done/total, and a
/// failure count once there's one to show.
fn run_status_text(app: &App) -> Option<String> {
    let action = app.run_action.as_ref()?;
    let mut text = format!("{} {}/{}", action, app.run_completed, app.run_total);
    if app.run_failed > 0 {
        text.push_str(&format!(" · {} failed", app.run_failed));
    }
    Some(text)
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
    let (branch_text, state_text) = if probe.spinner {
        (spinner_frame(app.tick).to_string(), String::new())
    } else {
        (probe.branch, probe.state)
    };
    let branch = truncate(&branch_text, branch_col);
    let branch_padding = branch_col.saturating_sub(display_width(&branch)) + COL_GAP;
    let state = truncate(&state_text, state_col);
    let state_padding = state_col.saturating_sub(display_width(&state)) + COL_GAP;

    let result_text = app.result_text(idx);
    let result_style = result_style(app, idx);
    let result = truncate(&result_text, result_col);

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
        Span::raw(" ".repeat(state_padding)),
        Span::styled(result, result_style),
    ])
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

/// Name and state column widths for the two-column sidebar: name gets about
/// two thirds of `avail`, state gets the rest, so the state text has a
/// bounded column to truncate into rather than running off the edge.
fn sidebar_column_widths(avail: usize) -> (usize, usize) {
    if avail == 0 {
        return (0, 0);
    }
    let name = (avail * 2 / 3).clamp(1, avail);
    let state = avail.saturating_sub(name + COL_GAP);
    (name, state)
}

/// The detail sidebar's row: name and one working-tree-state column, since
/// branch and ahead/behind are detail about a repo you're no longer
/// scanning (section 02).
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
        Some(state) => super::probe::dirty_text_brief(state),
        None if app.probing.contains(&idx) => spinner_frame(app.tick).to_string(),
        None => String::new(),
    };
    let state = truncate(&state_text, state_col);

    Line::from(vec![
        Span::styled(
            format!(" {} ", cursor_marker),
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

fn status_line(app: &App, sidebar: bool) -> Line<'static> {
    if let Some(msg) = &app.status_message {
        return Line::from(Span::styled(
            format!("  {msg}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    let keys = if app.filtering {
        "  esc clear  enter keep".to_string()
    } else if app.detail_open {
        "  j/k move  ^d/^u scroll  y copy  esc back  ^r reload  m mouse  q quit".to_string()
    } else if sidebar {
        "  j/k move  esc back".to_string()
    } else {
        let mut keys = String::from(
            "  j/k move  g/G top/bottom  space select  a all  A none  i invert  / filter  \
             u update  s/f/d status/fetch/diff  : action  r reprobe  tab set  ^r reload  m mouse",
        );
        // Esc only cancels here: while a run is live and the plain list is
        // showing, not once it's opened the detail view (there, Esc is back).
        if app.run_action.is_some() {
            keys.push_str("  esc cancel");
        }
        keys.push_str("  q quit");
        keys
    };
    Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray)))
}

/// Column widths for the four-column repo table: NAME, BRANCH, and STATE
/// each get their natural width up to a share of `avail`, RESULT gets
/// whatever is left, since a summary or a live step label is usually the
/// most interesting thing in the row.
fn column_widths(app: &App, avail: usize) -> (usize, usize, usize, usize) {
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
    let state_nat = (0..app.repos.len())
        .map(|i| display_width(&app.probe_display(i).state))
        .max()
        .unwrap_or(0)
        .max(display_width("STATE"));

    let name = name_nat.min(avail / 4);
    let branch = branch_nat.min(avail / 6);
    let state = state_nat.min(avail / 4);
    let result = avail.saturating_sub(name + branch + state + 3 * COL_GAP);
    (name, branch, state, result)
}

/// First visible-list index to draw, so the cursor stays on screen once it
/// scrolls past the bottom of `list_height` rows.
pub(crate) fn scroll_offset(cursor_pos: usize, visible_len: usize, list_height: usize) -> usize {
    if list_height == 0 || visible_len == 0 {
        return 0;
    }
    if cursor_pos < list_height {
        0
    } else {
        cursor_pos - list_height + 1
    }
}

fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - pct_y) / 2),
        Constraint::Percentage(pct_y),
        Constraint::Percentage((100 - pct_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - pct_x) / 2),
        Constraint::Percentage(pct_x),
        Constraint::Percentage((100 - pct_x) / 2),
    ])
    .split(vertical[1])[1]
}

/// The action palette (`:`): every runnable action for the set, filtered as
/// you type, showing source and repo count so an unfamiliar name is
/// trustworthy before you run it (section 08).
fn draw_palette(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered_rect(60, 60, area);
    frame.render_widget(Clear, popup);

    let repo_count = app.repos.len();
    let items: Vec<ListItem> = app
        .palette_visible()
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let source = match a.source {
                Source::Builtin => "builtin",
                Source::Default => "every repo",
                Source::PerRepo => "per-repo",
            };
            let text = format!("{}  {}, {} of {}", a.name, source, a.repos, repo_count);
            let style = if i == app.palette_cursor {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(text).style(style)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" action: {} ", app.palette_filter));
    frame.render_widget(List::new(items).block(block), popup);
}

/// The set picker (`tab`): every set `sets::discover()` finds, plus the
/// active config appended as `(unnamed)` when it isn't one of them, with a
/// `*` marking whichever entry is actually on screen right now.
fn draw_set_picker(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered_rect(50, 50, area);
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = app
        .set_entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let marker = if entry.path == app.config_path {
                "* "
            } else {
                "  "
            };
            let style = if i == app.set_picker_cursor {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(format!("{marker}{}", entry.name)).style(style)
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(" switch set ");
    frame.render_widget(List::new(items).block(block), popup);
}

/// Shown when `q`/`Ctrl-C` is pressed while a run is still live (section 03,
/// "prompts if a run is live").
fn draw_quit_confirm(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(50, 20, area);
    frame.render_widget(Clear, popup);

    let text = vec![
        Line::from("a run is still live, quit anyway?"),
        Line::default(),
        Line::from(Span::styled(
            "y/enter quit   anything else cancels",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let block = Block::default().borders(Borders::ALL).title(" quit? ");
    frame.render_widget(Paragraph::new(text).block(block), popup);
}

/// The dirty-selection confirmation from section 11: shown before a run
/// touches any repo the last probe found dirty, unless `--force` skipped it.
fn draw_confirm(frame: &mut Frame, pending: &PendingRun, area: Rect) {
    let popup = centered_rect(50, 20, area);
    frame.render_widget(Clear, popup);

    let text = vec![
        Line::from(format!(
            "run '{}' on {} repo{}, {} of them dirty?",
            pending.action,
            pending.targets.len(),
            if pending.targets.len() == 1 { "" } else { "s" },
            pending.dirty_count,
        )),
        Line::default(),
        Line::from(Span::styled(
            "y/enter confirm   n/esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let block = Block::default().borders(Borders::ALL).title(" confirm ");
    frame.render_widget(Paragraph::new(text).block(block), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Repo;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn repo(name: &str) -> Repo {
        Repo {
            name: name.to_string(),
            path: PathBuf::from(format!("/nonexistent/{}", name)),
            clone_url: None,
            keys: Default::default(),
        }
    }

    fn app(repos: Vec<Repo>) -> App {
        App::new(
            repos,
            "work".into(),
            4,
            BTreeMap::new(),
            PathBuf::from("/dev/null"),
            false,
            None,
        )
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
    fn name_column_never_exceeds_a_quarter_of_the_available_width() {
        let long_name = "a-name-that-is-extremely-long-past-any-reasonable-column-width";
        let a = app(vec![repo(long_name)]);
        let (name, branch, state, result) = column_widths(&a, 80);
        assert!(name <= 20, "got name width {name}");
        assert_eq!(name + branch + state + result + 3 * COL_GAP, 80);
    }

    #[test]
    fn the_detail_split_gives_the_sidebar_about_a_third_of_the_frame() {
        assert_eq!(detail::sidebar_width(120), 40);
    }

    #[test]
    fn sidebar_columns_reserve_room_for_the_state_text_instead_of_giving_it_all_to_name() {
        let (name, state) = sidebar_column_widths(30);
        assert!(
            state > 0,
            "the state column must not be squeezed to nothing"
        );
        assert_eq!(name + state + COL_GAP, 30);
    }

    #[test]
    fn sidebar_columns_handle_zero_available_width() {
        assert_eq!(sidebar_column_widths(0), (0, 0));
    }
}
