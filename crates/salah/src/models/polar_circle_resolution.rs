// Salah
//
// See LICENSE for more details.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// What to do on a day without the times, inside or near the polar circles:
/// no sunrise or sunset, an Asr the sun can't reach, or times crowding out
/// of order. The rulings of the Muslim World League's Fiqh Council (1986)
/// and others estimate the times from a place or a day that has them.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Default)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum PolarCircleResolution {
    /// No times: the calculation's [`Error`](crate::Error) says why.
    #[default]
    Unresolved,
    /// The times of the nearest latitude that has them on the day (aqrab
    /// al-bilad, "the nearest land"), along the same meridian.
    AqrabBalad,
    /// The times of the nearest day that has them at the place (aqrab
    /// al-ayyam, "the nearest day"), moved to the date.
    AqrabYaum,
}
