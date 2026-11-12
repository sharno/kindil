//! Wall-clock time, without pulling in a date/time crate.
//!
//! The Kindle's kernel has no `CLOCK_REALTIME` timezone database, so we read
//! the UTC time from the clock and apply the device's timezone offset, which
//! we take from `/etc/localtime` if the kernel exposes it, falling back to
//! UTC.

use crate::sys::{self, Timespec, CLOCK_REALTIME};

/// Seconds east of UTC.
pub type UtcOffset = i32;

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

/// Read the current wall-clock time, in seconds since the Unix epoch.
pub fn unix_seconds() -> i64 {
    let mut ts = Timespec::default();
    // SAFETY: `ts` is a live `timespec`; the kernel writes both fields.
    let rc = unsafe { sys::clock_gettime(CLOCK_REALTIME, &mut ts) };
    if rc < 0 {
        return 0;
    }
    ts.tv_sec
}

/// The device's UTC offset in seconds, read from `/etc/localtime`.
///
/// The kernel exports the offset in `tm_gmtoff` at the end of the `struct
/// tm`; we read the file and take the trailing 4-byte value.
pub fn utc_offset() -> UtcOffset {
    let mut buf = [0u8; 128];
    let n = read_file("/etc/localtime", &mut buf);
    if n < 8 {
        return 0;
    }
    let off = i32::from_le_bytes([buf[n - 4], buf[n - 3], buf[n - 2], buf[n - 1]]);
    // A plausible offset is within +/- 14 hours.
    if off.abs() > 14 * 3600 {
        0
    } else {
        off
    }
}

/// Read a file into `buf`, returning the number of bytes read.
fn read_file(path: &str, buf: &mut [u8]) -> usize {
    sys::read_file(path, buf)
}

/// Convert Unix seconds to a broken-down local date and time.
///
/// This is the civil-from-days algorithm, which is exact for the proleptic
/// Gregorian calendar.
pub fn civil_from_unix(seconds: i64) -> DateTime {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);

    let hour = (secs_of_day / 3600) as u32;
    let minute = ((secs_of_day % 3600) / 60) as u32;
    let second = (secs_of_day % 60) as u32;

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

    // 1970-01-01 was a Thursday, so weekday 0 = Sunday.
    let weekday = (days + 4).rem_euclid(7) as u32;

    DateTime {
        year: year as i32,
        month: m as u32,
        day: d as u32,
        hour,
        minute,
        second,
        weekday,
    }
}

/// The current local date and time.
pub fn now() -> DateTime {
    civil_from_unix(unix_seconds() + utc_offset() as i64)
}

/// Format a date and time as `YYYY-MM-DD HH:MM:SS`.
pub fn format(dt: &DateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
    )
}

/// The name of a weekday, `Sun`..`Sat`.
pub fn weekday_name(weekday: u32) -> &'static str {
    match weekday {
        0 => "Sun",
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        _ => "Sat",
    }
}

/// The name of a month, `Jan`..`Dec`.
pub fn month_name(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_a_thursday() {
        let dt = civil_from_unix(0);
        assert_eq!(dt.year, 1970);
        assert_eq!(dt.month, 1);
        assert_eq!(dt.day, 1);
        assert_eq!(dt.weekday, 4); // Thursday
    }

    #[test]
    fn known_dates_are_correct() {
        // 2026-11-11T00:00:00Z == 1794355200
        let dt = civil_from_unix(1_794_355_200);
        assert_eq!(dt.year, 2026);
        assert_eq!(dt.month, 11);
        assert_eq!(dt.day, 11);
        assert_eq!(dt.hour, 0);
        assert_eq!(dt.minute, 0);
    }

    #[test]
    fn leap_day_is_handled() {
        // 2024-02-29T12:00:00Z
        let dt = civil_from_unix(1_709_208_000);
        assert_eq!(dt.year, 2024);
        assert_eq!(dt.month, 2);
        assert_eq!(dt.day, 29);
        assert_eq!(dt.hour, 12);
    }

    #[test]
    fn century_non_leap_year_is_correct() {
        // 1900-03-01T00:00:00Z (1900 is not a leap year)
        let dt = civil_from_unix(-2_203_891_200);
        assert_eq!(dt.year, 1900);
        assert_eq!(dt.month, 3);
        assert_eq!(dt.day, 1);
    }

    #[test]
    fn formatting_is_padded() {
        let dt = DateTime {
            year: 2026,
            month: 1,
            day: 2,
            hour: 3,
            minute: 4,
            second: 5,
            weekday: 5,
        };
        assert_eq!(format(&dt), "2026-01-02 03:04:05");
    }

    #[test]
    fn timespec_layout_is_32_bit_clean() {
        assert_eq!(core::mem::size_of::<Timespec>(), 16);
    }
}
