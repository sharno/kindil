//! The alarm that wakes a suspended Kindle: on the PW1 the PMIC's RTC
//! (`rtc1`, `pmic_rtc`, an MC34708); the SoC's `rtc0` has no wake alarm.
//!
//! What its driver does (drivers/rtc/rtc-mc34708.c in Amazon's 2.6.31):
//! `set_alarm` writes the alarm's time-of-day and day registers and ignores
//! `enabled`, and `read_alarm` never reports `enabled`. So `wakealarm` in
//! sysfs reads back empty even while armed, and writing `0` to it, which
//! the kernel turns into "disabled, at now + 300 s", arms an alarm five
//! minutes ahead. kindil therefore arms with an absolute time, reads the
//! registers back with `RTC_ALM_READ`, and "disarms" by moving the alarm a
//! year ahead.
use chrono::{Datelike, NaiveDate, Timelike};
use std::path::PathBuf;
use std::time::Duration;

/// A disarmed alarm is moved this far ahead: never, for a nap.
pub const DISARMED: Duration = Duration::from_secs(366 * 24 * 60 * 60);

/// An alarm armed, in the RTC's own seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Armed {
    /// The RTC when arming began.
    pub rtc: i64,
    /// When the alarm is due.
    pub at: i64,
    /// What the alarm registers said afterwards; None when they couldn't be
    /// read (the device node busy, say).
    pub read_back: Option<i64>,
}

impl std::fmt::Display for Armed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "alarm at {} (rtc {} +{} s",
            self.at,
            self.rtc,
            self.at - self.rtc
        )?;
        match self.read_back {
            Some(read) => write!(f, ", read back {read})"),
            None => write!(f, ", not read back)"),
        }
    }
}

/// Whether an alarm armed for `at` can be trusted to wake the device: the
/// registers say `at` (when they could be read), and the RTC, read after
/// arming as `rtc_after`, hasn't reached it.
pub fn check(armed: Armed, rtc_after: i64) -> anyhow::Result<()> {
    if let Some(read) = armed.read_back {
        anyhow::ensure!(
            read == armed.at,
            "the alarm reads back {read}, not the {} armed",
            armed.at
        );
    }
    anyhow::ensure!(
        armed.at > rtc_after,
        "the alarm at {} isn't ahead of the RTC at {rtc_after}",
        armed.at
    );
    Ok(())
}

/// `struct rtc_time` in <linux/rtc.h>: a `struct tm`'s first nine fields.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtcTime(pub [libc::c_int; 9]);

impl RtcTime {
    /// Seconds since the epoch, if the fields make a time.
    pub fn seconds(self) -> Option<i64> {
        let [sec, min, hour, mday, mon, year, ..] = self.0;
        let date = NaiveDate::from_ymd_opt(year + 1900, u32::try_from(mon + 1).ok()?, {
            u32::try_from(mday).ok()?
        })?;
        let time = date.and_hms_opt(
            u32::try_from(hour).ok()?,
            u32::try_from(min).ok()?,
            u32::try_from(sec).ok()?,
        )?;
        Some(time.and_utc().timestamp())
    }

    pub fn of(seconds: i64) -> Option<Self> {
        let at = chrono::DateTime::from_timestamp(seconds, 0)?;
        Some(Self([
            at.second() as libc::c_int,
            at.minute() as libc::c_int,
            at.hour() as libc::c_int,
            at.day() as libc::c_int,
            at.month0() as libc::c_int,
            at.year() - 1900,
            at.weekday().num_days_from_sunday() as libc::c_int,
            at.ordinal0() as libc::c_int,
            0,
        ]))
    }
}

/// The RTC with a wake alarm: the first `rtcN` with a `wakealarm`.
pub struct WakeRtc {
    sys: PathBuf,
    dev: PathBuf,
}

impl WakeRtc {
    pub fn find() -> anyhow::Result<Self> {
        (0..4)
            .map(|n| Self {
                sys: PathBuf::from(format!("/sys/class/rtc/rtc{n}")),
                dev: PathBuf::from(format!("/dev/rtc{n}")),
            })
            .find(|rtc| rtc.sys.join("wakealarm").exists())
            .ok_or_else(|| anyhow::anyhow!("no RTC with a wake alarm"))
    }

    pub fn name(&self) -> String {
        self.sys
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The RTC's time, in its own seconds.
    pub fn now(&self) -> anyhow::Result<i64> {
        let text = std::fs::read_to_string(self.sys.join("since_epoch"))?;
        Ok(text.trim().parse()?)
    }

    fn write_alarm(&self, value: &str) -> anyhow::Result<()> {
        std::fs::write(self.sys.join("wakealarm"), value)
            .map_err(|e| anyhow::anyhow!("writing {value} to {}: {e}", self.name()))
    }

    /// Arms the alarm `after` from now (at least a second), and reads it
    /// back. An error means the alarm can't be trusted: don't suspend.
    pub fn arm(&self, after: Duration) -> anyhow::Result<Armed> {
        let rtc = self.now()?;
        let at = rtc + after.as_secs().max(1) as i64;
        // Over an alarm the kernel thinks pending, a write fails: clear
        // first (here that only moves it, see the module's note).
        self.write_alarm("0")?;
        self.write_alarm(&at.to_string())?;
        let read_back = self.read_alarm().ok();
        let armed = Armed { rtc, at, read_back };
        check(armed, self.now()?)?;
        Ok(armed)
    }

    /// Moves the alarm out of the way: [`DISARMED`] ahead.
    pub fn disarm(&self) -> anyhow::Result<()> {
        let at = self.now()? + DISARMED.as_secs() as i64;
        self.write_alarm("0")?;
        self.write_alarm(&at.to_string())
    }

    /// The alarm registers, through `RTC_ALM_READ` on the device node.
    #[cfg(target_os = "linux")]
    pub fn read_alarm(&self) -> anyhow::Result<i64> {
        use std::os::fd::AsRawFd;
        /// `_IOR('p', 0x08, struct rtc_time)`.
        const RTC_ALM_READ: u32 = (2 << 30)
            | ((std::mem::size_of::<RtcTime>() as u32) << 16)
            | ((b'p' as u32) << 8)
            | 0x08;
        let device = std::fs::File::open(&self.dev)?;
        let mut time = RtcTime::default();
        // SAFETY: the RTC's ioctl writes one rtc_time to a valid pointer.
        if unsafe { libc::ioctl(device.as_raw_fd(), RTC_ALM_READ as _, &mut time) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        time.seconds()
            .ok_or_else(|| anyhow::anyhow!("the alarm reads {time:?}"))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn read_alarm(&self) -> anyhow::Result<i64> {
        anyhow::bail!("no RTC here: {}", self.dev.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_times_round_trip() {
        // rtc1 on the PW1 counts from its last reset: 1970.
        for seconds in [0, 1_380, 86_399, 86_400, 1_795_005_104] {
            let time = RtcTime::of(seconds).unwrap();
            assert_eq!(time.seconds(), Some(seconds));
        }
        assert_eq!(RtcTime::of(1_380).unwrap().0[..6], [0, 23, 0, 1, 0, 70]);
        assert_eq!(RtcTime([0, 0, 0, 0, 0, 70, 0, 0, 0]).seconds(), None);
        assert_eq!(RtcTime([0, 0, 0, 1, -1, 70, 0, 0, 0]).seconds(), None);
    }

    #[test]
    fn an_alarm_is_trusted_only_as_armed_and_ahead() {
        let armed = Armed {
            rtc: 1_380,
            at: 1_420,
            read_back: Some(1_420),
        };
        assert!(check(armed, 1_381).is_ok());
        // Unreadable registers: the time still has to be ahead.
        let unread = Armed {
            read_back: None,
            ..armed
        };
        assert!(check(unread, 1_381).is_ok());
        assert!(check(unread, 1_420).is_err());
        // The registers say something else: a day off, or not written.
        let wrong = Armed {
            read_back: Some(1_420 - 86_400),
            ..armed
        };
        assert!(check(wrong, 1_381).is_err());
        assert!(check(armed, 1_421).is_err(), "passed while arming");
        assert_eq!(
            armed.to_string(),
            "alarm at 1420 (rtc 1380 +40 s, read back 1420)"
        );
    }
}
