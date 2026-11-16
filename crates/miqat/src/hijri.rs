//! Tabular (arithmetical) Islamic calendar. It can differ from a sighted or Umm al-Qura
//! date by a day, which `hijri_offset_days` corrects.

use chrono::{Datelike, Duration, NaiveDate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HijriDate {
    year: i32,
    /// Index into `MONTHS`; only `from_gregorian` constructs it, always in 0..12.
    month: usize,
    day: u8,
}

const MONTHS: [&str; 12] = [
    "Muharram",
    "Safar",
    "Rabi' al-Awwal",
    "Rabi' al-Thani",
    "Jumada al-Ula",
    "Jumada al-Akhirah",
    "Rajab",
    "Sha'ban",
    "Ramadan",
    "Shawwal",
    "Dhu al-Qa'dah",
    "Dhu al-Hijjah",
];

impl HijriDate {
    pub fn from_gregorian(date: NaiveDate, offset_days: i8) -> Self {
        let date = date + Duration::days(offset_days.into());
        // Days since 0001-01-01 (day 1) shifted to the Julian Day Number.
        let jd = i64::from(date.num_days_from_ce()) + 1_721_425;
        // Integer algorithm from the "Kuwaiti" tabular calendar.
        let l = jd - 1_948_440 + 10_632;
        let n = (l - 1) / 10_631;
        let l = l - 10_631 * n + 354;
        let j = ((10_985 - l) / 5_316) * ((50 * l) / 17_719) + (l / 5_670) * ((43 * l) / 15_238);
        let l = l - ((30 - j) / 15) * ((17_719 * j) / 50) - (j / 16) * ((15_238 * j) / 43) + 29;
        let month = (24 * l) / 709;
        let day = l - (709 * month) / 24;
        let year = 30 * n + j - 30;
        Self {
            year: year as i32,
            // The algorithm yields months 1..=12.
            month: (month - 1).rem_euclid(12) as usize,
            day: day as u8,
        }
    }

    pub fn english(self) -> String {
        format!("{} {} {} AH", self.day, MONTHS[self.month], self.year)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hijri(y: i32, m: u32, d: u32) -> HijriDate {
        HijriDate::from_gregorian(NaiveDate::from_ymd_opt(y, m, d).unwrap(), 0)
    }

    #[test]
    fn known_dates() {
        // 1 Ramadan 1445 was 2024-03-11 in Umm al-Qura; the tabular calendar agrees within a day.
        let r = hijri(2024, 3, 11);
        assert_eq!((r.year, r.month), (1445, 8));
        assert!(r.day <= 2, "{r:?}");
        // 1 Muharram 1448 is around 2026-06-16.
        let m = hijri(2026, 6, 17);
        assert_eq!((m.year, m.month), (1448, 0));
    }

    #[test]
    fn offset_shifts_day() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let base = HijriDate::from_gregorian(date, 0);
        let next = HijriDate::from_gregorian(date, 1);
        assert_ne!(base, next);
    }

    #[test]
    fn renders_names() {
        let h = hijri(2026, 11, 12);
        assert_eq!(h.english(), "1 Jumada al-Akhirah 1448 AH");
    }
}
