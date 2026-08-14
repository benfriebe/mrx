//! Layout for the resident app: header, repo table, status bar, and the
//! detail view's split and full-screen forms. Branch and working-tree state
//! come from the background probe, the result column from the executor, and
//! which of these three overlays draws (palette, confirmation, detail) comes
//! straight off `App`, not a separate render-time mode.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::actions::Source;
use super::detail::{self, DetailLayout};
use super::keymap;
use super::state::{App, PendingRun, RunStatus};
use crate::ui::widgets::{display_width, frame as spinner_frame, truncate};

const COL_GAP: usize = 2;
/// Width of the leading " ▸ ● " cursor and selection markers.
const PREFIX_W: usize = 5;
/// The sidebar drops the selection marker, so its rows start " ▸ ".
const SIDEBAR_PREFIX_W: usize = 3;

const REPO_LABEL: &str = "REPO";
const BRANCH_LABEL: &str = "BRANCH";
/// Working-tree and upstream state, distinct from [`RESULT_LABEL`], which is
/// what the last run reported.
const STATE_LABEL: &str = "STATE";
const RESULT_LABEL: &str = "RESULT";
/// Title line, a second line of labels, and the rule under them: the chrome
/// above the body of every pane, list and detail alike, so their rules meet
/// across a split. Click resolution derives its row offset from this, so a
/// click's row and the row the table actually painted never disagree.
pub(crate) const LIST_HEADER_ROWS: usize = 3;
/// The rule and key line under the body, drawn once per frame: by the pane
/// itself when it owns the whole width, by the split when it doesn't.
const FOOTER_ROWS: u16 = 2;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    if app.detail_open {
        match detail::layout_for_width(area.width) {
            DetailLayout::FullScreen => draw_detail(frame, app, area, false),
            DetailLayout::Split => draw_split(frame, app, area),
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
    if app.help_open {
        draw_help(frame, area);
    }
}

/// Total chrome rows above and below the table body: the header (fixed at
/// [`LIST_HEADER_ROWS`]) plus a bottom separator, status line, and (while
/// `/` is capturing text) the filter line.
pub(crate) fn chrome_rows(app: &App) -> usize {
    LIST_HEADER_ROWS + FOOTER_ROWS as usize + if app.filtering { 1 } else { 0 }
}

pub(crate) fn list_height(app: &App, area_height: u16) -> usize {
    (area_height as usize).saturating_sub(chrome_rows(app))
}

/// The split: the list narrowed to a sidebar, a rule down the middle, and
/// one footer under both. Two panes that merely abut read as two windows,
/// so the chrome is drawn as one frame divided rather than as two frames
/// side by side.
fn draw_split(frame: &mut Frame, app: &App, area: Rect) {
    let [panes, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(FOOTER_ROWS)]).areas(area);
    let [list, rule, output] = Layout::horizontal([
        Constraint::Length(detail::sidebar_width(area.width)),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(panes);

    draw_list(frame, app, list, true);
    draw_detail(frame, app, output, true);
    draw_split_rule(frame, rule);
    draw_split_footer(frame, app, footer, list.width as usize);
}

/// The vertical rule between the split's panes, notched where the two
/// panes' header rules run into it.
fn draw_split_rule(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line> = (0..area.height as usize)
        .map(|row| {
            let glyph = if row + 1 == LIST_HEADER_ROWS {
                "┼"
            } else {
                "│"
            };
            Line::from(Span::styled(glyph, Style::default().fg(Color::DarkGray)))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The split's shared footer, spanning both panes. One line of keys under
/// the whole frame, not one per pane: with the split open every keystroke
/// reaches the same handler whichever side the pointer is on, so two
/// different key lines would be claiming otherwise.
fn draw_split_footer(frame: &mut Frame, app: &App, area: Rect, rule_col: usize) {
    let width = area.width as usize;
    let lines = vec![joined_separator(width, rule_col), status_line(app, width)];
    frame.render_widget(Paragraph::new(lines), area);
}

/// A horizontal rule that meets the split's vertical one at `at`.
fn joined_separator(width: usize, at: usize) -> Line<'static> {
    if at >= width {
        return separator(width);
    }
    Line::from(Span::styled(
        format!("{}┴{}", "─".repeat(at), "─".repeat(width - at - 1)),
        Style::default().fg(Color::DarkGray),
    ))
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect, sidebar: bool) {
    let width = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    lines.push(header_line(app, width));

    let visible = app.visible_indices();
    // As a sidebar the pane's area already stops above the shared footer, so
    // its body is simply what's left under the header. That lands on the
    // same row count as the full-width list, which is what lets click
    // resolution use one formula for both.
    let lh = if sidebar {
        (area.height as usize).saturating_sub(LIST_HEADER_ROWS)
    } else {
        list_height(app, area.height)
    };
    let cursor_pos = visible.iter().position(|&i| i == app.cursor).unwrap_or(0);
    let start = scroll_offset(cursor_pos, visible.len(), lh);
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

/// The detail view for the cursor row: a title, a line of run and scroll
/// state, the output as labelled step sections, and (when it owns the whole
/// width) a footer. `split` says the frame's shared footer is drawing that
/// last part instead.
fn draw_detail(frame: &mut Frame, app: &App, area: Rect, split: bool) {
    let width = area.width as usize;
    let footer_rows = if split { 0 } else { FOOTER_ROWS as usize };
    let content_height = (area.height as usize).saturating_sub(LIST_HEADER_ROWS + footer_rows);

    let mut body: Vec<Line> = Vec::new();
    let mut position = None;

    match app.run_results.get(app.cursor).and_then(|r| r.as_ref()) {
        Some(RunStatus::Finished { steps, .. }) => {
            let detail_lines = detail::detail_lines(steps);
            let raw_scroll = app.detail_scroll.get(&app.cursor).copied().unwrap_or(0);
            let scroll = detail::clamp_scroll(raw_scroll, detail_lines.len(), content_height);
            for line in detail_lines.iter().skip(scroll).take(content_height) {
                body.push(render_detail_line(line));
            }
            position = scroll_position(scroll, detail_lines.len(), content_height);
        }
        Some(RunStatus::Running) => {
            body.push(Line::from(Span::styled(
                "  running…",
                Style::default().fg(Color::Yellow),
            )));
        }
        Some(RunStatus::Step { label }) => {
            body.push(Line::from(Span::styled(
                format!("  {label}…"),
                Style::default().fg(Color::Yellow),
            )));
        }
        Some(RunStatus::Skipped { reason }) => {
            body.push(Line::from(Span::raw(format!("  skipped: {reason}"))));
        }
        None => {
            body.push(Line::from(Span::styled(
                "  this repo hasn't run yet",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let repo_name = app.repos.get(app.cursor).map(|r| r.name.as_str());
    let title = match (repo_name, &app.run_action) {
        (Some(name), Some(action)) => format!("  {name} · {action}"),
        (Some(name), None) => format!("  {name} · output"),
        (None, _) => "  (no repo)".into(),
    };

    let mut lines = vec![
        Line::from(Span::styled(title, Style::default().bold())),
        // Where the list puts its column labels, so the two rules meet.
        two_column_line(&detail_summary(app), &position.unwrap_or_default(), width),
        separator(width),
    ];
    lines.extend(body);

    if !split {
        // The footer sits on the last two rows however short the output is.
        while lines.len() + (FOOTER_ROWS as usize) < area.height as usize {
            lines.push(Line::default());
        }
        lines.push(separator(width));
        lines.push(status_line(app, width));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// How the cursor row's last run ended, for the line under the detail
/// title: the one thing the title can't say and the output alone makes you
/// count.
fn detail_summary(app: &App) -> String {
    match app.run_results.get(app.cursor).and_then(|r| r.as_ref()) {
        Some(RunStatus::Finished { steps, exit_code }) => {
            let plural = if steps.len() == 1 { "" } else { "s" };
            format!("{} step{plural} · exit {exit_code}", steps.len())
        }
        Some(RunStatus::Running) => "running".into(),
        Some(RunStatus::Step { label }) => format!("running {label}"),
        Some(RunStatus::Skipped { .. }) => "skipped".into(),
        None => "no output yet".into(),
    }
}

/// Which slice of the output is on screen, or `None` when all of it is.
/// The detail view has no scrollbar, so without this a long transcript
/// gives no clue how much of it is above or below.
fn scroll_position(scroll: usize, total: usize, viewport: usize) -> Option<String> {
    (total > viewport && viewport > 0).then(|| {
        format!(
            "{}-{} of {total}",
            scroll + 1,
            (scroll + viewport).min(total)
        )
    })
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
    let title = format!("mrx · {}", app.set_label);
    styled_two_column_line(
        &title,
        &app.header_right_text(),
        width,
        Style::default().bold(),
    )
}

/// `left` at the table's indent and `right` against the far edge, dimmed.
/// A line too narrow for both keeps the left half and drops the right,
/// rather than letting them collide or spill past the pane.
fn two_column_line(left: &str, right: &str, width: usize) -> Line<'static> {
    styled_two_column_line(left, right, width, Style::default().fg(Color::DarkGray))
}

fn styled_two_column_line(
    left: &str,
    right: &str,
    width: usize,
    left_style: Style,
) -> Line<'static> {
    let left = format!("{LEAD_IN}{left}");
    let right = if right.is_empty() {
        String::new()
    } else {
        format!("{right}{LEAD_IN}")
    };
    let dim = Style::default().fg(Color::DarkGray);
    match width.checked_sub(display_width(&left) + display_width(&right)) {
        Some(gap) => Line::from(vec![
            Span::styled(left, left_style),
            Span::raw(" ".repeat(gap)),
            Span::styled(right, dim),
        ]),
        None => Line::from(Span::styled(truncate(&left, width), left_style)),
    }
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

/// Name and state column widths for the two-column sidebar. The name column
/// shrink-wraps the longest repo name so the state text sits beside the
/// names rather than across a field of empty cells, capped at two thirds of
/// `avail` so one very long name can't squeeze the state out. State takes
/// what is left, being the last column.
fn sidebar_column_widths(app: &App, avail: usize) -> (usize, usize) {
    if avail == 0 {
        return (0, 0);
    }
    let name_natural = app
        .repos
        .iter()
        .map(|r| display_width(&r.name))
        .max()
        .unwrap_or(0)
        .max(display_width(REPO_LABEL));
    let name = name_natural.clamp(1, (avail * 2 / 3).max(1));
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

/// Marks bindings left off the end of a footer too narrow for all of them.
/// Ascii, so it never costs a cell more than it looks like it does.
const FOOTER_ELLIPSIS: &str = "…  ";

fn status_line(app: &App, width: usize) -> Line<'static> {
    if let Some(msg) = &app.status_message {
        return Line::from(Span::styled(
            format!("  {msg}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    keys_footer(&keymap::bindings_for(app), width)
}

/// The current mode's keys, fitted to `width` so a narrow terminal never
/// pushes `? help` off the right edge the way an unbounded line would.
///
/// Help is drawn last but budgeted first, so it is the one binding a narrow
/// terminal never loses: everything else fills what is left, whole bindings
/// only, with an ellipsis standing in for what did not fit.
fn keys_footer(bindings: &[keymap::Binding], width: usize) -> Line<'static> {
    let hinted: Vec<keymap::Binding> = bindings.iter().copied().filter(|b| b.hinted).collect();
    let budget = width
        .saturating_sub(LEAD_IN.len())
        .saturating_sub(footer_width(&keymap::HELP));
    let (fitting, dropped) = fitted(&hinted, budget);

    let mut spans = vec![Span::raw(LEAD_IN)];
    for binding in &fitting {
        spans.extend(footer_spans(*binding));
    }
    // Too narrow even for the marker means too narrow to say anything but
    // `? help`, and overflowing the line to admit that would be worse than
    // leaving it unsaid.
    if dropped && budget >= display_width(FOOTER_ELLIPSIS) {
        spans.push(Span::styled(
            FOOTER_ELLIPSIS,
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.extend(footer_spans(keymap::HELP));
    Line::from(spans)
}

/// Indent shared with every other line in the table, so the footer's first
/// key sits under the first column rather than hard against the edge.
const LEAD_IN: &str = "  ";

/// One binding as the footer draws it: the keys, then the label dimmed
/// behind the gap separating it from the next.
fn footer_spans(binding: keymap::Binding) -> [Span<'static>; 2] {
    [
        Span::styled(binding.keys, Style::default().fg(Color::Gray)),
        Span::styled(
            format!(" {}  ", binding.label),
            Style::default().fg(Color::DarkGray),
        ),
    ]
}

/// The cells one binding costs in the footer, its spacing included.
fn footer_width(binding: &keymap::Binding) -> usize {
    display_width(binding.keys) + 1 + display_width(binding.label) + COL_GAP
}

/// The longest run of `bindings`, in order, that fits in `budget` cells, and
/// whether anything had to be left off to get there.
///
/// A binding is kept whole or not at all: cutting one mid-label would read
/// as a different, shorter key. When something does not fit, the ellipsis's
/// own room is set aside up front, so the marker cannot crowd out the
/// bindings it is there to explain.
fn fitted(bindings: &[keymap::Binding], budget: usize) -> (Vec<keymap::Binding>, bool) {
    let whole: usize = bindings.iter().map(footer_width).sum();
    if whole <= budget {
        return (bindings.to_vec(), false);
    }

    let budget = budget.saturating_sub(display_width(FOOTER_ELLIPSIS));
    let mut spent = 0;
    let mut kept = Vec::new();
    for binding in bindings.iter().copied() {
        let cost = footer_width(&binding);
        if spent + cost > budget {
            break;
        }
        spent += cost;
        kept.push(binding);
    }
    (kept, true)
}

/// The full keymap, centred over the table rather than replacing it.
///
/// It lists the detail view's keys alongside the list's, since the overlay
/// is the one place both sets can be read at once: inside the detail view
/// only its own footer is on screen. Two bindings to a row: one each reads
/// more easily but runs off the bottom of a short terminal, and a help
/// screen that crops is worse than one that packs.
fn draw_help(frame: &mut Frame, area: Rect) {
    let every = || keymap::LIST_KEYS.iter().chain(keymap::DETAIL_KEYS);
    let key_col = every().map(|b| display_width(b.keys)).max().unwrap_or(0);
    let label_col = every().map(|b| display_width(b.label)).max().unwrap_or(0);

    let bound = |bindings: &[keymap::Binding]| {
        bindings
            .chunks(2)
            .map(|pair| {
                let spans = pair
                    .iter()
                    .flat_map(|b| {
                        [
                            Span::styled(
                                format!("{LEAD_IN}{:>key_col$}  ", b.keys),
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::raw(format!("{:<label_col$}", b.label)),
                        ]
                    })
                    .collect::<Vec<_>>();
                Line::from(spans)
            })
            .collect::<Vec<_>>()
    };
    let heading = |text: &'static str| {
        Line::from(Span::styled(
            format!("  {text}"),
            Style::default().fg(Color::DarkGray).bold(),
        ))
    };

    let mut lines = vec![heading("IN THE LIST")];
    lines.extend(bound(keymap::LIST_KEYS));
    lines.push(Line::default());
    lines.push(heading("IN THE DETAIL VIEW"));
    lines.extend(bound(keymap::DETAIL_KEYS));
    lines.push(Line::default());
    lines.extend(
        keymap::NOTES
            .iter()
            .map(|n| Line::from(Span::styled(*n, Style::default().fg(Color::DarkGray)))),
    );

    // Wide enough for two key columns and for the widest note, so nothing
    // has to wrap to a second line and push the rest off the bottom.
    let columns = 2 * (LEAD_IN.len() + key_col + COL_GAP + label_col);
    let notes = keymap::NOTES
        .iter()
        .map(|n| display_width(n))
        .max()
        .unwrap_or(0);
    let width = u16::try_from(columns.max(notes) + 2).unwrap_or(u16::MAX);
    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let popup = centred(area, width, height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" keys · esc to close "),
            )
            // A note longer than the box wraps rather than losing its tail
            // off the right edge.
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// A `width` by `height` rect centred in `area`, clamped so an overlay
/// larger than the terminal is cropped rather than positioned off-screen.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let [_, band, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, centre, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width.min(area.width)),
        Constraint::Fill(1),
    ])
    .areas(band);
    centre
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
            "run '{}' on {} repo{}, {}?",
            pending.action,
            pending.targets.len(),
            if pending.targets.len() == 1 { "" } else { "s" },
            confirm_reason(pending),
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

/// The confirmation's reason clause: dirty and unprobed repos are both
/// worth pausing over (section 11), but they are not the same claim, so a
/// selection that is only unprobed says so rather than calling it dirty.
fn confirm_reason(pending: &PendingRun) -> String {
    match (pending.dirty_count, pending.unknown_count) {
        (0, 0) => "confirm".to_string(),
        (dirty, 0) => format!("{dirty} of them dirty"),
        (0, unknown) => format!("{unknown} of them not yet checked"),
        (dirty, unknown) => format!("{dirty} of them dirty, {unknown} not yet checked"),
    }
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

    fn flatten(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The display column `needle` starts at. Byte offsets would not do: the
    /// markers and the ellipsis are multi-byte, so a row's bytes and the
    /// terminal cells it occupies diverge well before the first column.
    fn col_of(haystack: &str, needle: &str) -> Option<usize> {
        haystack
            .find(needle)
            .map(|byte| display_width(&haystack[..byte]))
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

    #[test]
    fn a_footer_with_room_for_every_key_shows_them_all_and_no_ellipsis() {
        let a = app(vec![repo("bill-api")]);
        let text = flatten(&status_line(&a, 200));
        assert!(text.contains("j/k move"), "got {text:?}");
        assert!(text.contains("tab set"), "got {text:?}");
        assert!(text.ends_with("? help  "), "got {text:?}");
        assert!(!text.contains(FOOTER_ELLIPSIS), "got {text:?}");
    }

    #[test]
    fn help_is_the_one_binding_a_narrow_footer_never_drops() {
        let a = app(vec![repo("bill-api")]);
        for width in [12, 24, 40, 60, 80] {
            let text = flatten(&status_line(&a, width));
            assert!(
                text.contains("? help"),
                "width {width} lost the help hint: {text:?}"
            );
            assert!(
                display_width(&text) <= width,
                "width {width} overflowed to {}: {text:?}",
                display_width(&text)
            );
        }
    }

    #[test]
    fn a_footer_too_narrow_marks_what_it_dropped() {
        let a = app(vec![repo("bill-api")]);
        let text = flatten(&status_line(&a, 46));
        assert!(text.contains(FOOTER_ELLIPSIS), "got {text:?}");
        assert!(text.contains("? help"), "got {text:?}");
    }

    #[test]
    fn a_binding_is_kept_whole_or_dropped_never_cut_in_half() {
        let bindings = keymap::LIST_KEYS.to_vec();
        for budget in 0..90 {
            let (kept, _) = fitted(&bindings, budget);
            let spent: usize = kept.iter().map(footer_width).sum();
            assert!(spent <= budget, "budget {budget} spent {spent}");
            // Whatever survived is a prefix of the list, in order.
            for (i, binding) in kept.iter().enumerate() {
                assert_eq!(binding.keys, bindings[i].keys, "budget {budget}");
            }
        }
    }

    #[test]
    fn overlay_only_bindings_stay_out_of_the_footer() {
        let a = app(vec![repo("bill-api")]);
        let text = flatten(&status_line(&a, 400));
        assert!(!text.contains("re-probe"), "got {text:?}");
        assert!(!text.contains("auto-update"), "got {text:?}");
        assert!(
            keymap::LIST_KEYS.iter().any(|b| b.keys == "r"),
            "r is still bound, just not hinted"
        );
    }

    #[test]
    fn cancel_is_only_hinted_while_a_run_is_live() {
        let mut a = app(vec![repo("bill-api")]);
        assert!(!flatten(&status_line(&a, 200)).contains("esc cancel"));
        a.run_action = Some("update".into());
        assert!(flatten(&status_line(&a, 200)).contains("esc cancel"));
    }

    #[test]
    fn the_detail_split_gives_the_sidebar_about_a_third_of_the_frame() {
        assert_eq!(detail::sidebar_width(120), 40);
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

    /// The split's two panes are drawn as separate widgets, so nothing but
    /// this makes their rules meet: both put a title, a labels line, and a
    /// rule above the body, and the vertical rule notches at the same row.
    #[test]
    fn both_panes_of_the_split_rule_off_their_header_on_the_same_row() {
        let mut a = app(vec![repo("bill-api"), repo("crew")]);
        a.detail_open = true;
        let (list, output) = split_panes(&a, 140, 20);

        assert!(
            list[LIST_HEADER_ROWS - 1].starts_with('─'),
            "the list rules off row {}, got {:?}",
            LIST_HEADER_ROWS - 1,
            list[LIST_HEADER_ROWS - 1]
        );
        assert!(
            output[LIST_HEADER_ROWS - 1].starts_with('─'),
            "the detail pane rules off the same row, got {:?}",
            output[LIST_HEADER_ROWS - 1]
        );
    }

    #[test]
    fn the_split_draws_one_footer_under_both_panes_not_one_each() {
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        let (list, output) = split_panes(&a, 140, 20);
        for pane in [&list, &output] {
            assert!(
                !pane.iter().any(|line| line.contains("? help")),
                "a pane drew its own footer: {pane:?}"
            );
        }
    }

    #[test]
    fn the_vertical_rule_notches_where_the_header_rules_meet_it() {
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        let rows = frame_rows(&a, 140, 20);
        let col = detail::sidebar_width(140) as usize;
        assert_eq!(
            rows[LIST_HEADER_ROWS - 1].chars().nth(col),
            Some('┼'),
            "got {:?}",
            rows[LIST_HEADER_ROWS - 1]
        );
        assert_eq!(rows[0].chars().nth(col), Some('│'), "got {:?}", rows[0]);
    }

    #[test]
    fn the_shared_footers_rule_meets_the_vertical_one() {
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        let rows = frame_rows(&a, 140, 20);
        let col = detail::sidebar_width(140) as usize;
        assert_eq!(rows[rows.len() - 2].chars().nth(col), Some('┴'));
    }

    /// The overlay grows with the keymap, and `centred` crops rather than
    /// scrolls, so the notes at the bottom are what a terminal one row too
    /// short silently loses.
    #[test]
    fn the_help_overlay_fits_a_terminal_no_taller_than_thirty_rows() {
        let mut a = app(vec![repo("bill-api")]);
        a.help_open = true;
        let rows = frame_rows(&a, 100, 30);
        let last = keymap::NOTES.last().unwrap().trim();
        assert!(
            rows.iter().any(|line| line.contains(last)),
            "the last note was cropped off the bottom: {rows:#?}"
        );
    }

    /// The blank rows that push the footer down are counted, not measured,
    /// so an off-by-one there pads the key line off the bottom of the frame
    /// where nothing else would notice it had gone.
    #[test]
    fn the_full_screen_detail_view_keeps_its_footer_on_the_last_row() {
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        for height in [8, 12, 40] {
            let rows = frame_rows(&a, 90, height);
            assert!(
                rows.last().unwrap().contains("? help"),
                "height {height} lost the footer: {rows:#?}"
            );
        }
    }

    #[test]
    fn the_detail_pane_says_how_the_run_ended() {
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        a.run_results[0] = Some(RunStatus::Finished {
            steps: vec![],
            exit_code: 1,
        });
        assert_eq!(detail_summary(&a), "0 steps · exit 1");

        a.run_results[0] = None;
        assert_eq!(detail_summary(&a), "no output yet");
    }

    #[test]
    fn the_scroll_position_is_only_reported_when_some_output_is_off_screen() {
        assert_eq!(scroll_position(0, 10, 20), None);
        assert_eq!(scroll_position(0, 100, 20), Some("1-20 of 100".into()));
        assert_eq!(scroll_position(90, 100, 20), Some("91-100 of 100".into()));
    }

    /// Every row of a rendered frame, as plain text with trailing blanks
    /// trimmed, so a layout assertion can be made against what a terminal
    /// would actually show.
    fn frame_rows(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The split's two panes, each as its own rows, sliced apart at the
    /// vertical rule and stopping above the footer both of them share.
    fn split_panes(app: &App, width: u16, height: u16) -> (Vec<String>, Vec<String>) {
        let mut rows = frame_rows(app, width, height);
        rows.truncate(rows.len() - FOOTER_ROWS as usize);
        let col = detail::sidebar_width(width) as usize;
        let cut = |line: &String, range: std::ops::Range<usize>| {
            line.chars()
                .skip(range.start)
                .take(range.len())
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        (
            rows.iter().map(|l| cut(l, 0..col)).collect(),
            rows.iter()
                .map(|l| cut(l, col + 1..width as usize))
                .collect(),
        )
    }
}
