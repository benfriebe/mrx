//! The keys each mode binds, in one place, so the footer hint and the `?`
//! overlay cannot describe different keymaps.
//!
//! The footer shows only what fits and only whole bindings, so the set it
//! advertises is the handful worth the room. Everything else is bound and
//! documented, just left to the overlay.

use super::state::App;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// As the footer prints them, several alternatives separated by `/`.
    pub keys: &'static str,
    pub label: &'static str,
    /// Whether the footer budgets room for this one, as opposed to a binding
    /// only the `?` overlay lists.
    pub hinted: bool,
}

impl Binding {
    const fn new(keys: &'static str, label: &'static str) -> Self {
        Self {
            keys,
            label,
            hinted: true,
        }
    }

    /// Bound and listed under `?`, but never given footer room: a key that
    /// would spend more of a narrow line than it earns there.
    const fn overlay_only(keys: &'static str, label: &'static str) -> Self {
        Self {
            keys,
            label,
            hinted: false,
        }
    }
}

pub const HELP: Binding = Binding::new("?", "help");

/// Hinted bindings lead, in the order a narrow footer should shed them from
/// the right: whatever fits, fits.
pub const LIST_KEYS: &[Binding] = &[
    Binding::new("j/k", "move"),
    Binding::new("space", "select"),
    Binding::new("/", "filter"),
    Binding::new("enter", "output"),
    Binding::new("u", "update"),
    Binding::new(":", "action"),
    Binding::new("q", "quit"),
    Binding::new("tab", "set"),
    Binding::overlay_only("g/G", "first/last"),
    Binding::overlay_only("a/A", "select all/none"),
    Binding::overlay_only("i", "invert selection"),
    Binding::overlay_only("s/f/d", "status/fetch/diff"),
    Binding::overlay_only("r", "re-probe"),
    Binding::overlay_only("o", "open in $EDITOR"),
    Binding::overlay_only("F", "freshness poll"),
    Binding::overlay_only("^a", "auto-update"),
    Binding::overlay_only("^r", "reload config"),
    Binding::overlay_only("m", "mouse capture"),
];

pub const DETAIL_KEYS: &[Binding] = &[
    Binding::new("j/k", "move"),
    Binding::new("^d/^u", "scroll"),
    Binding::new("y", "copy"),
    Binding::new("esc", "back"),
    Binding::new("q", "quit"),
    Binding::overlay_only("o", "open in $EDITOR"),
    Binding::overlay_only("^r", "reload config"),
    Binding::overlay_only("m", "mouse capture"),
];

const FILTER_KEYS: &[Binding] = &[Binding::new("esc", "clear"), Binding::new("enter", "keep")];

const SIDEBAR_KEYS: &[Binding] = &[Binding::new("j/k", "move"), Binding::new("esc", "back")];

/// Only bound while a run is live, so it is appended rather than listed: a
/// hint for a key that does nothing is worse than no hint.
const CANCEL: Binding = Binding::new("esc", "cancel");

/// Notes the overlay carries that are not themselves keys, for behaviour a
/// keymap cannot show.
pub const NOTES: &[&str] = &[
    "  an empty selection acts on the row under the cursor",
    "  ↓? means this repo has not been fetched, so its",
    "     behind count is unknown. F starts the poll.",
    "  with mouse capture on, hold your terminal's modifier",
    "     (option, or shift) to select text as usual",
];

/// What the footer should offer right now. `sidebar` distinguishes the
/// narrow list beside an open detail view from the full-width one.
pub fn bindings_for(app: &App, sidebar: bool) -> Vec<Binding> {
    if app.filtering {
        return FILTER_KEYS.to_vec();
    }
    if sidebar {
        return SIDEBAR_KEYS.to_vec();
    }
    if app.detail_open {
        return DETAIL_KEYS.to_vec();
    }
    let mut keys = LIST_KEYS.to_vec();
    if app.run_action.is_some() {
        keys.push(CANCEL);
    }
    keys
}
