//! The run-command prompt (`r`): an arbitrary shell body typed into the app
//! and run as one script against the selection.

use super::App;

impl App {
    /// Open the prompt on an empty buffer. It never resumes the last body:
    /// an arbitrary command re-run by accident is the mistake worth avoiding.
    pub fn open_run_command(&mut self) {
        self.run_command_open = true;
        self.run_command.clear();
    }

    pub fn close_run_command(&mut self) {
        self.run_command_open = false;
    }

    /// Close the prompt and hand the whole buffer to
    /// [`request_run_body`](Self::request_run_body). A body of nothing but
    /// whitespace is a slip rather than a command, so it closes and says so
    /// instead of running `sh` on it.
    pub fn run_command_confirm(&mut self) {
        self.close_run_command();
        if self.run_command.is_blank() {
            self.status_message = Some("nothing to run".into());
            return;
        }
        let body = self.run_command.text().to_string();
        self.request_run_body(&body);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{app, probed};
    use super::App;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Type `body` into the open prompt the way a user would, through the
    /// buffer's own key handling rather than around it.
    fn type_body(a: &mut App, body: &str) {
        for c in body.chars() {
            let code = if c == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            a.run_command
                .on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }

    #[test]
    fn a_confirmed_body_is_requested_as_a_run_carrying_it_verbatim() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main")); // clean and known, so it runs immediately
        a.open_run_command();
        type_body(&mut a, "git fetch\ngit status");
        a.run_command_confirm();

        assert!(!a.run_command_open);
        let req = a.run_requested.expect("a clean selection runs immediately");
        assert_eq!(req.body.as_deref(), Some("git fetch\ngit status"));
        assert_eq!(
            req.action, "git fetch",
            "the label is the body's first line, for the header and the row results"
        );
    }

    #[test]
    fn a_blank_body_runs_nothing_and_leaves_a_status_message() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main"));
        a.open_run_command();
        type_body(&mut a, " \n");
        a.run_command_confirm();

        assert!(!a.run_command_open);
        assert!(a.run_requested.is_none());
        assert!(a.pending_run.is_none());
        assert!(a.status_message.is_some());
    }

    #[test]
    fn a_dirty_selection_confirms_before_running_the_typed_body() {
        let mut a = app(&["foo"]);
        let mut dirty = probed(0, "main");
        dirty.changed = 1;
        a.on_probe(0, dirty);

        a.open_run_command();
        type_body(&mut a, "ls");
        a.run_command_confirm();

        assert!(a.run_requested.is_none(), "must not run before confirming");
        assert_eq!(
            a.pending_run.as_ref().unwrap().body.as_deref(),
            Some("ls"),
            "the body waits on the prompt with the run it belongs to"
        );

        a.confirm_pending_run();
        assert_eq!(a.run_requested.unwrap().body.as_deref(), Some("ls"));
    }

    #[test]
    fn opening_the_prompt_starts_from_an_empty_buffer() {
        let mut a = app(&["foo"]);
        a.open_run_command();
        type_body(&mut a, "rm -rf .");
        a.close_run_command();

        a.open_run_command();
        assert!(a.run_command.text().is_empty());
    }
}
