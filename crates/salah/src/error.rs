// Salah
//
// See LICENSE for more details.

//! Why prayer times can't be calculated.

use core::fmt;

use chrono::NaiveDate;

/// Why the prayer times of a day can't be calculated.
///
/// Each variant names the UTC date it is about, which can be the day after
/// the requested one: a day's times need the next day's sunrise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The date is outside the years the calculations are made for,
    /// [`FIRST_DATE`](crate::FIRST_DATE) to [`LAST_DATE`](crate::LAST_DATE).
    DateOutOfRange(NaiveDate),
    /// The sun stays below the horizon all day (polar night).
    SunNeverRises(NaiveDate),
    /// The sun stays above the horizon all day (midnight sun).
    SunNeverSets(NaiveDate),
    /// The sun doesn't get high enough for the Asr shadow, or only at noon
    /// (near the polar circles in winter).
    NoAsr(NaiveDate),
    /// The day's times aren't in order at least five minutes apart: near
    /// the polar circles, where the sun only just rises or sets.
    TimesOutOfOrder(NaiveDate),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::DateOutOfRange(date) => write!(
                f,
                "{date} is outside the dates prayer times are calculated for ({} to {})",
                crate::FIRST_DATE,
                crate::LAST_DATE
            ),
            Error::SunNeverRises(date) => write!(f, "the sun doesn't rise on {date}"),
            Error::SunNeverSets(date) => write!(f, "the sun doesn't set on {date}"),
            Error::NoAsr(date) => {
                write!(f, "the sun doesn't get high enough for Asr on {date}")
            }
            Error::TimesOutOfOrder(date) => {
                write!(f, "the prayer times on {date} are out of order")
            }
        }
    }
}

impl core::error::Error for Error {}
