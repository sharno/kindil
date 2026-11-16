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
    /// The wall dashboard redrew in full.
    DashboardFrame,
    /// Only the dashboard's clock and countdown redrew: it ghosts like a
    /// frame, but the flash waits for the next full one, so a minute tick
    /// stays a small partial update.
    DashboardHeader,
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
    /// Half an hour of minute updates, full or header-only.
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
            Update::DashboardFrame | Update::DashboardHeader => (
                Self {
                    dashboard_frames: self.dashboard_frames.saturating_add(1),
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

    #[test]
    fn minute_ticks_count_but_flash_only_with_a_full_frame() {
        // Four ticks, then a full frame, as a dozing dashboard draws.
        let doze = std::iter::repeat_n(
            [Update::DashboardHeader; 4]
                .into_iter()
                .chain([Update::DashboardFrame]),
            12,
        )
        .flatten();
        let seen = refreshes(doze);
        let full: Vec<usize> = seen
            .iter()
            .enumerate()
            .filter(|(_, r)| **r == Refresh::Full)
            .map(|(i, _)| i + 1)
            .collect();
        // Every half hour of minutes, on the full frame that ends it.
        assert_eq!(full, [30, 60]);
        // Ticks alone never flash.
        let ticks = refreshes(std::iter::repeat_n(Update::DashboardHeader, 100));
        assert!(ticks.iter().all(|r| *r == Refresh::Partial));
    }
}
