// Salah
//
// See LICENSE for more details.
// Copyright (c) 2019-2022 Farhan Ahmed. All rights reserved.
//

//! An Islamic prayer time implementation based on the [Adhan](https://github.com/batoulapps/Adhan) library by Batoul Apps.
//! While it has a lot of commnalities with the interface has
//! been changed slightly to make it more ergonomic and intuitive.
//!
//! ##### Example
//!
//! ```
//! use salah::prelude::*;
//!
//! let istanbul      = Coordinates::new(41.0082, 28.9784).expect("valid coordinates");
//! let date          = NaiveDate::from_ymd_opt(2019, 1, 25).expect("Invalid date provided");
//! let params        = Configuration::with(Method::Turkey, Madhab::Hanafi);
//! let prayers       = PrayerTimes::new(date, istanbul, params);
//!
//! // Every setting can be changed: the rounding and shafaq types are public.
//! let mut custom = Configuration::with(Method::MoonsightingCommittee, Madhab::Shafi);
//! custom.rounding = Rounding::None;
//! custom.shafaq = Shafaq::Ahmer;
//! assert!(PrayerTimes::new(date, istanbul, custom).is_ok());
//! ```
//!
//! ## Crate Features
//!
//! - `std` (default): the standard library's math. Without it the crate is
//!   `no_std` (and needs no allocator), with `libm` for its math:
//!   `default-features = false, features = ["libm"]`.
//! - `serde`: serialization and deserialization, checking coordinates and
//!   angles as they come in.

#![cfg_attr(not(any(feature = "std", test)), no_std)]

#[cfg(not(any(feature = "std", feature = "libm")))]
compile_error!("salah needs the `std` feature, or `libm` for its math without std");

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

mod astronomy;
mod error;
#[cfg(not(feature = "std"))]
mod math;
mod models;
mod schedule;
mod time;

pub use crate::astronomy::qiblah::Qiblah;
pub use crate::error::Error;
pub use crate::models::adjustments::{Adjustment, TimeAdjustment};
pub use crate::models::coordinates::{Coordinates, CoordinatesError};
pub use crate::models::high_altitude_rule::HighLatitudeRule;
pub use crate::models::madhab::Madhab;
pub use crate::models::method::Method;
pub use crate::models::parameters::{AngleError, Configuration, DepressionAngle, Isha, Parameters};
pub use crate::models::polar_circle_resolution::PolarCircleResolution;
pub use crate::models::prayer::Prayer;
pub use crate::models::rounding::Rounding;
pub use crate::models::shafaq::Shafaq;
pub use crate::schedule::{FIRST_DATE, LAST_DATE, PrayerTimes};
pub use chrono::{
    DateTime, Datelike, Duration, NaiveDate, TimeDelta, TimeZone, Timelike, Utc, Weekday,
};

/// A convenience module appropriate for glob imports (`use salah::prelude::*;`).
pub mod prelude {
    #[doc(no_inline)]
    pub use crate::{
        Adjustment, AngleError, Configuration, Coordinates, CoordinatesError, DepressionAngle,
        Error, FIRST_DATE, HighLatitudeRule, Isha, LAST_DATE, Madhab, Method, Parameters,
        PolarCircleResolution, Prayer, PrayerTimes, Qiblah, Rounding, Shafaq, TimeAdjustment,
    };
    #[doc(no_inline)]
    pub use chrono::{
        DateTime, Datelike, Duration, NaiveDate, TimeDelta, TimeZone, Timelike, Utc, Weekday,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::prelude::*;

    /// The six times as "H:MM" in `zone`, Fajr to Isha.
    fn clock<Tz: TimeZone>(times: &PrayerTimes, zone: &Tz) -> [String; 6]
    where
        Tz::Offset: core::fmt::Display,
    {
        [
            Prayer::Fajr,
            Prayer::Sunrise,
            Prayer::Dhuhr,
            Prayer::Asr,
            Prayer::Maghrib,
            Prayer::Isha,
        ]
        .map(|p| {
            times
                .time(p)
                .with_timezone(zone)
                .format("%-k:%M")
                .to_string()
        })
    }

    #[test]
    fn calculate_prayer_times() {
        let date = NaiveDate::from_ymd_opt(2015, 7, 12).unwrap();
        let params = Configuration::with(Method::NorthAmerica, Madhab::Hanafi);
        let coordinates = Coordinates::new(35.7750, -78.6336).unwrap();
        let times = PrayerTimes::new(date, coordinates, params).unwrap();

        assert_eq!(
            clock(&times, &Utc),
            ["8:42", "10:08", "17:21", "22:22", "0:32", "1:57"]
        );
    }

    #[test]
    fn calculate_qiyam_times() {
        // Maghrib 2015-07-13T00:32Z, the next Fajr 08:43Z: the last third
        // of the night begins at 05:59Z.
        let date = NaiveDate::from_ymd_opt(2015, 7, 12).unwrap();
        let params = Configuration::with(Method::NorthAmerica, Madhab::Hanafi);
        let coordinates = Coordinates::new(35.7750, -78.6336).unwrap();
        let times = PrayerTimes::new(date, coordinates, params).unwrap();

        assert_eq!(
            times.time(Prayer::Maghrib).format("%-k:%M").to_string(),
            "0:32"
        );
        assert_eq!(
            times.time(Prayer::Qiyam).format("%-k:%M").to_string(),
            "5:59"
        );
    }

    /// Singapore's method rounds up: salah 0.7 rounded to the nearest
    /// minute first, then added one, so half its times were a minute late.
    #[test]
    fn calculate_times_for_singapore() {
        let mut params = Configuration::with(Method::Singapore, Madhab::Shafi);
        params.high_latitude_rule = HighLatitudeRule::MiddleOfTheNight;
        let date = NaiveDate::from_ymd_opt(2021, 1, 13).unwrap();
        let coordinates = Coordinates::new(1.370844612058886, 103.80145644060552).unwrap();
        let times = PrayerTimes::new(date, coordinates, params).unwrap();
        let sgt = FixedOffset::east_opt(8 * 3600).unwrap();

        assert_eq!(
            clock(&times, &sgt),
            ["5:50", "7:12", "13:15", "16:38", "19:15", "20:29"]
        );
    }

    #[test]
    fn calculate_times_for_jakarta() {
        let mut params = Configuration::with(Method::Egyptian, Madhab::Shafi);

        // The adjustment below are based on the prayer times that are provided
        // on the website (https://www.jadwalsholat.org/). I don't know the exact
        // calculation that this site is using for the prayer times; therefore the
        // use of time adjustment.
        //
        // It would be a good idea to get some more information on how the Fajr
        // and Isha are calculated, since that's where the biggest variance is;
        // however, the other times are within the 2 minute variance. With
        // these adjustments the times are Adhan's.
        params.method_adjustments = Adjustment::new()
            .fajr(-10)
            .sunrise(-2)
            .dhuhr(2)
            .asr(1)
            .maghrib(2)
            .isha(4)
            .done();
        let date = NaiveDate::from_ymd_opt(2021, 1, 12).unwrap();
        let coordinates = Coordinates::new(-6.18233995, 106.84287154).unwrap();
        let times = PrayerTimes::new(date, coordinates, params).unwrap();
        let wib = FixedOffset::east_opt(7 * 3600).unwrap();

        assert_eq!(
            clock(&times, &wib),
            ["4:15", "5:45", "12:03", "15:27", "18:16", "19:31"]
        );
    }

    #[test]
    fn calculate_time_for_kuala_lumpur() {
        let location = Coordinates::new(3.12, 101.69).unwrap();
        let date = NaiveDate::from_ymd_opt(2024, 4, 9).unwrap();
        let params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);

        assert!(PrayerTimes::new(date, location, params).is_ok());
    }
}
