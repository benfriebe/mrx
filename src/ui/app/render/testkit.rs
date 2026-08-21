//! Test fixtures shared by the render submodules: an app to draw, a probe
//! result to fill its rows, and helpers that read a drawn frame back as text.

use super::{draw, sidebar_natural_width, FOOTER_ROWS};
use crate::config::Repo;
use crate::ui::app::detail;
use crate::ui::app::probe;
use crate::ui::app::state::App;
use crate::ui::widgets::display_width;
use ratatui::prelude::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) fn repo(name: &str) -> Repo {
    Repo {
        name: name.to_string(),
        path: PathBuf::from(format!("/nonexistent/{name}")),
        clone_url: None,
        keys: BTreeMap::default(),
    }
}

pub(super) fn app(repos: Vec<Repo>) -> App {
    App::new(
        repos,
        "work".into(),
        4,
        BTreeMap::new(),
        PathBuf::from("/dev/null"),
        false,
        None,
    )
}

pub(super) fn flatten(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// The display column `needle` starts at. Byte offsets would not do: the
/// markers and the ellipsis are multi-byte, so a row's bytes and the terminal
/// cells it occupies diverge before the first column.
pub(super) fn col_of(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .find(needle)
        .map(|byte| display_width(&haystack[..byte]))
}

pub(super) fn probed(branch: &str, changes: probe::Changes, ahead: u32) -> probe::RepoState {
    probe::RepoState {
        index: 0,
        branch: Some(branch.into()),
        upstream: Some(format!("origin/{branch}")),
        ahead,
        behind: 0,
        changed: changes.modified + changes.untracked + changes.deleted,
        changes,
        present: true,
        timed_out: false,
        fetched: true,
        fetch_head: None,
    }
}

/// Every row of a rendered frame, as plain text with trailing blanks trimmed,
/// so a layout assertion runs against what a terminal would actually show.
pub(super) fn frame_rows(app: &App, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The split's two panes, each as its own rows, sliced apart at the
/// vertical rule and stopping above the footer both of them share.
pub(super) fn split_panes(app: &App, width: u16, height: u16) -> (Vec<String>, Vec<String>) {
    let mut rows = frame_rows(app, width, height);
    rows.truncate(rows.len() - FOOTER_ROWS as usize);
    let col = detail::sidebar_width(width, sidebar_natural_width(app)) as usize;
    let cut = |line: &String, range: std::ops::Range<usize>| {
        line.chars()
            .skip(range.start)
            .take(range.len())
            .collect::<String>()
            .trim_end()
            .to_string()
    };
    (
        rows.iter().map(|l| cut(l, 0..col)).collect(),
        rows.iter()
            .map(|l| cut(l, col + 1..width as usize))
            .collect(),
    )
}
