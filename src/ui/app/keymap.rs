//! The keys each mode binds, in one place, so the footer hint and the `?`
//! overlay cannot describe different keymaps.
//!
//! The footer shows only whole bindings, and only what fits; everything else
//! is bound and documented, just left to the overlay.

use super::state::App;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// As the footer prints them, several alternatives separated by `/`.
    pub keys: &'static str,
    pub label: &'static str,
    /// Whether the footer budgets room for this one, or it is left to the
    /// `?` overlay.
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

    /// Bound and listed under `?`, but never given footer room.
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
    Binding::overlay_only("^d/^u", "half page"),
    Binding::overlay_only("g/G", "first/last"),
    Binding::overlay_only("a", "select all shown"),
    Binding::overlay_only("A", "select whole set"),
    Binding::overlay_only("c", "clear selection"),
    Binding::overlay_only("i", "invert selection"),
    Binding::overlay_only("s/f/d", "status/fetch/diff"),
    Binding::overlay_only("!", "shell in repo"),
    Binding::overlay_only("r", "re-probe"),
    Binding::overlay_only("o", "open in $EDITOR"),
    Binding::overlay_only("F", "freshness poll"),
    Binding::overlay_only("^a", "auto-update"),
    Binding::overlay_only("^r", "reload config"),
    Binding::overlay_only("m", "mouse capture"),
];

pub const DETAIL_KEYS: &[Binding] = &[
    Binding::new("tab", "focus"),
    Binding::new("j/k", "move"),
    Binding::new("^d/^u", "scroll"),
    Binding::new("y", "copy"),
    Binding::new("esc", "back"),
    Binding::new("q", "quit"),
    Binding::overlay_only("enter", "focus output"),
    Binding::overlay_only("o", "open the log"),
    Binding::overlay_only("!", "shell in repo"),
    Binding::overlay_only("^r", "reload config"),
    Binding::overlay_only("m", "mouse capture"),
];

const FILTER_KEYS: &[Binding] = &[Binding::new("esc", "clear"), Binding::new("enter", "keep")];

/// Only bound while a run is live, so it is appended rather than listed: a
/// hint for a key that does nothing is worse than no hint.
const CANCEL: Binding = Binding::new("esc", "cancel");

/// Notes the overlay carries that are not themselves keys, for behaviour a
/// keymap cannot show.
pub const NOTES: &[&str] = &[
    "  an empty selection acts on every repo on screen",
    "  no ↓ count means nothing has fetched this repo yet;",
    "     u, f, F or a pull elsewhere all settle the distance.",
    "  drag the output pane to select lines and copy them on",
    "     release. Mouse capture takes the terminal's own",
    "     selection away; hold option/shift, or m, for it back.",
];

/// What the footer should offer right now. The detail split has one footer
/// rather than one per pane, and it shows the detail view's keys: with the
/// split open, those are what every keystroke reaches, whichever pane the
/// pointer happens to be over.
pub fn bindings_for(app: &App) -> Vec<Binding> {
    if app.filtering {
        return FILTER_KEYS.to_vec();
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
