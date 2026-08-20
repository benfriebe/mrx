//! Test fixtures shared by the keys submodules: an app to drive, a probe
//! result to feed it, and the two constructors that turn a key code into an
//! input event.

use crate::config::Repo;
use crate::executor::StepResult;
use crate::summarize::Shape;
use crate::ui::app::probe::RepoState;
use crate::ui::app::state::{App, RunStatus};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
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

pub(super) fn app(names: &[&str]) -> App {
    App::new(
        names.iter().map(|n| repo(n)).collect(),
        "work".into(),
        4,
        BTreeMap::new(),
        PathBuf::from("/dev/null"),
        false,
        None,
    )
}

/// A clean, present repo: the state a run may start against unconfirmed.
pub(super) fn probed(index: usize, branch: &str) -> RepoState {
    RepoState {
        index,
        branch: Some(branch.to_string()),
        upstream: None,
        ahead: 0,
        behind: 0,
        changed: 0,
        changes: Default::default(),
        present: true,
        timed_out: false,
        fetched: false,
        fetch_head: None,
    }
}

/// A finished run on `index` with a `lines`-long transcript, so the output
/// pane has somewhere to scroll.
pub(super) fn ran(app: &mut App, index: usize, lines: usize) {
    app.run_results[index] = Some(RunStatus::Finished {
        steps: vec![StepResult {
            label: "git pull".into(),
            shape: Shape::Generic,
            stdout: (1..=lines).map(|i| format!("line {i}\n")).collect(),
            stderr: String::new(),
            code: 0,
        }],
        exit_code: 0,
    });
}

pub(super) fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

pub(super) fn ctrl(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::CONTROL))
}
