//! The output pane: the transcript as step sections and ANSI-styled lines,
//! the summary of how the run ended, and the position readout beside it.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::footer::status_line;
use super::layout::detail_content_height;
use super::{focus_marker, separator, title_style, two_column_line, FOOTER_ROWS};
use crate::ui::app::detail;
use crate::ui::app::state::{App, Pane, RunStatus};
use crate::ui::output;

/// Output lines sit two cells in, clear of the pane's left edge.
const INDENT: &str = "  ";

/// The detail view for the cursor row: a title, a line of run and scroll
/// state, the output as labelled step sections, and a footer unless `split`
/// says the frame's shared one is drawing that part instead.
pub(super) fn draw_detail(frame: &mut Frame, app: &App, area: Rect, split: bool) {
    let width = area.width as usize;
    let content_height = detail_content_height(area.height, split);

    let mut body: Vec<Line> = Vec::new();
    let mut position = None;

    match app.transcript_lines() {
        Some(lines) => {
            let scroll = app.detail_view_scroll(lines.len(), content_height);
            let selected = app.output_selection_range();
            for (i, line) in lines.iter().enumerate().skip(scroll).take(content_height) {
                let rendered = render_detail_line(line);
                body.push(match &selected {
                    Some(range) if range.contains(&i) => {
                        rendered.style(Style::default().add_modifier(Modifier::REVERSED))
                    }
                    _ => rendered,
                });
            }
            position = scroll_position(scroll, lines.len(), content_height);
        }
        None => body.push(
            match app.run_results.get(app.cursor).and_then(|r| r.as_ref()) {
                Some(RunStatus::Skipped { reason }) => {
                    Line::from(Span::raw(format!("  skipped: {reason}")))
                }
                Some(_) => Line::from(Span::styled(
                    "  waiting for output…",
                    Style::default().fg(Color::Yellow),
                )),
                None => Line::from(Span::styled(
                    "  this repo hasn't run yet",
                    Style::default().fg(Color::DarkGray),
                )),
            },
        ),
    }

    let repo_name = app.repos.get(app.cursor).map(|r| r.name.as_str());
    let lead = focus_marker(app, Pane::Output, split);
    let title = match (repo_name, &app.run_action) {
        (Some(name), Some(action)) => format!("{lead}{name} · {action}"),
        (Some(name), None) => format!("{lead}{name} · output"),
        (None, _) => format!("{lead}(no repo)"),
    };

    let mut lines = vec![
        Line::from(Span::styled(title, title_style(app, Pane::Output, split))),
        Line::default(),
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

/// How the cursor row's last run ended, for the line under the detail title.
fn detail_summary(app: &App) -> String {
    match app.run_results.get(app.cursor).and_then(|r| r.as_ref()) {
        Some(RunStatus::Finished { steps, exit_code }) => {
            let plural = if steps.len() == 1 { "" } else { "s" };
            format!("{} step{plural} · exit {exit_code}", steps.len())
        }
        Some(RunStatus::Running) => "running".into(),
        Some(RunStatus::Step { label }) => format!("running {label}"),
        Some(RunStatus::Skipped { .. }) => "skipped".into(),
        // The body already says the repo has not run.
        None => String::new(),
    }
}

/// Which slice of the output is on screen, or `None` when all of it is. The
/// detail view has no scrollbar, so this is its only position cue.
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
            let (marker, color) = match code {
                None => ("…", Color::Yellow),
                Some(0) => ("✓", Color::Green),
                Some(_) => ("✗", Color::Red),
            };
            Line::from(Span::styled(
                format!("  $ {label}  {marker}"),
                Style::default().fg(color).bold(),
            ))
        }
        detail::DetailLine::Stdout(s) => output::output_line(s, false, INDENT),
        detail::DetailLine::Stderr(s) => output::output_line(s, true, INDENT),
        detail::DetailLine::Blank => Line::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;

    fn stderr_color(text: &str) -> Option<Color> {
        render_detail_line(&detail::DetailLine::Stderr(text.into())).spans[0]
            .style
            .fg
    }

    /// The two stream variants have to reach [`output::output_line`] with the
    /// right flag, or stderr silently loses its severity colouring.
    #[test]
    fn each_stream_is_handed_to_the_shared_renderer_as_itself() {
        assert_eq!(stderr_color("npm error code ELIFECYCLE"), Some(Color::Red));
        assert_eq!(
            render_detail_line(&detail::DetailLine::Stdout(
                "npm error code ELIFECYCLE".into()
            ))
            .spans[0]
                .style
                .fg,
            None,
            "stdout is the data channel and keeps the terminal's own colour"
        );
    }

    /// The blank rows that push the footer down are counted, not measured, so
    /// an off-by-one pads the key line off the bottom unnoticed.
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
    fn a_live_step_is_marked_as_running_rather_than_as_having_succeeded() {
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        let run_id = a.begin_run();
        a.on_task(
            run_id,
            crate::executor::TaskEvent::Step {
                index: 0,
                label: "update".into(),
            },
        );
        a.on_task(
            run_id,
            crate::executor::TaskEvent::Output {
                index: 0,
                step: 0,
                stderr: false,
                line: "step 1 of 6".into(),
            },
        );

        let rows = frame_rows(&a, 90, 12);
        let joined = rows.join("\n");
        assert!(joined.contains("step 1 of 6"), "got {joined}");
        assert!(
            joined.contains("$ update  …"),
            "a step still running has no ✓ to show, got {joined}"
        );
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
        assert_eq!(
            detail_summary(&a),
            "",
            "the body already says the repo has not run"
        );
    }

    #[test]
    fn the_scroll_position_is_only_reported_when_some_output_is_off_screen() {
        assert_eq!(scroll_position(0, 10, 20), None);
        assert_eq!(scroll_position(0, 100, 20), Some("1-20 of 100".into()));
        assert_eq!(scroll_position(90, 100, 20), Some("91-100 of 100".into()));
    }
}
