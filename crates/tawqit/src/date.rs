//! When: the date whose prayer times are calculated.

use core::fmt;

use jiff::civil::Date;

use crate::verified::Civil;

/// A date prayer times are calculated for: 1000-01-01 to 3000-12-31, the
/// centuries Meeus's astronomical series are made for (and far from every
/// limit of the calculation).
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Copy, Clone, Hash)]
pub struct PrayerDate {
    date: Date,
    civil: Civil,
}

/// A date outside 1000-01-01 to 3000-12-31.
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub struct DateOutOfRange(pub Date);

impl PrayerDate {
    /// The first date.
    pub const FIRST: Date = jiff::civil::date(1000, 1, 1);
    /// The last date.
    pub const LAST: Date = jiff::civil::date(3000, 12, 31);

    /// `date`, if from 1000-01-01 to 3000-12-31.
    pub fn new(date: Date) -> Result<PrayerDate, DateOutOfRange> {
        if !(Self::FIRST..=Self::LAST).contains(&date) {
            return Err(DateOutOfRange(date));
        }
        // jiff's year, month and day are a date; the verified calendar
        // agrees (tests/properties.rs checks every date's day number with
        // jiff's).
        match Civil::new(i32::from(date.year()), date.month() as u8, date.day() as u8) {
            Some(civil) => Ok(PrayerDate { date, civil }),
            None => Err(DateOutOfRange(date)),
        }
    }

    pub fn date(&self) -> Date {
        self.date
    }

    pub(crate) fn civil(&self) -> Civil {
        self.civil
    }
}

impl TryFrom<Date> for PrayerDate {
    type Error = DateOutOfRange;

    fn try_from(date: Date) -> Result<Self, DateOutOfRange> {
        PrayerDate::new(date)
    }
}

impl From<PrayerDate> for Date {
    fn from(date: PrayerDate) -> Date {
        date.date
    }
}

impl fmt::Display for DateOutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is outside the dates prayer times are calculated for ({} to {})",
            self.0,
            PrayerDate::FIRST,
            PrayerDate::LAST
        )
    }
}

impl core::error::Error for DateOutOfRange {}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn dates_outside_the_supported_years_are_errors() {
        // salah 0.7 panicked in its date arithmetic at chrono's last date.
        for d in [Date::MIN, Date::MAX, date(999, 12, 31), date(3001, 1, 1)] {
            assert_eq!(PrayerDate::new(d), Err(DateOutOfRange(d)));
        }
        assert!(PrayerDate::new(PrayerDate::FIRST).is_ok());
        assert!(PrayerDate::new(PrayerDate::LAST).is_ok());
    }

    /// The verified calendar and jiff's agree on every supported date: each
    /// is a `PrayerDate` (so `new` accepts exactly the supported dates: the
    /// domain is finite, and this goes through all of it), the day number
    /// counts, the day of the year matches.
    #[test]
    fn the_calendar_agrees_with_jiff_on_every_date() {
        let mut d = PrayerDate::FIRST;
        let mut expected = PrayerDate::new(d).unwrap().civil().number();
        loop {
            let civil = PrayerDate::new(d).unwrap().civil();
            assert_eq!(civil.number(), expected, "{d}");
            assert_eq!(
                i32::from(civil.day_of_year()),
                i32::from(d.day_of_year()),
                "{d}"
            );
            if d == PrayerDate::LAST {
                break;
            }
            d = d.tomorrow().unwrap();
            expected += 1;
        }
    }

    #[test]
    fn day_numbers_count_from_1970() {
        let number = |d| PrayerDate::new(d).unwrap().civil().number();
        assert_eq!(number(date(1970, 1, 1)), 0);
        assert_eq!(number(date(2000, 1, 1)), 10957);
        assert_eq!(number(date(1000, 1, 1)), -354285);
        assert_eq!(number(date(3000, 12, 31)), 376564);
    }
}
