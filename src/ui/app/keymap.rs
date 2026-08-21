//! The keys each mode binds, in one place, so the footer hint and the `?`
//! overlay cannot describe different keymaps.
//!
//! The footer shows only whole bindings, and only what fits; everything else
//! is bound and documented, just left to the overlay.

use super::state::{App, Mode};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    Binding::overlay_only("S", "sort the table"),
    Binding::overlay_only("r", "run a command"),
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
    "  an empty SYNC cell means nothing has fetched this repo yet;",
    "     u, f, F or a pull elsewhere all settle the distance.",
    "  drag the output pane to select lines and copy them on",
    "     release. Mouse capture takes the terminal's own",
    "     selection away; hold option/shift, or m, for it back.",
];

/// All the footer offers under a popup. Every overlay names its own keys
/// inside the popup, where the eye already is, so the footer's job here is to
/// stop advertising list keys the next keystroke will not reach.
const MODAL_KEYS: &[Binding] = &[Binding::new("esc", "close")];

/// What the footer should offer right now, for the mode that actually has the
/// keys. The detail split has one footer rather than one per pane, and it
/// shows the detail view's keys: with the split open, those are what every
/// keystroke reaches, whichever pane the pointer happens to be over.
pub fn bindings_for(app: &App) -> Vec<Binding> {
    let mode = app.mode();
    if mode.is_modal() {
        return MODAL_KEYS.to_vec();
    }
    match mode {
        Mode::Filter => FILTER_KEYS.to_vec(),
        Mode::Detail => DETAIL_KEYS.to_vec(),
        _ => {
            let mut keys = LIST_KEYS.to_vec();
            if app.run_action.is_some() {
                keys.push(CANCEL);
            }
            keys
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::state::testkit::app;

    /// The footer named list keys under every popup, so `q` looked like a
    /// quit while it was really being typed into a filter or swallowed.
    #[test]
    fn the_footer_offers_no_list_key_under_a_modal() {
        for &mode in Mode::ALL.iter().filter(|m| m.is_modal()) {
            let mut a = app(&["foo"]);
            a.enter_mode(mode);
            assert_eq!(
                bindings_for(&a),
                MODAL_KEYS.to_vec(),
                "{mode:?} still advertises another mode's keys"
            );
        }
    }

    #[test]
    fn each_unobscured_mode_gets_its_own_keys() {
        let mut a = app(&["foo"]);
        assert_eq!(bindings_for(&a), LIST_KEYS.to_vec());

        a.enter_mode(Mode::Filter);
        assert_eq!(bindings_for(&a), FILTER_KEYS.to_vec());

        let mut a = app(&["foo"]);
        a.enter_mode(Mode::Detail);
        assert_eq!(bindings_for(&a), DETAIL_KEYS.to_vec());
    }

    /// Cancel is bound only while a run is live, so it is appended rather
    /// than listed: a hint for a key that does nothing is worse than none.
    #[test]
    fn cancel_joins_the_list_keys_only_while_a_run_is_live() {
        let mut a = app(&["foo"]);
        assert!(!bindings_for(&a).contains(&CANCEL));
        a.run_action = Some("update".into());
        assert!(bindings_for(&a).contains(&CANCEL));
    }
}
