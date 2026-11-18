//! kindil's log lines, stamped with the wall time.
//!
//! stderr is kindil.log (the launcher redirects it). Each line goes out in
//! one write, errors ignored: `eprintln!` panics when stderr can't be
//! written (a full /var/local), and a log line must never take kindil down.
//! The stamp is the device's zone once [`set_zone`] has been told it (the
//! launcher's own lines are local time too), UTC with a `Z` before that.
use chrono::{DateTime, Utc};
use std::io::Write;
use std::sync::OnceLock;

static ZONE: OnceLock<chrono_tz::Tz> = OnceLock::new();

/// The zone the stamps are in from now on; the first call wins.
pub fn set_zone(zone: chrono_tz::Tz) {
    let _ = ZONE.set(zone);
}

/// The zone [`set_zone`] was told, if any.
pub fn zone() -> Option<chrono_tz::Tz> {
    ZONE.get().copied()
}

/// `now` as a stamp: `2026-11-17 22:18:04.512`, in `zone`, or in UTC with a
/// `Z` without one.
pub fn stamp(now: DateTime<Utc>, zone: Option<chrono_tz::Tz>) -> String {
    const FORMAT: &str = "%F %T%.3f";
    match zone {
        Some(zone) => now.with_timezone(&zone).format(FORMAT).to_string(),
        None => format!("{}Z", now.format(FORMAT)),
    }
}

/// One stamped line on stderr, in one write; never panics.
pub fn line(args: std::fmt::Arguments<'_>) {
    let text = format!("{} {args}\n", stamp(Utc::now(), zone()));
    let _ = std::io::stderr().lock().write_all(text.as_bytes());
}

/// `eprintln!`, stamped with the wall time and never panicking.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::logging::line(format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn stamps_in_the_devices_zone_or_utc() {
        let at = Utc.with_ymd_and_hms(2026, 11, 18, 2, 18, 4).unwrap()
            + chrono::TimeDelta::milliseconds(512);
        assert_eq!(stamp(at, None), "2026-11-18 02:18:04.512Z");
        assert_eq!(
            stamp(at, Some(chrono_tz::America::Halifax)),
            "2026-11-17 22:18:04.512"
        );
    }
}
