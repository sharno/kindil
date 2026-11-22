//! kindil's log lines, stamped with the wall time.
//!
//! stderr is kindil.log (the launcher redirects it). Each line goes out in
//! one write, errors ignored: `eprintln!` panics when stderr can't be
//! written (a full /var/local), and a log line must never take kindil down.
//! The stamp is the device's zone once [`set_zone`] has been told it (the
//! launcher's own lines are local time too), UTC with a `Z` before that.
use chrono::{DateTime, Utc};
#[cfg(feature = "kindle")]
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
    // On the device, a write that can't fail. Elsewhere (tests, previews)
    // eprint!, whose output the test harness captures.
    #[cfg(feature = "kindle")]
    {
        let stderr = std::io::stderr();
        let mut locked = stderr.lock();
        let _ = locked.write_all(text.as_bytes());
        if LINES
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .is_multiple_of(CHECK_EVERY)
        {
            use std::os::fd::AsFd;
            // The launcher opened it to append only: read through a fresh
            // open of the same file.
            let reader = std::fs::File::open("/proc/self/fd/2");
            if let (Ok(fd), Ok(reader)) = (locked.as_fd().try_clone_to_owned(), reader) {
                let _ = trim(&reader, &std::fs::File::from(fd), MAX_BYTES);
            }
        }
    }
    #[cfg(not(feature = "kindle"))]
    eprint!("{text}");
}

/// The launcher's bound on kindil.log (launcher/src/main.rs `LOG_MAX`),
/// which it applies only when a session starts: a doze running for weeks
/// grows it by about 20 KB a day on /var/local, 64 MB and mostly Amazon's.
/// kindil applies it too, every [`CHECK_EVERY`] lines.
pub const MAX_BYTES: u64 = 256 * 1024;
const CHECK_EVERY: u64 = 64;
#[cfg(feature = "kindle")]
static LINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Past `max` bytes, keeps the file's last half (from a line's start), in
/// place: the file is everyone's stderr, opened for appending by the
/// launcher and shared with kindil's children, whose next writes go to
/// its new end. `reader` and `writer` are the same file, opened to read
/// and to append. Whether it was trimmed.
pub fn trim(reader: &std::fs::File, writer: &std::fs::File, max: u64) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom, Write as _};
    let len = writer.metadata()?.len();
    if len <= max {
        return Ok(false);
    }
    let mut reader = reader;
    reader.seek(SeekFrom::Start(len - max / 2))?;
    let mut tail = Vec::new();
    reader.read_to_end(&mut tail)?;
    // From the first whole line.
    let start = tail.iter().position(|&b| b == b'\n').map_or(0, |at| at + 1);
    writer.set_len(0)?;
    let mut writer = writer;
    writer.write_all(&tail[start..])?;
    Ok(true)
}

/// Hands the `log` crate's warnings and errors (the Kindle backend's: an
/// e-ink update the driver refused, say) to [`line`]; the first call wins.
pub fn forward_log() {
    struct Forward;
    impl log::Log for Forward {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Warn
        }
        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                line(format_args!("{}: {}", record.target(), record.args()));
            }
        }
        fn flush(&self) {}
    }
    static FORWARD: Forward = Forward;
    if log::set_logger(&FORWARD).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
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
    fn a_log_past_its_bound_keeps_its_last_half_in_place() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kindil.log");
        // As the launcher opens it: to append only.
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let reader = std::fs::File::open(&path).unwrap();
        let lines: Vec<String> = (0..100).map(|i| format!("line {i:03}\n")).collect();
        file.write_all(lines.concat().as_bytes()).unwrap();
        assert!(!trim(&reader, &file, 10_000).unwrap(), "under its bound");
        assert!(trim(&reader, &file, 400).unwrap());
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(kept.len() <= 200 && kept.starts_with("line ") && kept.ends_with("line 099\n"));
        // Appending goes on at the new end.
        file.write_all(b"line 100\n").unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .ends_with("line 099\nline 100\n"));
    }

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
