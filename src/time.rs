//! The wall clock, without a date/time crate.

use crate::civil::civil_from_unix;
use crate::sys::{self, Timespec, CLOCK_REALTIME};
use crate::tz::Tz;

/// A clock with a fixed timezone.
pub struct Clock {
    tz: Tz,
}

impl Clock {
    /// The device clock, with the timezone loaded from the environment.
    pub fn system() -> Self {
        Clock { tz: Tz::system() }
    }

    /// A clock with an explicit timezone.
    pub fn with_tz(tz: Tz) -> Self {
        Clock { tz }
    }

    /// The current local date and time.
    pub fn now(&self) -> crate::civil::DateTime {
        let unix = unix_seconds();
        civil_from_unix(unix + self.tz.offset_at(unix) as i64)
    }
}

/// Seconds since the Unix epoch, or 0 if the clock cannot be read.
#[allow(clippy::useless_conversion)] // `c_long` is i64 on the host, i32 on the device
pub fn unix_seconds() -> i64 {
    let mut ts = Timespec::default();
    // SAFETY: `ts` is a live `timespec` and the kernel writes both fields.
    let rc = unsafe { sys::clock_gettime(CLOCK_REALTIME, &mut ts) };
    if rc < 0 {
        return 0;
    }
    // `tv_sec` is `c_long`: 32-bit on the device, 64-bit on the host, so the
    // widening has to be explicit but `From` covers both.
    i64::from(ts.tv_sec)
}

/// Format a date and time as `YYYY-MM-DD HH:MM:SS`.
pub fn format(dt: &crate::civil::DateTime) -> String {
    std::format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year,
        dt.month,
        dt.day,
        dt.hour,
        dt.minute,
        dt.second
    )
}

/// Format just the time as `HH:MM`.
pub fn format_time(dt: &crate::civil::DateTime) -> String {
    std::format!("{:02}:{:02}", dt.hour, dt.minute)
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
    use crate::civil::{unix_from_civil, DateTime};

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
        assert_eq!(format_time(&dt), "03:04");
    }

    #[test]
    fn names_cover_the_week_and_year() {
        assert_eq!(weekday_name(0), "Sun");
        assert_eq!(weekday_name(6), "Sat");
        assert_eq!(month_name(1), "Jan");
        assert_eq!(month_name(12), "Dec");
    }

    #[test]
    fn the_clock_agrees_with_std() {
        // `std` makes its own `clock_gettime` call with the correct `timespec`
        // layout for the target. If our `Timespec` were the wrong size, the
        // kernel would write the nanoseconds into the high half of `tv_sec`
        // and this would be wildly off.
        let ours = unix_seconds();
        let theirs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            (ours - theirs).abs() <= 2,
            "our clock says {ours}, std says {theirs}"
        );
    }

    #[test]
    fn a_clock_applies_its_timezone() {
        // 2026-07-11T22:30:00Z in CEST (UTC+2) is 2026-07-12T00:30 local.
        let tz = Tz::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        let unix = unix_from_civil(2026, 7, 11, 22, 30, 0);
        let local = civil_from_unix(unix + tz.offset_at(unix) as i64);
        assert_eq!(local.day, 12);
        assert_eq!(local.hour, 0);
        assert_eq!(local.minute, 30);
    }
}
