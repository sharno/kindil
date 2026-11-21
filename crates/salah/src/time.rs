// Salah
//
// See LICENSE for more details.

//! UTC times as whole seconds, the way the calculation adds and rounds them.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use crate::math::Float;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};

use crate::models::rounding::Rounding;

/// A UTC time: whole seconds since 1970-01-01 00:00 UTC.
///
/// Every moment the calculation makes is within a few days of a date in
/// [`FIRST_DATE`](crate::FIRST_DATE)..=[`LAST_DATE`](crate::LAST_DATE),
/// about ±3·10¹⁰ s, so none of the sums below can overflow an i64.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Moment(i64);

/// The most hours from a date's 00:00 UTC a solar event is taken at: the
/// sun's events on a date fall within about a day of it, so this only
/// turns away NaN, infinities and nonsense.
const MAX_HOURS: f64 = 72.0;

impl Moment {
    /// 00:00 UTC on `date`.
    pub fn midnight(date: NaiveDate) -> Moment {
        Moment(date.and_time(NaiveTime::MIN).and_utc().timestamp())
    }

    /// `hours` after 00:00 UTC on `date`, down to the whole second (as
    /// Adhan's TimeComponents); None for no time (NaN: the sun never gets
    /// there) or one more than three days away.
    pub fn at_hours(date: NaiveDate, hours: f64) -> Option<Moment> {
        (-MAX_HOURS..=MAX_HOURS)
            .contains(&hours)
            .then(|| Moment::midnight(date).plus((hours * 3600.0).floor() as i64))
    }

    pub fn plus(self, seconds: i64) -> Moment {
        Moment(self.0 + seconds)
    }

    /// `seconds` later, down to the whole second; `seconds` is a share of a
    /// night, under two days.
    pub fn plus_fraction(self, seconds: f64) -> Moment {
        self.plus(seconds.floor() as i64)
    }

    pub fn plus_minutes(self, minutes: i32) -> Moment {
        self.plus(i64::from(minutes) * 60)
    }

    /// Seconds from `earlier` to `self`.
    pub fn since(self, earlier: Moment) -> i64 {
        self.0 - earlier.0
    }

    /// On a whole minute: the nearest (half a minute up), the next (a whole
    /// minute stays), or left as it is.
    pub fn rounded(self, rounding: Rounding) -> Moment {
        let minute = self.0.div_euclid(60) * 60;
        match rounding {
            Rounding::Nearest => Moment((self.0 + 30).div_euclid(60) * 60),
            Rounding::Up if minute == self.0 => self,
            Rounding::Up => Moment(minute + 60),
            Rounding::None => self,
        }
    }

    /// As a chrono time; None only out of chrono's range, which a moment
    /// near a supported date never is.
    pub fn utc(self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp(self.0, 0)
    }
}

/// Any date prayer times are calculated for.
#[cfg(kani)]
pub(crate) fn any_supported_date() -> NaiveDate {
    use chrono::Datelike;
    let days: i32 = kani::any();
    kani::assume(
        (crate::FIRST_DATE.num_days_from_ce()..=crate::LAST_DATE.num_days_from_ce())
            .contains(&days),
    );
    NaiveDate::from_num_days_from_ce_opt(days).unwrap()
}

/// 00:00 UTC on FIRST_DATE and LAST_DATE, in Unix seconds (a test checks
/// them against chrono).
#[cfg(any(kani, test))]
const SUPPORTED_SECONDS: (i64, i64) = (-30_610_224_000, 32_535_129_600);

#[cfg(kani)]
impl kani::Arbitrary for Moment {
    /// Any moment within five days of the supported dates, without chrono
    /// (which is slow to verify).
    fn any() -> Self {
        let seconds: i64 = kani::any();
        let margin = 5 * 86_400;
        kani::assume(
            (SUPPORTED_SECONDS.0 - margin..=SUPPORTED_SECONDS.1 + margin).contains(&seconds),
        );
        Moment(seconds)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any number of hours (NaN, infinities, any size) on any supported
    /// date is no time or one within three days of its midnight.
    #[kani::proof]
    fn at_hours_is_total_and_near_the_date() {
        let date = any_supported_date();
        let hours: f64 = kani::any();
        if let Some(moment) = Moment::at_hours(date, hours) {
            assert!(moment.since(Moment::midnight(date)).abs() <= 72 * 3600);
        }
    }

    /// Rounding moves a time by under a minute, the right way, onto a whole
    /// minute (or not at all), once; and the result is a chrono time.
    #[kani::proof]
    fn rounding_stays_within_a_minute() {
        let moment: Moment = kani::any();
        let rounding: Rounding = kani::any();
        let rounded = moment.rounded(rounding);
        let moved = rounded.since(moment);
        match rounding {
            Rounding::Nearest => {
                assert!((-29..=30).contains(&moved));
            }
            Rounding::Up => {
                assert!((0..=59).contains(&moved));
            }
            Rounding::None => {
                assert!(moved == 0);
            }
        }
        if rounding != Rounding::None {
            assert!(rounded.0.rem_euclid(60) == 0);
        }
        assert!(rounded.rounded(rounding) == rounded);
        assert!(rounded.utc().is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hms: &str) -> Moment {
        let time: DateTime<Utc> = format!("2015-07-13T{hms}Z").parse().unwrap();
        Moment(time.timestamp())
    }

    #[test]
    fn supported_seconds_are_the_supported_dates() {
        assert_eq!(Moment::midnight(crate::FIRST_DATE).0, SUPPORTED_SECONDS.0);
        assert_eq!(Moment::midnight(crate::LAST_DATE).0, SUPPORTED_SECONDS.1);
    }

    #[test]
    fn rounding_nearest() {
        assert_eq!(at("04:37:30").rounded(Rounding::Nearest), at("04:38:00"));
        assert_eq!(at("04:37:29").rounded(Rounding::Nearest), at("04:37:00"));
    }

    #[test]
    fn rounding_up_is_a_ceiling() {
        // salah 0.7 added a whole minute to a time already on one.
        assert_eq!(at("05:59:20").rounded(Rounding::Up), at("06:00:00"));
        assert_eq!(at("05:59:00").rounded(Rounding::Up), at("05:59:00"));
    }

    #[test]
    fn rounding_none_keeps_the_seconds() {
        assert_eq!(at("05:59:20").rounded(Rounding::None), at("05:59:20"));
    }

    #[test]
    fn hours_are_floored_to_the_second_and_roll_over() {
        let date = NaiveDate::from_ymd_opt(2015, 7, 12).unwrap();
        assert_eq!(
            Moment::at_hours(date, 24.0 + 4.0 / 3600.9),
            Some(at("00:00:03"))
        );
        assert_eq!(Moment::at_hours(date, f64::NAN), None);
        assert_eq!(Moment::at_hours(date, f64::INFINITY), None);
        assert_eq!(Moment::at_hours(date, 1e300), None);
    }
}
