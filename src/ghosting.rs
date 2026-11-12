//! When to flash the e-ink panel clean.
//!
//! Partial updates are fast but leave faint ghosts that build up. E-readers
//! hide this with an occasional full (GC16) refresh; this is that policy, kept
//! pure so it can be tested without a panel.

/// What just changed on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Update {
    /// A different page: most of the panel changes, so flash now.
    Navigate,
    PageTurn,
    /// The wall dashboard redrew (once a minute).
    DashboardFrame,
    /// The user asked for it.
    Clean,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refresh {
    Partial,
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ghosting {
    page_turns: u16,
    dashboard_frames: u16,
}

impl Ghosting {
    pub const PAGE_TURNS: u16 = 6;
    /// Half an hour of minute updates.
    pub const DASHBOARD_FRAMES: u16 = 30;

    pub fn after(self, update: Update) -> (Self, Refresh) {
        match update {
            Update::Navigate | Update::Clean => (Self::default(), Refresh::Full),
            Update::PageTurn if self.page_turns + 1 >= Self::PAGE_TURNS => (
                Self {
                    page_turns: 0,
                    ..self
                },
                Refresh::Full,
            ),
            Update::PageTurn => (
                Self {
                    page_turns: self.page_turns + 1,
                    ..self
                },
                Refresh::Partial,
            ),
            Update::DashboardFrame if self.dashboard_frames + 1 >= Self::DASHBOARD_FRAMES => (
                Self {
                    dashboard_frames: 0,
                    ..self
                },
                Refresh::Full,
            ),
            Update::DashboardFrame => (
                Self {
                    dashboard_frames: self.dashboard_frames + 1,
                    ..self
                },
                Refresh::Partial,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refreshes(updates: impl IntoIterator<Item = Update>) -> Vec<Refresh> {
        updates
            .into_iter()
            .scan(Ghosting::default(), |state, update| {
                let (next, refresh) = state.after(update);
                *state = next;
                Some(refresh)
            })
            .collect()
    }

    #[test]
    fn every_sixth_page_turn_flashes() {
        let turns = refreshes(std::iter::repeat_n(Update::PageTurn, 12));
        let full: Vec<usize> = turns
            .iter()
            .enumerate()
            .filter(|(_, r)| **r == Refresh::Full)
            .map(|(i, _)| i + 1)
            .collect();
        assert_eq!(full, [6, 12]);
    }

    #[test]
    fn navigation_flashes_and_resets_the_count() {
        let updates = [Update::PageTurn; 5]
            .into_iter()
            .chain([Update::Navigate])
            .chain([Update::PageTurn; 5]);
        assert_eq!(
            refreshes(updates)
                .iter()
                .filter(|r| **r == Refresh::Full)
                .count(),
            1
        );
    }

    #[test]
    fn dashboard_flashes_every_half_hour() {
        let frames = refreshes(std::iter::repeat_n(Update::DashboardFrame, 60));
        assert_eq!(frames.iter().filter(|r| **r == Refresh::Full).count(), 2);
    }
}
