//! Which mode owns the input, resolved once so the dispatcher, the renderer,
//! the pointer and the footer cannot each answer it differently.

use super::App;

/// The modes ui mode can be in, in the precedence order [`App::mode`] resolves
/// them.
///
/// The flags behind this stay independent: `Help` and `QuitConfirm` layer over
/// whatever they interrupted and hand it back on the way out, so an enum over
/// the flags themselves would have to enumerate those combinations. This names
/// which mode the next keystroke reaches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    QuitConfirm,
    Help,
    RunConfirm,
    SetPicker,
    Palette,
    RunCommand,
    SortMenu,
    Filter,
    Detail,
    List,
}

impl Mode {
    /// Every mode, in precedence order, so a sweep covers the next one added
    /// rather than the ones a test remembered to list.
    pub const ALL: &'static [Self] = &[
        Self::QuitConfirm,
        Self::Help,
        Self::RunConfirm,
        Self::SetPicker,
        Self::Palette,
        Self::RunCommand,
        Self::SortMenu,
        Self::Filter,
        Self::Detail,
        Self::List,
    ];

    /// Whether the pointer reaches the table. A modal draws a `Clear`ed popup
    /// over it, so a click resolving to a row would land on something the user
    /// cannot see. The filter draws inline and leaves its rows on screen, so it
    /// does not block.
    pub fn takes_pointer(self) -> bool {
        matches!(self, Self::Filter | Self::Detail | Self::List)
    }

    /// Whether this mode is a popup drawn over the frame rather than part of
    /// it. Exactly the inverse of [`takes_pointer`](Self::takes_pointer)
    /// today, and named separately because the renderer is asking a different
    /// question: one is "can a click land", the other "is there a popup".
    pub fn is_modal(self) -> bool {
        !self.takes_pointer()
    }
}

impl App {
    /// Which mode owns the input right now.
    ///
    /// The order is the contract: `QuitConfirm` and `Help` are layered over a
    /// mode still open underneath, and `List` is what is left when nothing else
    /// is up.
    pub fn mode(&self) -> Mode {
        if self.quit_pending {
            return Mode::QuitConfirm;
        }
        if self.help_open {
            return Mode::Help;
        }
        if self.pending_run.is_some() {
            return Mode::RunConfirm;
        }
        if self.set_picker_open {
            return Mode::SetPicker;
        }
        if self.palette_open {
            return Mode::Palette;
        }
        if self.run_command_open {
            return Mode::RunCommand;
        }
        if self.sort_menu_open {
            return Mode::SortMenu;
        }
        if self.filtering {
            return Mode::Filter;
        }
        if self.detail_open {
            return Mode::Detail;
        }
        Mode::List
    }
}

#[cfg(test)]
impl App {
    // `PendingRun` is the only mode not named by a bare flag.
    /// Put the app into `mode` by setting the flag that mode reads, so a
    /// sweep over [`Mode::ALL`] drives each one without the test having to
    /// know which field stands for it.
    pub(crate) fn enter_mode(&mut self, mode: Mode) {
        match mode {
            Mode::QuitConfirm => self.quit_pending = true,
            Mode::Help => self.help_open = true,
            Mode::RunConfirm => {
                self.pending_run = Some(super::PendingRun {
                    action: "update".into(),
                    body: None,
                    targets: vec![0],
                    dirty_count: 1,
                    unknown_count: 0,
                    cursor_only: None,
                });
            }
            Mode::SetPicker => self.set_picker_open = true,
            Mode::Palette => self.palette_open = true,
            Mode::RunCommand => self.run_command_open = true,
            Mode::SortMenu => self.sort_menu_open = true,
            Mode::Filter => self.filtering = true,
            Mode::Detail => self.detail_open = true,
            Mode::List => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::app;
    use super::*;

    #[test]
    fn every_mode_is_reachable_and_resolves_to_itself() {
        for &mode in Mode::ALL {
            let mut a = app(&["foo"]);
            a.enter_mode(mode);
            assert_eq!(a.mode(), mode, "{mode:?} does not resolve to itself");
        }
    }

    /// The two layered modes sit over another one rather than replacing it,
    /// which is why they lead the precedence order.
    #[test]
    fn a_layered_mode_wins_over_the_one_it_covers() {
        let mut a = app(&["foo"]);
        a.detail_open = true;
        a.help_open = true;
        assert_eq!(a.mode(), Mode::Help);

        a.quit_pending = true;
        assert_eq!(a.mode(), Mode::QuitConfirm, "quit outranks help");
    }

    #[test]
    fn only_the_list_the_user_can_see_takes_a_pointer() {
        for &mode in Mode::ALL {
            assert_eq!(
                mode.takes_pointer(),
                matches!(mode, Mode::Filter | Mode::Detail | Mode::List),
                "{mode:?}"
            );
            assert_ne!(mode.takes_pointer(), mode.is_modal(), "{mode:?}");
        }
    }
}
