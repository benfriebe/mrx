//! The handoffs to the run loop for things state has no I/O to do itself:
//! editor, shell, transcript, mouse capture.

use super::{App, RunStatus};
use crate::ui::app::detail;
use std::path::PathBuf;

/// A pending request to hand the terminal to something else. Resolved
/// against the cursor when the run loop takes it, not when it is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Foreground {
    /// `$EDITOR` on the cursor row's repo.
    Repo,
    /// `$EDITOR` on a file already written, such as a run transcript.
    Path(PathBuf),
    /// `$SHELL` in the cursor row's repo.
    Shell,
}

/// A resolved [`Foreground`]: the same request with the cursor's repo
/// already looked up, which is all the run loop needs.
pub enum Suspend {
    Editor(PathBuf),
    Shell(PathBuf),
}

impl App {
    /// `m`: hand the mouse to the terminal, or take it back. Says which
    /// state it landed in: with capture off the only other evidence is that
    /// clicking and scrolling quietly stop doing anything.
    pub fn toggle_mouse_capture(&mut self) {
        self.mouse_captured = !self.mouse_captured;
        self.mouse_capture_dirty = true;
        self.status_message = Some(if self.mouse_captured {
            "mouse capture on".into()
        } else {
            "mouse capture off, press m to take it back".into()
        });
    }

    pub fn take_mouse_capture_dirty(&mut self) -> bool {
        std::mem::take(&mut self.mouse_capture_dirty)
    }

    /// `o`: open `$EDITOR` on whatever the current view is about. In the list
    /// that is the cursor row's repo; in the detail view it is the transcript
    /// on screen, since that is what you are looking at and the repo is one
    /// `esc` away.
    ///
    /// A no-op with a status message when the filter hides every row, the
    /// same as [`open_detail`](Self::open_detail). Also refused by
    /// [`mutation_blocker`](Self::mutation_blocker): a live run or
    /// auto-update pass keeps writing to repos in the background while the
    /// editor has the terminal.
    pub fn request_open_editor(&mut self) {
        if self.detail_open {
            self.request_open_transcript();
            return;
        }
        self.request_foreground(Foreground::Repo);
    }

    /// `!`: a shell in the cursor row's repo, for the things no action
    /// covers. Same suspend path as the editor, and refused on the same
    /// grounds.
    pub fn request_shell(&mut self) {
        self.request_foreground(Foreground::Shell);
    }

    /// `o` from the detail view: write the transcript somewhere real and
    /// open that. A file rather than a pipe, so the editor gets a name to
    /// show and the text survives being closed.
    fn request_open_transcript(&mut self) {
        let Some(Some(RunStatus::Finished { steps, .. })) = self.run_results.get(self.cursor)
        else {
            self.status_message = Some("no finished output to open yet".into());
            return;
        };
        let name = self
            .repos
            .get(self.cursor)
            .map_or("repo", |r| r.name.as_str());
        match detail::write_transcript(steps, name) {
            Ok(path) => self.foreground = Some(Foreground::Path(path)),
            Err(e) => self.status_message = Some(format!("could not write the transcript: {e}")),
        }
    }

    fn request_foreground(&mut self, what: Foreground) {
        if self.visible_indices().is_empty() {
            self.status_message = Some(self.no_visible_rows_message());
            return;
        }
        let verb = match what {
            Foreground::Shell => "open a shell",
            _ => "open the editor",
        };
        if self.refuse_if_mutation_blocked(verb) {
            return;
        }
        self.foreground = Some(what);
    }

    /// Resolves against the cursor at the moment it's taken rather than when
    /// it was requested.
    pub fn take_foreground(&mut self) -> Option<Suspend> {
        let repo = self.repos.get(self.cursor);
        match self.foreground.take()? {
            Foreground::Repo => Some(Suspend::Editor(repo?.path.clone())),
            Foreground::Path(path) => Some(Suspend::Editor(path)),
            Foreground::Shell => Some(Suspend::Shell(repo?.path.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::app;
    use super::*;
    use crate::executor::StepResult;
    use crate::summarize;

    #[test]
    fn requesting_the_editor_on_a_zero_match_filter_is_a_no_op() {
        let mut a = app(&["foo"]);
        a.filter = "zzz".into();
        a.request_open_editor();
        assert!(a.foreground.is_none());
        assert!(a.status_message.is_some());
    }

    /// A live run keeps writing to repos in the background while the editor
    /// has the terminal.
    #[test]
    fn requesting_the_editor_is_refused_while_a_run_is_live() {
        let mut a = app(&["foo"]);
        a.begin_named_run("update".into(), vec![0]);
        a.request_open_editor();
        assert!(a.foreground.is_none());
        assert!(a.status_message.is_some());
    }

    #[test]
    fn toggle_mouse_capture_flips_the_flag_and_marks_it_dirty() {
        let mut a = app(&["foo"]);
        assert!(a.mouse_captured, "capture starts on");
        a.toggle_mouse_capture();
        assert!(!a.mouse_captured);
        assert!(a.take_mouse_capture_dirty());
        assert!(!a.take_mouse_capture_dirty(), "only taken once");
        assert_eq!(
            a.status_message.as_deref(),
            Some("mouse capture off, press m to take it back"),
            "the only other sign capture is off is that the mouse stops working"
        );

        a.toggle_mouse_capture();
        assert!(a.mouse_captured);
        assert!(a.take_mouse_capture_dirty());
        assert_eq!(a.status_message.as_deref(), Some("mouse capture on"));
    }

    #[test]
    fn open_editor_resolves_to_the_cursor_repo_at_the_moment_its_taken() {
        let mut a = app(&["foo", "bar"]);
        a.cursor = 1;
        a.request_open_editor();
        assert!(a.foreground.is_some());

        // Moving the cursor before the run loop gets around to taking the
        // request is what "at the moment it's taken" means.
        a.cursor = 0;
        match a.take_foreground() {
            Some(Suspend::Editor(path)) => assert_eq!(path, PathBuf::from("/nonexistent/foo")),
            _ => panic!("expected the editor on the cursor's repo"),
        }
        assert!(a.foreground.is_none(), "only taken once");
        assert!(a.take_foreground().is_none());
    }

    #[test]
    fn bang_asks_for_a_shell_in_the_cursor_repo() {
        let mut a = app(&["foo", "bar"]);
        a.cursor = 1;
        a.request_shell();
        match a.take_foreground() {
            Some(Suspend::Shell(dir)) => assert_eq!(dir, PathBuf::from("/nonexistent/bar")),
            _ => panic!("expected a shell in the cursor's repo"),
        }
    }

    /// In the list `o` means the repo; in the detail view the repo is not
    /// what is on screen, so it means the transcript instead.
    #[test]
    fn o_in_the_detail_view_asks_for_the_log_rather_than_the_repo() {
        let mut a = app(&["foo"]);
        a.detail_open = true;
        a.run_results[0] = Some(RunStatus::Finished {
            steps: vec![StepResult {
                label: "update".into(),
                shape: summarize::Shape::Generic,
                stdout: "Already up to date.\n".into(),
                stderr: String::new(),
                code: 0,
            }],
            exit_code: 0,
        });

        a.request_open_editor();
        match a.take_foreground() {
            Some(Suspend::Editor(path)) => {
                assert!(path.to_string_lossy().ends_with(".log"), "got {path:?}");
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(text.contains("Already up to date."), "got {text:?}");
            }
            _ => panic!("expected the transcript"),
        }
    }

    #[test]
    fn o_in_the_detail_view_says_so_when_there_is_no_output_to_open() {
        let mut a = app(&["foo"]);
        a.detail_open = true;
        a.request_open_editor();
        assert!(a.foreground.is_none());
        assert!(a.status_message.is_some());
    }
}
