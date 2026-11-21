//! Islamic prayer times, verified.
//!
//! ```
//! use jiff::civil::date;
//! use tawqit::{Coordinates, Day, Method, Prayer, PrayerDate};
//!
//! let istanbul = Coordinates::from_degrees(41.0082, 28.9784).expect("a place");
//! let day = PrayerDate::new(date(2026, 3, 20)).expect("a supported date");
//! match Day::calculate(day, istanbul, &Method::Turkey.parameters()) {
//!     Day::Complete(schedule) => {
//!         for prayer in Prayer::ALL {
//!             println!("{}: {}", prayer.name(), schedule.at(prayer));
//!         }
//!     }
//!     Day::Partial(partial) => println!("{:?}", partial.outcome(Prayer::Fajr)),
//! }
//! ```
//!
//! - **What a time is** is in its type: [`Time::Computed`] from the sun,
//!   [`Time::Fallback`] when a rule stood in (and which), and
//!   [`Unavailable`] with the reason when there is none. A [`Day`] is a
//!   [`Schedule`] (all six, in order) or a [`PartialDay`].
//! - **Every input is checked once**, by its type's constructor
//!   ([`Latitude::new`], [`DepressionAngle::new`], [`PrayerDate::new`], …);
//!   after that the calculation is total: no panics, no errors.
//! - **Verified**: everything after the astronomy is an integer core proven
//!   with Verus; the astronomy's conversions into it are proven with Kani;
//!   the astronomy is tested against Adhan and NOAA. See VERIFICATION.md.
//!
//! Times are [`jiff::Timestamp`]s, days [`jiff::civil::Date`]s; a local
//! date's times need its [`jiff::tz::TimeZone`] ([`Day::on_local_date`]).
//!
//! ## Features
//!
//! - `std` (default). Without it the crate is `no_std` (no allocator
//!   either), with `libm` for its math: `default-features = false,
//!   features = ["libm"]`.
//! - `serde`: serialization; deserializing checks every value.
//!
//! ## Origins
//!
//! tawqit began from [salah](https://github.com/insha/salah) by Farhan
//! Ahmed, itself a port of [Adhan](https://github.com/batoulapps/Adhan); its
//! astronomy is Jean Meeus's *Astronomical Algorithms*.

#![cfg_attr(not(any(feature = "std", test)), no_std)]

#[cfg(not(any(feature = "std", feature = "libm")))]
compile_error!("tawqit needs the `std` feature, or `libm` for its math without std");

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

mod astronomy;
mod date;
mod geo;
#[cfg(not(feature = "std"))]
mod math;
mod observe;
mod parameters;
mod schedule;
mod verified;

pub use crate::astronomy::qiblah::Qiblah;
pub use crate::date::{DateOutOfRange, PrayerDate};
pub use crate::geo::{
    Coordinates, CoordinatesError, Latitude, LatitudeError, Longitude, LongitudeError,
};
pub use crate::parameters::{
    AngleError, DepressionAngle, HighLatitudeRule, IntervalError, Isha, IshaInterval, Madhab,
    Maghrib, Method, MinuteOffset, NightBound, OffsetError, Offsets, Parameters,
    PolarCircleResolution, Rounding, Shafaq, presets,
};
pub use crate::schedule::{
    Day, Fallback, Mark, Night, Outcome, PartialDay, Prayer, Schedule, Time, Unavailable,
};
