//! The detail view: a repo's run output, opened with Enter on the cursor row
//! and closed with Esc. Steps come from `StepResult` as separately labelled
//! sections rather than one concatenated scrollback (section 02).

use crate::ansi;
use crate::executor::StepResult;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Below this width the detail view takes the whole screen instead of
/// splitting beside the list (section 02): the split stops being readable
/// once the sidebar has nowhere left to shrink. Both layouts read the same
/// `App` state; this is the one branch that decides which to draw.
pub const WIDTH_BREAKPOINT: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailLayout {
    /// The list collapses to a sidebar; the detail view fills the rest.
    Split,
    /// The detail view takes the whole frame; Esc is the only way back.
    FullScreen,
}

/// Which layout a frame of `width` columns should use.
pub fn layout_for_width(width: u16) -> DetailLayout {
    if width < WIDTH_BREAKPOINT {
        DetailLayout::FullScreen
    } else {
        DetailLayout::Split
    }
}

/// Sidebar width in a split layout: about a third of the frame, matching the
/// mockup's proportions.
pub fn sidebar_width(width: u16) -> u16 {
    width / 3
}

/// Whether a pointer at `column` is over the output pane in whichever
/// layout `width` selects; click, drag, and scroll all resolve through
/// this so they can't disagree with what draw_detail painted.
pub fn pointer_over_output(width: u16, column: u16) -> bool {
    match layout_for_width(width) {
        DetailLayout::FullScreen => true,
        DetailLayout::Split => column >= sidebar_width(width),
    }
}

/// One line of a flattened, step-labelled run transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailLine {
    /// A step's own header: its label and exit code, so steps read as
    /// separate sections rather than one scrollback. `code` is `None` for a
    /// step still running, which has no outcome to report yet.
    StepHeader {
        step: usize,
        label: String,
        code: Option<i32>,
    },
    Stdout(String),
    Stderr(String),
    Blank,
}

impl DetailLine {
    /// The line as text worth putting on a clipboard: what's on screen
    /// without the tick, cross or ellipsis a step header is drawn with
    /// (those are status, not output), and without the ANSI escapes a
    /// captured line carries, since whatever receives this is rarely a
    /// terminal.
    pub fn text(&self) -> String {
        match self {
            DetailLine::StepHeader { label, .. } => format!("$ {label}"),
            DetailLine::Stdout(s) | DetailLine::Stderr(s) => ansi::strip(s),
            DetailLine::Blank => String::new(),
        }
    }
}

/// Flatten a finished run's steps into a scrollable transcript, one
/// labelled section per step, in order.
pub fn detail_lines(steps: &[StepResult]) -> Vec<DetailLine> {
    lines(steps, false)
}

/// The same, for a run still in flight: the last step is the one running,
/// so its heading reports no exit code rather than the zero it is carrying
/// as a placeholder.
pub fn live_lines(steps: &[StepResult]) -> Vec<DetailLine> {
    lines(steps, true)
}

fn lines(steps: &[StepResult], last_is_running: bool) -> Vec<DetailLine> {
    let mut out = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            out.push(DetailLine::Blank);
        }
        let running = last_is_running && i + 1 == steps.len();
        out.push(DetailLine::StepHeader {
            step: i,
            label: step.label.clone(),
            code: (!running).then_some(step.code),
        });
        out.extend(
            ansi::split_lines(&step.stdout)
                .into_iter()
                .map(DetailLine::Stdout),
        );
        out.extend(
            ansi::split_lines(&step.stderr)
                .into_iter()
                .map(DetailLine::Stderr),
        );
    }
    out
}

/// The whole run as plain text, step headings included, for handing to
/// something that is not this app: an editor, a pager, a paste. Escapes are
/// stripped, as for [`DetailLine::text`].
pub fn transcript(steps: &[StepResult]) -> String {
    let mut text = String::new();
    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        text.push_str(&format!("$ {}  (exit {})\n", step.label, step.code));
        text.push_str(&ansi::strip(&step.stdout));
        text.push_str(&ansi::strip(&step.stderr));
    }
    text
}

/// A string safe to appear in a temp filename: non-alphanumeric characters
/// (including a repo's `/` or `.`) become `-` so the result is always a
/// single path segment.
fn filename_safe(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Write [`transcript`] to a temp file and hand back its path. Named after
/// the repo so a couple of them open at once are still tellable apart.
pub fn write_transcript(steps: &[StepResult], repo: &str) -> std::io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("mrx-{}.log", filename_safe(repo)));
    std::fs::write(&path, transcript(steps))?;
    Ok(path)
}

/// The step whose header is at or above `scroll`: the one the viewport is
/// currently showing, so `y` follows whatever the scroll position last
/// brought into view without a separate key to pick a step.
pub fn step_at_line(lines: &[DetailLine], scroll: usize) -> usize {
    lines
        .iter()
        .take(scroll + 1)
        .filter_map(|line| match line {
            DetailLine::StepHeader { step, .. } => Some(*step),
            _ => None,
        })
        .next_back()
        .unwrap_or(0)
}

/// Clamp a scroll offset so it never scrolls past the point where the last
/// line is still on screen.
pub fn clamp_scroll(offset: usize, total_lines: usize, viewport: usize) -> usize {
    offset.min(total_lines.saturating_sub(viewport.min(total_lines)))
}

/// Copy `text` to the system clipboard via `pbcopy`, `xclip`, or `wl-copy`,
/// whichever is on `PATH`, falling back to a temp file when none is: a
/// clipboard crate is more than one key needs (section 03, "y copies the
/// visible step's output").
pub fn copy_or_save(text: &str, repo: &str, step_label: &str) -> String {
    for (bin, args) in [
        ("pbcopy", &[][..]),
        ("xclip", &["-selection", "clipboard"][..]),
        ("wl-copy", &[][..]),
    ] {
        if let Some(msg) = try_clipboard(bin, args, text) {
            return msg;
        }
    }
    save_to_file(text, repo, step_label)
}

/// Attempt one clipboard binary; `None` covers both "not installed" and "it
/// ran but failed", either of which should fall through to the next one.
fn try_clipboard(bin: &str, args: &[&str], text: &str) -> Option<String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(text.as_bytes()).ok()?;
    let status = child.wait().ok()?;
    status
        .success()
        .then_some("copied to clipboard".to_string())
}

fn save_to_file(text: &str, repo: &str, step_label: &str) -> String {
    let safe_repo = filename_safe(repo);
    let safe_step = filename_safe(step_label);
    let path = std::env::temp_dir().join(format!("mrx-{safe_repo}-{safe_step}.txt"));
    match std::fs::write(&path, text) {
        Ok(()) => format!("no clipboard available, wrote output to {}", path.display()),
        Err(e) => format!("could not copy or save output: {e}"),
    }
}

/// How loudly a stderr line should be drawn. stderr is the "not the data"
/// channel, not an error channel: git's fetch progress and npm's warnings
/// both arrive there, so painting the whole stream red says every run
/// failed. Tools that mean something urgent say so in the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Plain,
    Warn,
    Error,
}

/// Severity markers as tools actually emit them, matched against the first
/// few words only: `npm warn ...`, `fatal: ...`, `[error] ...`. Looking
/// further in would catch a filename or a phrase like "0 errors".
const LEAD_WORDS: usize = 3;

/// Classify a stderr line by the marker it leads with, if any.
pub fn severity(line: &str) -> Severity {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summarize::Shape;

    fn step(label: &str, stdout: &str, stderr: &str, code: i32) -> StepResult {
        StepResult {
            label: label.into(),
            shape: Shape::Generic,
            stdout: stdout.into(),
            stderr: stderr.into(),
            code,
        }
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
        // Only the lead words are inspected, so prose and paths that happen
        // to contain a marker stay plain.
        for line in [
            "Compiling the error-handling crate",
            "wrote report to /tmp/build/error.log",
            "run `npm audit` for details",
        ] {
            assert_eq!(severity(line), Severity::Plain, "{line}");
        }
    }

    #[test]
    fn layout_splits_above_the_breakpoint_and_goes_full_screen_below_it() {
        assert_eq!(layout_for_width(120), DetailLayout::Split);
        assert_eq!(layout_for_width(WIDTH_BREAKPOINT), DetailLayout::Split);
        assert_eq!(
            layout_for_width(WIDTH_BREAKPOINT - 1),
            DetailLayout::FullScreen
        );
    }

    #[test]
    fn detail_lines_separate_each_step_into_its_own_section() {
        let steps = vec![
            step("git pull", "Already up to date.", "", 0),
            step("post_update", "wrote 3 files", "", 0),
        ];
        let lines = detail_lines(&steps);
        assert!(matches!(lines[0], DetailLine::StepHeader { step: 0, .. }));
        assert!(
            lines.iter().any(|l| matches!(l, DetailLine::Blank)),
            "steps are separated by a blank line"
        );
        assert!(matches!(
            lines.last().unwrap(),
            DetailLine::Stdout(s) if s == "wrote 3 files"
        ));
    }

    #[test]
    fn step_at_line_follows_the_scroll_position_into_the_second_step() {
        let steps = vec![
            step("git pull", "one line", "", 0),
            step("post_update", "another line", "", 0),
        ];
        let lines = detail_lines(&steps);
        assert_eq!(step_at_line(&lines, 0), 0);

        let second_header = lines
            .iter()
            .position(|l| matches!(l, DetailLine::StepHeader { step: 1, .. }))
            .unwrap();
        assert_eq!(step_at_line(&lines, second_header), 1);
        assert_eq!(step_at_line(&lines, lines.len() - 1), 1);
    }

    #[test]
    fn clamp_scroll_stops_once_the_last_line_is_on_screen() {
        assert_eq!(clamp_scroll(100, 20, 10), 10);
        assert_eq!(clamp_scroll(2, 20, 10), 2);
        assert_eq!(
            clamp_scroll(5, 3, 10),
            0,
            "content shorter than the viewport never scrolls"
        );
    }

    #[test]
    fn sidebar_width_is_about_a_third_of_the_frame() {
        assert_eq!(sidebar_width(120), 40);
    }

    #[test]
    fn detail_line_text_strips_ansi_escapes() {
        let line = DetailLine::Stdout("\u{1b}[32mgreen\u{1b}[0m text".into());
        assert_eq!(line.text(), "green text");
    }
}
