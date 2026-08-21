//! Test fixtures shared by every `state` submodule: the three-repo app
//! `App::new` needs seven arguments to build, a probe result to feed it, and
//! a timestamp old enough for the code under test to call stale.

use super::App;
use crate::config::Repo;
use crate::ui::app::probe::{Changes, RepoState};
use crate::ui::app::render;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn repo(name: &str) -> Repo {
    Repo {
        name: name.to_string(),
        path: PathBuf::from(format!("/nonexistent/{name}")),
        clone_url: None,
        keys: BTreeMap::default(),
    }
}

pub(crate) fn app(names: &[&str]) -> App {
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

/// A list long enough to scroll in a short terminal, with the window
/// height the app would draw it at.
pub(super) fn scrolling_app(rows: usize, terminal_height: u16) -> (App, usize) {
    let names: Vec<String> = (0..rows).map(|i| format!("repo-{i:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut a = app(&refs);
    a.terminal_height = terminal_height;
    let height = render::list_height(&a, a.terminal_height);
    assert!(height >= 2 && rows > height, "list must actually scroll");
    (a, height)
}

pub(super) fn probed(index: usize, branch: &str) -> RepoState {
    RepoState {
        index,
        branch: Some(branch.to_string()),
        upstream: None,
        ahead: 0,
        behind: 0,
        changed: 0,
        changes: Changes::default(),
        present: true,
        timed_out: false,
        fetched: false,
        fetch_head: None,
    }
}

/// An [`Instant`] `d` in the past, for the tests that need a timestamp
/// something has already had time to age past.
pub(super) fn ago(d: Duration) -> Instant {
    Instant::now()
        .checked_sub(d)
        .expect("a test's offset never reaches back past the monotonic clock's own origin")
}
