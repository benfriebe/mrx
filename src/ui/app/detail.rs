//! The detail view: a repo's run output, opened with Enter on the cursor row
//! and closed with Esc. Steps come from `StepResult` as separately labelled
//! sections rather than one concatenated scrollback (section 02).

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
    /// without the tick, cross or ellipsis a step header is drawn with,
    /// since those are status, not output.
    pub fn text(&self) -> String {
        match self {
            DetailLine::StepHeader { label, .. } => format!("$ {label}"),
            DetailLine::Stdout(s) | DetailLine::Stderr(s) => s.clone(),
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
            step.stdout
                .lines()
                .map(|l| DetailLine::Stdout(l.to_string())),
        );
        out.extend(
            step.stderr
                .lines()
                .map(|l| DetailLine::Stderr(l.to_string())),
        );
    }
    out
}

/// The whole run as plain text, step headings included, for handing to
/// something that is not this app: an editor, a pager, a paste.
pub fn transcript(steps: &[StepResult]) -> String {
    let mut text = String::new();
    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        text.push_str(&format!("$ {}  (exit {})\n", step.label, step.code));
        text.push_str(&step.stdout);
        text.push_str(&step.stderr);
    }
    text
}

/// Write [`transcript`] to a temp file and hand back its path. Named after
/// the repo so a couple of them open at once are still tellable apart.
pub fn write_transcript(steps: &[StepResult], repo: &str) -> std::io::Result<PathBuf> {
    let safe: String = repo
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let path = std::env::temp_dir().join(format!("mrx-{safe}.log"));
    std::fs::write(&path, transcript(steps))?;
    Ok(path)
}

/// The step whose header is at or above `scroll`: the one the viewport is
/// currently showing, so `y` follows whatever the scroll position last
/// brought into view without a separate key to pick a step.
pub fn step_at_line(lines: &[DetailLine], scroll: usize) -> usize {
    let mut current = 0;
    for line in lines.iter().take(scroll + 1) {
        if let DetailLine::StepHeader { step, .. } = line {
            current = *step;
        }
    }
    current
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
    let safe_step: String = step_label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let path = std::env::temp_dir().join(format!("mrx-{repo}-{safe_step}.txt"));
    match std::fs::write(&path, text) {
        Ok(()) => format!("no clipboard available, wrote output to {}", path.display()),
        Err(e) => format!("could not copy or save output: {e}"),
    }
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
}
