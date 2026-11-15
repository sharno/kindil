//! The server's log: a line per event, never a panic, never unbounded.
//!
//! Lines go to a file (`--log`), kept under [`CAP`] bytes by moving it to
//! `<file>.1` (replacing the last one) when full, or else to stderr. Writing
//! errors (a full /var/local) are ignored. What unauthenticated peers cause
//! (refused connections, failed handshakes, timeouts) goes through
//! [`noise`], at most a line per [`QUIET`] with a count of what it held back.
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// The log file's most, before it moves aside.
pub const CAP: u64 = 64 << 10;
/// The least time between two lines of noise.
pub const QUIET: Duration = Duration::from_secs(10);

static FILE: OnceLock<Option<PathBuf>> = OnceLock::new();
static NOISE: Mutex<Throttle> = Mutex::new(Throttle::new(QUIET));

/// Sends the log to `file` rather than stderr; only the first call counts.
pub fn to_file(file: Option<PathBuf>) {
    let _ = FILE.set(file);
}

/// One line, always.
pub fn line(message: std::fmt::Arguments) {
    let text = format!("{} kindil-sshd: {message}\n", timestamp(SystemTime::now()));
    match FILE.get().and_then(Option::as_ref) {
        Some(path) => {
            let _ = append(path, text.as_bytes());
        }
        None => {
            let _ = std::io::stderr().write_all(text.as_bytes());
        }
    }
}

/// One line, unless one went out less than [`QUIET`] ago: then it is only
/// counted, and the next line says how many were held back.
pub fn noise(message: std::fmt::Arguments) {
    let held = match NOISE.lock() {
        Ok(mut throttle) => throttle.admit(Instant::now()),
        Err(_) => return,
    };
    match held {
        None => {}
        Some(0) => line(message),
        Some(held) => line(format_args!("{message} (and {held} more like it)")),
    }
}

fn append(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.len() + bytes.len() as u64 > CAP) {
        let mut old = path.clone().into_os_string();
        old.push(".1");
        std::fs::rename(path, old)?;
    }
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?
        .write_all(bytes)
}

/// At most one event per interval; the rest counted.
pub struct Throttle {
    interval: Duration,
    last: Option<Instant>,
    held: u32,
}

impl Throttle {
    pub const fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
            held: 0,
        }
    }

    /// `Some(held back since the last)` if this one goes out, else None.
    pub fn admit(&mut self, now: Instant) -> Option<u32> {
        match self.last {
            Some(last) if now.duration_since(last) < self.interval => {
                self.held = self.held.saturating_add(1);
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.held))
            }
        }
    }
}

/// `YYYY-MM-DD HH:MM:SS` in UTC (the Kindle's zone isn't known here).
pub fn timestamp(at: SystemTime) -> String {
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_lines_in_utc() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_744_000);
        assert_eq!(timestamp(at), "2026-11-15 12:00:00");
        assert_eq!(timestamp(SystemTime::UNIX_EPOCH), "1970-01-01 00:00:00");
        let leap = SystemTime::UNIX_EPOCH + Duration::from_secs(1_709_208_000 + 3_599);
        assert_eq!(timestamp(leap), "2024-02-29 12:59:59");
    }

    #[test]
    fn lets_one_through_per_interval_and_counts_the_rest() {
        let mut throttle = Throttle::new(Duration::from_secs(10));
        let start = Instant::now();
        assert_eq!(throttle.admit(start), Some(0));
        for i in 1..=5 {
            assert_eq!(throttle.admit(start + Duration::from_secs(i)), None);
        }
        assert_eq!(throttle.admit(start + Duration::from_secs(10)), Some(5));
        assert_eq!(throttle.admit(start + Duration::from_secs(11)), None);
        assert_eq!(throttle.admit(start + Duration::from_secs(25)), Some(1));
    }

    #[test]
    fn keeps_the_file_small() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sshd.log");
        let line = [b'x'; 1000];
        for _ in 0..200 {
            append(&path, &line).unwrap();
        }
        let size = |p: &std::path::Path| std::fs::metadata(p).unwrap().len();
        assert!(size(&path) <= CAP);
        assert!(size(&dir.path().join("sshd.log.1")) <= CAP);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn writes_through_no_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        let path = dir.path().join("sshd.log");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(append(&path, b"line\n").is_err());
        assert!(!target.exists());
    }
}
