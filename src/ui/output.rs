//! Captured process output as styled terminal lines, shared by the run
//! view's expand panel and ui mode's detail pane so the same output never
//! reads two different ways depending on which view is open.

use ratatui::prelude::*;

use crate::ansi;

/// How loudly a stderr line should be drawn. stderr is the "not the data"
/// channel, not an error channel: git's fetch progress and npm's warnings
/// both arrive there, so painting the whole stream red would say every run
/// failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Plain,
    Warn,
    Error,
}

/// Severity markers as tools actually emit them, matched against the first
/// few words only: `npm warn ...`, `fatal: ...`, `[error] ...`. Looking
/// further in would catch a filename or a phrase like "0 errors".
const LEAD_WORDS: usize = 3;

/// Classify a stderr line by the marker it leads with, if any.
fn severity(line: &str) -> Severity {
    line.split_whitespace()
        .take(LEAD_WORDS)
        .find_map(|word| {
            match word
                .trim_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_ascii_lowercase()
                .as_str()
            {
                "warn" | "warning" => Some(Severity::Warn),
                "err" | "error" | "fatal" | "panicked" => Some(Severity::Error),
                _ => None,
            }
        })
        .unwrap_or(Severity::Plain)
}

/// One line of captured output as one span per [`ansi::Run`]: a run carrying
/// its own SGR colour keeps it, since the tool knew what it meant. A run that
/// set none falls back to the line's [`severity`] on stderr, and to the
/// terminal's own foreground on stdout. `indent` merges into the first span
/// rather than standing alone, so a line with no escapes is still one span.
pub fn output_line(text: &str, stderr: bool, indent: &str) -> Line<'static> {
    // severity() reads the lead words, which a leading escape would hide
    // from it, so it gets the stripped text.
    let fallback_fg = stderr.then(|| match severity(&ansi::strip(text)) {
        Severity::Plain => Color::DarkGray,
        Severity::Warn => Color::Yellow,
        Severity::Error => Color::Red,
    });

    let mut runs = ansi::parse(text);
    if runs.is_empty() {
        runs.push(ansi::Run {
            text: String::new(),
            style: Style::default(),
        });
    }
    let spans = runs
        .into_iter()
        .enumerate()
        .map(|(i, run)| {
            let mut style = run.style;
            if style.fg.is_none() {
                style.fg = fallback_fg;
            }
            let text = if i == 0 {
                format!("{indent}{}", run.text)
            } else {
                run.text
            };
            Span::styled(text, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stderr_color(text: &str) -> Option<Color> {
        output_line(text, true, "").spans[0].style.fg
    }

    #[test]
    fn git_progress_on_stderr_is_not_an_error() {
        for line in [
            "Already on 'main'",
            "From ssh://github.com/mr-yum/bill-db-schema",
            " * branch            main       -> FETCH_HEAD",
            "Switched to branch 'main'",
        ] {
            assert_eq!(severity(line), Severity::Plain, "{line}");
        }
    }

    #[test]
    fn a_warning_reads_as_a_warning_whoever_spells_it() {
        for line in [
            "npm warn install-scripts 4 packages have install scripts",
            "npm WARN deprecated request@2.88.2",
            "warning: unused variable `x`",
            "[warn] something is off",
        ] {
            assert_eq!(severity(line), Severity::Warn, "{line}");
        }
    }

    #[test]
    fn an_error_reads_as_an_error_whoever_spells_it() {
        for line in [
            "npm error code ELIFECYCLE",
            "npm ERR! code ELIFECYCLE",
            "fatal: not a git repository",
            "error: could not compile `mrx`",
            "thread 'main' panicked at src/main.rs:1:1",
        ] {
            assert_eq!(severity(line), Severity::Error, "{line}");
        }
    }

    #[test]
    fn a_marker_word_further_in_does_not_colour_the_line() {
        for line in [
            "Compiling the error-handling crate",
            "wrote report to /tmp/build/error.log",
            "run `npm audit` for details",
        ] {
            assert_eq!(severity(line), Severity::Plain, "{line}");
        }
    }

    #[test]
    fn only_stderr_lines_that_claim_trouble_are_drawn_as_trouble() {
        assert_eq!(
            stderr_color("From ssh://example.com/x"),
            Some(Color::DarkGray)
        );
        assert_eq!(
            stderr_color("npm warn install-scripts 4 packages"),
            Some(Color::Yellow)
        );
        assert_eq!(stderr_color("npm error code ELIFECYCLE"), Some(Color::Red));
    }

    /// stdout is the data channel, so nothing about the text itself is
    /// grounds for recolouring it.
    #[test]
    fn a_stdout_line_is_left_the_terminals_own_colour_however_it_reads() {
        let line = output_line("npm error code ELIFECYCLE", false, "  ");
        assert_eq!(line.spans[0].style.fg, None);
        assert_eq!(line.spans[0].content, "  npm error code ELIFECYCLE");
    }

    #[test]
    fn a_line_with_ansi_escapes_renders_as_multiple_spans_with_their_own_colours() {
        // The first run carries its own colour; the second sets none, so it
        // falls back to Warn, from "npm warn" in the stripped text.
        let line = output_line("\u{1b}[34mnote: \u{1b}[0mnpm warn trailing", true, "  ");
        assert_eq!(line.spans.len(), 2, "got {:?}", line.spans);
        assert_eq!(line.spans[0].content, "  note: ");
        assert_eq!(line.spans[0].style.fg, Some(Color::Blue));
        assert_eq!(line.spans[1].content, "npm warn trailing");
        assert_eq!(line.spans[1].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn a_stderr_lines_own_colour_wins_over_its_severity_colour() {
        // Severity alone would call this an error; the line's own colour wins.
        assert_eq!(
            stderr_color("\u{1b}[34mnpm error code ELIFECYCLE"),
            Some(Color::Blue)
        );
    }

    #[test]
    fn severity_is_still_detected_when_the_line_starts_with_an_escape_sequence() {
        // A leading escape that sets only bold must not hide the lead words.
        assert_eq!(
            stderr_color("\u{1b}[1mnpm warn something"),
            Some(Color::Yellow)
        );
    }
}
