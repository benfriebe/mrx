//! The run-command prompt (`r`): an arbitrary shell body typed into the app
//! and run as one script against the selection.

use super::App;

impl App {
    /// Open the prompt on an empty buffer. It never resumes the last body:
    /// an arbitrary command re-run by accident is the mistake worth avoiding.
    pub fn open_run_command(&mut self) {
        self.run_command_open = true;
        self.run_command_input.clear();
    }

    pub fn close_run_command(&mut self) {
        self.run_command_open = false;
    }

    pub fn run_command_push(&mut self, c: char) {
        self.run_command_input.push(c);
    }

    pub fn run_command_backspace(&mut self) {
        self.run_command_input.pop();
    }

    /// Close the prompt and hand the whole buffer to
    /// [`request_run_body`](Self::request_run_body). A body of nothing but
    /// whitespace is a slip rather than a command, so it closes and says so
    /// instead of running `sh` on it.
    pub fn run_command_confirm(&mut self) {
        self.close_run_command();
        let body = self.run_command_input.clone();
        if body.trim().is_empty() {
            self.status_message = Some("nothing to run".into());
            return;
        }
        self.request_run_body(&body);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{app, probed};

    #[test]
    fn a_confirmed_body_is_requested_as_a_run_carrying_it_verbatim() {
        let mut a = app(&["foo"]);
        a.on_probe(0, probed(0, "main")); // clean and known, so it runs immediately
        a.open_run_command();
        for c in "git fetch\ngit status".chars() {
            a.run_command_push(c);
        }
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
        a.run_command_push(' ');
        a.run_command_push('\n');
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
        a.run_command_push('l');
        a.run_command_push('s');
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
        a.run_command_push('x');
        a.close_run_command();

        a.open_run_command();
        assert!(a.run_command_input.is_empty());
    }

    #[test]
    fn backspace_crosses_a_newline_back_into_the_line_above() {
        let mut a = app(&["foo"]);
        a.open_run_command();
        for c in "ls\n".chars() {
            a.run_command_push(c);
        }
        a.run_command_backspace();
        a.run_command_backspace();
        assert_eq!(a.run_command_input, "l");
    }
}
