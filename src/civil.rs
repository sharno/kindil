//! Proleptic-Gregorian calendar arithmetic, with no external crates.
//!
//! The algorithms are Howard Hinnant's `days_from_civil` / `civil_from_days`,
//! which are exact for the whole range of `i64` seconds that matters here.

/// A broken-down date and time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday.
    pub weekday: u32,
}

/// True if `year` is a leap year in the proleptic Gregorian calendar.
pub const fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days in `month` (1..=12) of `year`.
pub const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a civil date.
pub const fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Seconds since the Unix epoch for a civil date and time, treated as UTC.
pub const fn unix_from_civil(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> i64 {
    days_from_civil(year, month, day) * 86_400
        + hour as i64 * 3600
        + minute as i64 * 60
        + second as i64
}

/// Day of the week for a count of days since the epoch; 0 = Sunday.
pub const fn weekday_from_days(days: i64) -> u32 {
    // 1970-01-01 was a Thursday, so weekday 4.
    (days + 4).rem_euclid(7) as u32
}

/// Convert Unix seconds to a broken-down UTC date and time.
pub fn civil_from_unix(seconds: i64) -> DateTime {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);

    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    DateTime {
        year: year as i32,
        month: m as u32,
        day: d as u32,
        hour: (secs_of_day / 3600) as u32,
        minute: ((secs_of_day % 3600) / 60) as u32,
        second: (secs_of_day % 60) as u32,
        weekday: weekday_from_days(days),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_a_thursday() {
        let dt = civil_from_unix(0);
        assert_eq!((dt.year, dt.month, dt.day), (1970, 1, 1));
        assert_eq!(dt.weekday, 4);
    }

    #[test]
    fn known_dates_are_correct() {
        // 2026-11-11T00:00:00Z
        let dt = civil_from_unix(1_794_355_200);
        assert_eq!((dt.year, dt.month, dt.day), (2026, 11, 11));
        assert_eq!((dt.hour, dt.minute, dt.second), (0, 0, 0));
    }

    #[test]
    fn leap_day_is_handled() {
        let dt = civil_from_unix(1_709_208_000); // 2024-02-29T12:00:00Z
        assert_eq!((dt.year, dt.month, dt.day), (2024, 2, 29));
        assert_eq!(dt.hour, 12);
    }

    #[test]
    fn century_non_leap_year_is_correct() {
        let dt = civil_from_unix(-2_203_891_200); // 1900-03-01T00:00:00Z
        assert_eq!((dt.year, dt.month, dt.day), (1900, 3, 1));
    }

    #[test]
    fn civil_round_trips() {
        for &(y, m, d) in &[
            (1970, 1, 1),
            (1999, 12, 31),
            (2000, 2, 29),
            (2026, 11, 11),
            (2038, 1, 19),
            (2100, 3, 1),
        ] {
            let secs = unix_from_civil(y, m, d, 13, 37, 42);
            let dt = civil_from_unix(secs);
            assert_eq!((dt.year, dt.month, dt.day), (y, m, d), "{y}-{m}-{d}");
            assert_eq!((dt.hour, dt.minute, dt.second), (13, 37, 42));
        }
    }

    #[test]
    fn month_lengths() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2025, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2026, 4), 30);
    }
}
