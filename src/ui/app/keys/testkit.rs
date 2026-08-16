//! Test fixtures shared by the keys submodules: an app to drive, and the two
//! constructors that turn a key code into an input event.

use crate::config::Repo;
use crate::ui::app::state::App;
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

pub(super) fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

pub(super) fn ctrl(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::CONTROL))
}
