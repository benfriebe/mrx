//! Test fixtures shared by every `state` submodule: the three-repo app
//! `App::new` needs seven arguments to build, and a probe result to feed it.

use super::App;
use crate::config::Repo;
use crate::ui::app::probe::RepoState;
use crate::ui::app::render;
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
        changes: Default::default(),
        present: true,
        timed_out: false,
        fetched: false,
        fetch_head: None,
    }
}
