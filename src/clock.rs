//! Keeping time. Amazon's `updatetime` set the clock while its services
//! ran (`ntpdate`, else the `Date` of kindle-time.amazon.com over HTTP, then
//! `hwclock -u -w`); kindil stops them, so it keeps the clock itself:
//!
//! - Online, it asks a few SNTP servers ([`SERVERS`]) and steps the system
//!   clock when at least two agree it is off; with UDP 123 blocked, the
//!   `Date` of an HTTP answer does, more coarsely. Either way the
//!   persistent RTC is written too, so a reboot keeps the time.
//! - Each resume on the PW1's 2.6.31 kernel loses about half a second of
//!   system time (the RTC it restores from counts whole seconds), and
//!   dozing resumes once a minute: measured on 2026-11-17, the clock fell
//!   from 726 s to 755 s behind `rtc0` in 55 minutes of naps, while `rtc0`
//!   stayed within a second of real time. `rtc0` keeps time through a
//!   suspend, so after each nap the system clock catches up with it once it
//!   has fallen a whole second behind.
use chrono::{DateTime, TimeDelta, Utc};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// Asked together; the answer is one that another agrees with.
pub const SERVERS: [&str; 3] = ["pool.ntp.org", "time.google.com", "time.cloudflare.com"];
/// How long each SNTP server has to answer.
pub const NTP_TIMEOUT: Duration = Duration::from_secs(2);
/// Answers within this many seconds of each other agree.
pub const AGREE: f64 = 1.0;
/// Network time steps the clock only when it is off by this many seconds.
pub const NTP_MIN_STEP: f64 = 0.25;
/// An HTTP `Date` counts whole seconds: it steps the clock only when it is
/// off by this many.
pub const HTTP_MIN_STEP: f64 = 2.0;
/// Without SNTP, the `Date` of this (plain HTTP: it needs no clock to
/// check a certificate with), the weather service's.
pub const HTTP_DATE_URL: &str = "http://api.open-meteo.com/";
/// A step larger than this is an answer to distrust, unless the clock is
/// before [`TRUSTED_YEAR`] (a lost RTC: the clock starts in 1970 or at
/// Amazon's epoch).
pub const MAX_STEP: TimeDelta = TimeDelta::days(365);
/// A clock earlier than this year is known to be wrong.
pub const TRUSTED_YEAR: i32 = 2026;
/// The RTC that keeps time through suspends and power-offs: the SoC's
/// `mxc_rtc`, which Amazon's boot reads the time from (`hwclock -u -s` in
/// /etc/upstart/system.conf). `rtc1` (`pmic_rtc`) counts from 1970 and
/// carries the nap's wake alarm.
pub const PERSISTENT_RTC: &str = "rtc0";
/// The most the clock catches up with the RTC after one resume; more says
/// the RTC is wrong, and network time sorts it out.
pub const MAX_CATCH_UP: f64 = 86_400.0;

/// Where a correction came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Ntp(&'static str),
    HttpDate(&'static str),
    Rtc,
}

/// A step of the system clock, in seconds: positive when it was slow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Correction {
    pub seconds: f64,
    pub source: Source,
}

impl Correction {
    /// From network time, rather than the RTC.
    pub fn online(self) -> bool {
        !matches!(self.source, Source::Rtc)
    }

    pub fn delta(self) -> TimeDelta {
        TimeDelta::microseconds((self.seconds * 1e6).round() as i64)
    }
}

impl fmt::Display for Correction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let size = self.seconds.abs();
        let way = if self.seconds >= 0.0 { "slow" } else { "fast" };
        if size >= 10.0 {
            write!(f, "clock was {size:.0} s {way}; ")?;
        } else {
            write!(f, "clock was {size:.1} s {way}; ")?;
        }
        match self.source {
            Source::Ntp(server) => write!(f, "set from {server}"),
            Source::HttpDate(host) => write!(f, "set from the Date of {host}"),
            Source::Rtc => write!(f, "caught up with the RTC"),
        }
    }
}

/// One SNTP server's answer: how far the clock is behind (seconds), and
/// the round trip it took.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub server: &'static str,
    pub offset: f64,
    pub delay: f64,
}

/// The answer to trust: among the largest group of answers within [`AGREE`]
/// of one of them, if it has at least two, the one with the shortest round
/// trip. None when fewer than two servers answered, or no two agree.
pub fn consensus(readings: &[Reading]) -> Option<Reading> {
    let near = |anchor: f64| {
        readings
            .iter()
            .filter(move |r| (r.offset - anchor).abs() <= AGREE)
    };
    let anchor = readings.iter().max_by_key(|a| near(a.offset).count())?;
    let agreeing: Vec<&Reading> = near(anchor.offset).collect();
    if agreeing.len() < 2 {
        return None;
    }
    agreeing
        .into_iter()
        .min_by(|a, b| a.delay.total_cmp(&b.delay))
        .copied()
}

/// What to do with a measured offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    /// Close enough.
    Keep,
    Step(Correction),
    /// Not to be believed: a step of over [`MAX_STEP`] from a clock that
    /// looks right.
    Absurd,
}

/// Whether to step the clock, now `now`, by `offset` seconds measured from
/// `source`; steps under `min_step` aren't worth it.
pub fn decide(now: DateTime<Utc>, offset: f64, min_step: f64, source: Source) -> Decision {
    use chrono::Datelike;
    let lost = now.year() < TRUSTED_YEAR;
    if !offset.is_finite() || (!lost && offset.abs() > MAX_STEP.as_seconds_f64()) {
        return Decision::Absurd;
    }
    if offset.abs() < min_step {
        return Decision::Keep;
    }
    Decision::Step(Correction {
        seconds: offset,
        source,
    })
}

/// How far the clock is behind a server whose `Date` header was `date`,
/// for a request sent at `sent` and answered at `received` (the clock's
/// times). The server's second ended somewhere within it: its middle is
/// the best guess, as is the middle of the round trip.
pub fn http_date_offset(date: &str, sent: DateTime<Utc>, received: DateTime<Utc>) -> Option<f64> {
    let server = DateTime::parse_from_rfc2822(date).ok()?.with_timezone(&Utc);
    let middle = sent + (received - sent) / 2;
    Some((server - middle).as_seconds_f64() + 0.5)
}

/// After a resume: how far to step a clock reading `system` (seconds since
/// the epoch) forward, given the persistent RTC reads `rtc` (whole seconds).
/// Only when it has fallen a whole second behind, so the step is never back:
/// to the middle of the RTC's second.
pub fn catch_up_by(rtc: i64, system: f64) -> Option<f64> {
    let behind = rtc as f64 - system;
    (1.0..=MAX_CATCH_UP)
        .contains(&behind)
        .then_some(behind + 0.5)
}

/// Whether the RTC (whole seconds) should be written to match a clock that
/// is right: when it is off by more than its own resolution.
pub fn rtc_disagrees(rtc: i64, system: f64) -> bool {
    (rtc - system.floor() as i64).abs() >= 2
}

/// The system clock, in seconds since the epoch.
fn system_seconds() -> f64 {
    Utc::now().timestamp_micros() as f64 / 1e6
}

fn rtc_dir() -> std::path::PathBuf {
    Path::new("/sys/class/rtc").join(PERSISTENT_RTC)
}

/// The persistent RTC, whole seconds since the epoch.
pub fn read_rtc() -> std::io::Result<i64> {
    let text = std::fs::read_to_string(rtc_dir().join("since_epoch"))?;
    text.trim()
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Steps the system clock by `seconds`.
fn step(seconds: f64) -> std::io::Result<()> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid timespec to fill and then to read.
    if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let nanos =
        now.tv_sec as i128 * 1_000_000_000 + now.tv_nsec as i128 + (seconds * 1e9).round() as i128;
    let to = libc::timespec {
        tv_sec: nanos.div_euclid(1_000_000_000) as _,
        tv_nsec: nanos.rem_euclid(1_000_000_000) as _,
    };
    // SAFETY: as above.
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &to) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Writes the system clock's time to the persistent RTC, in UTC, at the
/// start of a second (the RTC keeps whole seconds): up to a second's wait.
#[cfg(target_os = "linux")]
fn write_rtc() -> std::io::Result<()> {
    use chrono::{Datelike, Timelike};
    use std::os::fd::AsRawFd;
    /// `struct rtc_time` in <linux/rtc.h>: a `struct tm`'s first nine fields.
    #[repr(C)]
    struct RtcTime([libc::c_int; 9]);
    /// `_IOW('p', 0x0a, struct rtc_time)`.
    const RTC_SET_TIME: u32 = (1 << 30) | (36 << 16) | ((b'p' as u32) << 8) | 0x0a;
    let device = std::fs::OpenOptions::new()
        .write(true)
        .open(Path::new("/dev").join(PERSISTENT_RTC))?;
    let into = Utc::now().timestamp_subsec_micros();
    std::thread::sleep(Duration::from_micros(u64::from(
        1_000_000 - into.min(999_999),
    )));
    let now = Utc::now();
    let tm = RtcTime([
        now.second() as libc::c_int,
        now.minute() as libc::c_int,
        now.hour() as libc::c_int,
        now.day() as libc::c_int,
        now.month0() as libc::c_int,
        now.year() - 1900,
        now.weekday().num_days_from_sunday() as libc::c_int,
        now.ordinal0() as libc::c_int,
        0,
    ]);
    // SAFETY: the RTC's ioctl reads one rtc_time from a valid pointer.
    if unsafe { libc::ioctl(device.as_raw_fd(), RTC_SET_TIME as _, &tm) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn write_rtc() -> std::io::Result<()> {
    Err(std::io::Error::other("no RTC here"))
}

/// Back from a suspend: steps the clock up to the persistent RTC if it has
/// fallen a second behind.
pub fn catch_up() -> Option<Correction> {
    let rtc = read_rtc().ok()?;
    let seconds = catch_up_by(rtc, system_seconds())?;
    match step(seconds) {
        Ok(()) => Some(Correction {
            seconds,
            source: Source::Rtc,
        }),
        Err(error) => {
            eprintln!("kindil: clock: catching up with the RTC: {error}");
            None
        }
    }
}

/// Asks each of [`SERVERS`], together.
fn ask_servers() -> Vec<Reading> {
    std::thread::scope(|scope| {
        let asks: Vec<_> = SERVERS
            .iter()
            .map(|&server| {
                scope.spawn(move || {
                    let mut client = rsntp::SntpClient::new();
                    client.set_timeout(NTP_TIMEOUT);
                    client
                        .synchronize(server)
                        .map(|result| Reading {
                            server,
                            offset: result.clock_offset().as_secs_f64(),
                            delay: result.round_trip_delay().as_secs_f64(),
                        })
                        .map_err(|error| eprintln!("kindil: clock: {server}: {error}"))
                        .ok()
                })
            })
            .collect();
        asks.into_iter()
            .filter_map(|ask| ask.join().ok().flatten())
            .collect()
    })
}

/// The offset by [`HTTP_DATE_URL`]'s `Date`.
fn ask_http_date() -> anyhow::Result<f64> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into();
    let sent = Utc::now();
    let response = agent.head(HTTP_DATE_URL).call()?;
    let received = Utc::now();
    let date = response
        .headers()
        .get("date")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("no Date header"))?;
    http_date_offset(date, sent, received).ok_or_else(|| anyhow::anyhow!("a Date of {date:?}"))
}

/// The host part of [`HTTP_DATE_URL`].
fn http_date_host() -> &'static str {
    HTTP_DATE_URL
        .trim_start_matches("http://")
        .trim_end_matches('/')
}

/// Online: sets the clock from network time if it is off, and the RTC to
/// match. Ok(None) when the clock was right; an error when no source
/// answered, or only an unbelievable one did.
pub fn sync() -> anyhow::Result<Option<Correction>> {
    let readings = ask_servers();
    let (offset, min_step, source) = match consensus(&readings) {
        Some(reading) => (reading.offset, NTP_MIN_STEP, Source::Ntp(reading.server)),
        None => {
            eprintln!(
                "kindil: clock: {} of {} time servers answered, not two agreeing; \
                 trying {HTTP_DATE_URL}",
                readings.len(),
                SERVERS.len()
            );
            (
                ask_http_date()?,
                HTTP_MIN_STEP,
                Source::HttpDate(http_date_host()),
            )
        }
    };
    let correction = match decide(Utc::now(), offset, min_step, source) {
        Decision::Absurd => anyhow::bail!("distrusting a step of {offset:.0} s"),
        Decision::Keep => None,
        Decision::Step(correction) => {
            step(correction.seconds)?;
            Some(correction)
        }
    };
    let rtc = read_rtc();
    let stale = match &rtc {
        Ok(rtc) => correction.is_some() || rtc_disagrees(*rtc, system_seconds()),
        Err(_) => false,
    };
    if stale {
        if let Err(error) = write_rtc() {
            eprintln!("kindil: clock: writing the RTC: {error}");
        }
    }
    Ok(correction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn reading(server: &'static str, offset: f64, delay: f64) -> Reading {
        Reading {
            server,
            offset,
            delay,
        }
    }

    fn at(year: i32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, 11, 17, 3, 39, 15).unwrap()
    }

    #[test]
    fn two_servers_that_agree_are_believed_the_quickest_first() {
        let a = reading("pool.ntp.org", 724.3, 0.120);
        let b = reading("time.google.com", 724.1, 0.030);
        let c = reading("time.cloudflare.com", 724.2, 0.045);
        assert_eq!(consensus(&[a, b, c]), Some(b));
        assert_eq!(consensus(&[a, c]), Some(c));
        // One answer alone, or none, isn't enough.
        assert_eq!(consensus(&[a]), None);
        assert_eq!(consensus(&[]), None);
    }

    #[test]
    fn a_server_out_of_line_is_outvoted_and_two_that_disagree_are_not_believed() {
        let liar = reading("pool.ntp.org", -3_600.0, 0.001);
        let b = reading("time.google.com", 724.1, 0.030);
        let c = reading("time.cloudflare.com", 724.4, 0.045);
        assert_eq!(consensus(&[liar, b, c]), Some(b));
        assert_eq!(consensus(&[liar, b]), None);
        // All three apart: no two agree.
        let d = reading("time.cloudflare.com", 30.0, 0.045);
        assert_eq!(consensus(&[liar, b, d]), None);
    }

    #[test]
    fn steps_are_taken_when_worth_it_and_believable() {
        let ntp = Source::Ntp("time.google.com");
        let step = |seconds| {
            Decision::Step(Correction {
                seconds,
                source: ntp,
            })
        };
        assert_eq!(decide(at(2026), 724.0, NTP_MIN_STEP, ntp), step(724.0));
        assert_eq!(decide(at(2026), -2.5, NTP_MIN_STEP, ntp), step(-2.5));
        assert_eq!(decide(at(2026), 0.1, NTP_MIN_STEP, ntp), Decision::Keep);
        assert_eq!(decide(at(2026), 1.5, HTTP_MIN_STEP, ntp), Decision::Keep);
        // Over a year away, from a clock that looks right: no.
        let two_years = 2.0 * 365.0 * 86_400.0;
        assert_eq!(
            decide(at(2026), two_years, NTP_MIN_STEP, ntp),
            Decision::Absurd
        );
        assert_eq!(
            decide(at(2026), -two_years, NTP_MIN_STEP, ntp),
            Decision::Absurd
        );
        assert_eq!(
            decide(at(2026), f64::NAN, NTP_MIN_STEP, ntp),
            Decision::Absurd
        );
        // A clock back in 1970 after a lost RTC takes any step.
        let fifty_years = 56.0 * 365.0 * 86_400.0;
        assert_eq!(
            decide(at(1970), fifty_years, NTP_MIN_STEP, ntp),
            step(fifty_years)
        );
    }

    #[test]
    fn corrections_say_how_large_and_from_where() {
        let line = |seconds, source| Correction { seconds, source }.to_string();
        assert_eq!(
            line(724.4, Source::Ntp("time.google.com")),
            "clock was 724 s slow; set from time.google.com"
        );
        assert_eq!(
            line(-3.25, Source::HttpDate("api.open-meteo.com")),
            "clock was 3.2 s fast; set from the Date of api.open-meteo.com"
        );
        assert_eq!(
            line(1.4, Source::Rtc),
            "clock was 1.4 s slow; caught up with the RTC"
        );
        assert_eq!(http_date_host(), "api.open-meteo.com");
    }

    #[test]
    fn an_http_date_is_read_against_the_middle_of_the_round_trip() {
        // Measured on 2026-11-17: the Kindle said 03:39:15, the world 03:51:21.
        let sent = at(2026);
        let received = sent + TimeDelta::milliseconds(400);
        let offset = http_date_offset("Tue, 17 Nov 2026 03:51:21 GMT", sent, received).unwrap();
        assert!((offset - 726.3).abs() < 1e-9, "{offset}");
        assert_eq!(http_date_offset("yesterday", sent, received), None);
    }

    #[test]
    fn the_clock_catches_up_with_the_rtc_only_a_whole_second_behind() {
        let rtc = 1_794_887_551;
        // Measured: 726 s behind after two days of naps.
        assert_eq!(catch_up_by(rtc, 1_794_886_825.0), Some(726.5));
        assert_eq!(catch_up_by(rtc, rtc as f64 - 1.0), Some(1.5));
        // Within the RTC's second, or ahead of it: never stepped back.
        assert_eq!(catch_up_by(rtc, rtc as f64 - 0.9), None);
        assert_eq!(catch_up_by(rtc, rtc as f64 + 0.7), None);
        assert_eq!(catch_up_by(rtc, rtc as f64 + 3_600.0), None);
        // An RTC days ahead is wrong itself: network time settles it.
        assert_eq!(catch_up_by(rtc, rtc as f64 - 2.0 * 86_400.0), None);
    }

    /// Asks the real servers; sets nothing.
    #[test]
    #[ignore = "network"]
    fn the_servers_agree_with_each_other_and_with_http() {
        let readings = ask_servers();
        eprintln!("{readings:?}");
        let ntp = consensus(&readings).expect("two servers agreeing");
        let http = ask_http_date().unwrap();
        eprintln!("ntp {ntp:?}, http {http:.3}");
        assert!((ntp.offset - http).abs() < 1.5);
    }

    #[test]
    fn the_rtc_is_written_only_when_off_by_more_than_its_resolution() {
        let rtc = 1_794_887_551;
        assert!(!rtc_disagrees(rtc, rtc as f64 + 0.9));
        assert!(!rtc_disagrees(rtc, rtc as f64 - 0.1));
        assert!(rtc_disagrees(rtc, rtc as f64 + 2.0));
        assert!(rtc_disagrees(rtc, rtc as f64 - 726.0));
    }
}
