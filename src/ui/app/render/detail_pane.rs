//! The output pane: the transcript as step sections and ANSI-styled lines,
//! the summary of how the run ended, and the scrollbar down its right edge.

use std::ops::Range;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::footer::status_line;
use super::{focus_marker, separator, title_style, two_column_line};
use super::{FOOTER_ROWS, LIST_HEADER_ROWS};
use crate::ui::app::detail;
use crate::ui::app::state::{App, Pane, RunStatus};
use crate::ui::output;

/// Output lines sit two cells in, clear of the pane's left edge.
const INDENT: &str = "  ";

/// The scrollbar's track, and the part of it the visible slice covers.
const TRACK: &str = "\u{2502}";
const THUMB: &str = "\u{2588}";

/// The detail view for the cursor row: a title, a line of run state, the
/// output as labelled step sections with a scrollbar beside them, and a
/// footer unless `split` says the frame's shared one is drawing that part
/// instead.
pub(super) fn draw_detail(frame: &mut Frame, app: &App, area: Rect, split: bool, rows: usize) {
    let width = area.width as usize;

    let mut body: Vec<Line> = Vec::new();
    let mut thumb = None;

    match app.transcript_lines() {
        Some(lines) => {
            let scroll = app.detail_view_scroll(lines.len(), rows);
            let selected = app.output_selection_range();
            for (i, line) in lines.iter().enumerate().skip(scroll).take(rows) {
                let rendered = render_detail_line(line);
                body.push(match &selected {
                    Some(range) if range.contains(&i) => {
                        rendered.style(Style::default().add_modifier(Modifier::REVERSED))
                    }
                    _ => rendered,
                });
            }
            thumb = thumb_rows(scroll, lines.len(), rows);
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

    // The header, the transcript and the footer are drawn as three bands
    // rather than one paragraph, so the scrollbar can take a column off the
    // transcript without taking it off the rules and the key line too.
    let head = area
        .height
        .min(u16::try_from(LIST_HEADER_ROWS).unwrap_or(u16::MAX));
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(title, title_style(app, Pane::Output, split))),
            Line::default(),
            // Where the list puts its column labels, so the two rules meet.
            two_column_line(&detail_summary(app), "", width),
            separator(width),
        ]),
        Rect {
            height: head,
            ..area
        },
    );

    let body_area = Rect::new(
        area.x,
        area.y + head,
        area.width,
        u16::try_from(rows)
            .unwrap_or(u16::MAX)
            .min(area.height - head),
    );
    let text_area = match thumb.filter(|_| body_area.width > 0) {
        Some(thumb) => {
            let bar = Rect {
                x: body_area.right() - 1,
                width: 1,
                ..body_area
            };
            frame.render_widget(Paragraph::new(track_lines(&thumb, rows)), bar);
            Rect {
                width: body_area.width - 1,
                ..body_area
            }
        }
        None => body_area,
    };
    frame.render_widget(Paragraph::new(body), text_area);

    if !split {
        frame.render_widget(
            Paragraph::new(vec![separator(width), status_line(app, width)]),
            Rect::new(
                area.x,
                body_area.y + body_area.height,
                area.width,
                FOOTER_ROWS,
            ),
        );
    }
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

/// The stretch of a `viewport`-row track the visible slice covers, or `None`
/// when the whole transcript is on screen and a bar would only say so at the
/// cost of a column.
///
/// The thumb is proportional but never fills the track: some track showing is
/// the only thing distinguishing "there is more" from "this is all of it", and
/// a long transcript's thumb is already down to its one-row floor.
fn thumb_rows(scroll: usize, total: usize, viewport: usize) -> Option<Range<usize>> {
    // A track under two rows long cannot hold both, so it draws neither.
    if viewport < 2 || total <= viewport {
        return None;
    }
    let length = ((viewport * viewport + total / 2) / total).clamp(1, viewport - 1);
    let travel = viewport - length;
    let furthest = total - viewport;
    // Rounded rather than floored, so the thumb sits against each end of the
    // track exactly when the transcript is against that end and not before.
    let start = (scroll.min(furthest) * travel + furthest / 2) / furthest;
    Some(start..start + length)
}

/// The scrollbar column, a glyph per body row.
fn track_lines(thumb: &Range<usize>, rows: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    (0..rows)
        .map(|row| {
            let glyph = if thumb.contains(&row) { THUMB } else { TRACK };
            Line::from(Span::styled(glyph, dim))
        })
        .collect()
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

    /// A full-screen detail view over a run of `lines` output lines, for the
    /// tests that need more transcript than the pane can hold.
    fn transcript(lines: usize) -> App {
        use crate::executor::TaskEvent;
        let mut a = app(vec![repo("bill-api")]);
        a.detail_open = true;
        let run_id = a.begin_run();
        a.on_task(
            run_id,
            TaskEvent::Step {
                index: 0,
                label: "update".into(),
            },
        );
        for i in 0..lines {
            a.on_task(
                run_id,
                TaskEvent::Output {
                    index: 0,
                    step: 0,
                    stderr: false,
                    line: format!("line {i}"),
                },
            );
        }
        a
    }

    /// The pane's last column down its body rows, which is where the
    /// scrollbar is if there is one.
    fn right_edge(rows: &[String], width: usize) -> String {
        rows[LIST_HEADER_ROWS..rows.len() - FOOTER_ROWS as usize]
            .iter()
            .map(|line| line.chars().nth(width - 1).unwrap_or(' '))
            .collect()
    }

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
    fn output_that_all_fits_on_screen_is_given_no_thumb_to_read() {
        assert_eq!(thumb_rows(0, 20, 20), None);
        assert_eq!(thumb_rows(0, 5, 20), None);
        assert_eq!(thumb_rows(0, 100, 0), None, "a pane with no body rows");
        assert_eq!(
            thumb_rows(0, 100, 1),
            None,
            "one row is not enough track to say anything on"
        );
    }

    #[test]
    fn the_thumb_sits_against_each_end_of_the_track_at_each_end_of_the_output() {
        assert_eq!(thumb_rows(0, 100, 20), Some(0..4));
        assert_eq!(thumb_rows(80, 100, 20), Some(16..20));
    }

    /// Track showing past the thumb is the whole cue. A thumb that filled its
    /// track would say the opposite of what it is there to say, and the floor
    /// on its length is exactly where that could happen.
    #[test]
    fn the_thumb_never_fills_the_track_however_long_the_output_is() {
        for total in [21, 22, 37, 100, 5_000] {
            for scroll in 0..=(total - 20) {
                let thumb = thumb_rows(scroll, total, 20).expect("the output overflows");
                assert!(
                    !thumb.is_empty() && thumb.end <= 20,
                    "{total} lines at {scroll} put the thumb at {thumb:?}"
                );
                assert!(
                    thumb.len() < 20,
                    "{total} lines at {scroll} left no track showing"
                );
            }
        }
    }

    #[test]
    fn a_transcript_taller_than_the_pane_draws_a_scrollbar_down_its_right_edge() {
        let a = transcript(100);
        let rows = frame_rows(&a, 90, 20);
        let column = right_edge(&rows, 90);
        assert!(
            column.contains(THUMB) && column.contains(TRACK),
            "got {column:?}"
        );
    }

    /// The bar costs the output a column, so it is drawn only when it has
    /// something to say.
    #[test]
    fn a_transcript_that_fits_the_pane_is_given_no_scrollbar() {
        let a = transcript(2);
        let rows = frame_rows(&a, 90, 20);
        assert_eq!(right_edge(&rows, 90).trim(), "", "got {rows:#?}");
    }

    /// The thumb is the only thing saying where in a long transcript the pane
    /// is, so it has to move when the reader does.
    #[test]
    fn the_thumb_follows_the_scroll_up_the_track() {
        let mut a = transcript(100);
        let tail = right_edge(&frame_rows(&a, 90, 20), 90);
        assert!(
            tail.ends_with(THUMB),
            "a transcript opens at its tail, got {tail:?}"
        );

        a.detail_scroll.insert(0, 0);
        let top = right_edge(&frame_rows(&a, 90, 20), 90);
        assert!(
            top.starts_with(THUMB) && top.ends_with(TRACK),
            "got {top:?}"
        );
    }
}
