//! The bottom line of the frame: the status message or the mode's key hints,
//! and the rule that fits those hints to the width available.

use ratatui::prelude::*;

use super::COL_GAP;
use crate::ui::app::keymap;
use crate::ui::app::state::App;
use crate::ui::widgets::display_width;

/// Marks bindings left off the end of a footer too narrow for all of them.
/// Ascii, so it never costs a cell more than it looks like it does.
const FOOTER_ELLIPSIS: &str = "…  ";

pub(super) fn status_line(app: &App, width: usize) -> Line<'static> {
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
pub(super) const LEAD_IN: &str = "  ";

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

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;

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
}
