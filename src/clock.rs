//! Keeping time. Amazon's `updatetime` set the clock while its services
//! ran (`ntpdate`, else the `Date` of kindle-time.amazon.com over HTTP, then
//! `hwclock -u -w`); kindil stops them, so it keeps the clock itself:
//!
//! - Online, it asks a few SNTP servers ([`SERVERS`]) and steps the system
//!   clock when at least two agree it is off; with UDP 123 blocked, the
//!   `Date` of an HTTP answer does, more coarsely. Either way the
//!   persistent RTC is written too, so a reboot keeps the time.
//! - Each resume on the PW1's 2.6.31 kernel moves the system clock on by
//!   the whole seconds `rtc0` counted (drivers/rtc/class.c `rtc_resume`):
//!   off by up to a second either way per resume, and not at random, since
//!   the nap's alarm (`rtc1`) wakes at the same point of `rtc0`'s second
//!   each time. On 2026-11-17 the clock fell 29 s behind in 55 minutes of
//!   naps; that night, with `rtc0` written again, it ran 20 to 40 s an hour
//!   fast instead. `rtc0` keeps time through a suspend, so after each nap
//!   the system clock is put back within `rtc0`'s second, either way
//!   ([`catch_up_by`]); where that second begins on a right clock is
//!   measured after each network sync ([`RtcOffset`]).
use chrono::{DateTime, TimeDelta, Utc};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// Asked together; the answer is one that another agrees with.
pub const SERVERS: [&str; 3] = ["pool.ntp.org", "time.google.com", "time.cloudflare.com"];
/// How long each SNTP server has to answer.
pub const NTP_TIMEOUT: Duration = Duration::from_secs(2);
/// The most the servers are waited for, name lookups included.
pub const NTP_DEADLINE: Duration = Duration::from_secs(4);
/// A clock set from a `Date` (whole seconds) writes the RTC only when it is
/// off by this many seconds.
pub const HTTP_RTC_WRITE: f64 = 5.0;
/// The most the `Date` over HTTP is waited for, name lookup included.
pub const HTTP_DEADLINE: Duration = Duration::from_secs(5);
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
/// the epoch), given the persistent RTC reads `rtc` (whole seconds) and, on
/// a right clock, reads `rtc` from `rtc + offset` to `rtc + offset + 1`
/// ([`RtcOffset`]). None while the clock is within that second; otherwise
/// to its middle, forward or back. A step of over [`MAX_CATCH_UP`] says the
/// RTC is wrong itself: network time settles that.
pub fn catch_up_by(rtc: i64, system: f64, offset: f64) -> Option<f64> {
    let begins = rtc as f64 + offset;
    if (begins..begins + 1.0).contains(&system) {
        return None;
    }
    let step = begins + 0.5 - system;
    (step.abs() <= MAX_CATCH_UP).then_some(step)
}

/// Whether the RTC (whole seconds, read from `rtc + offset` on a right
/// clock) should be written to match a clock that is right: when it is off
/// by more than its own resolution.
pub fn rtc_disagrees(rtc: i64, system: f64, offset: f64) -> bool {
    (rtc as f64 + offset + 0.5 - system).abs() >= 1.5
}

/// Where `rtc0`'s second begins, against a right clock: it reads `n` from
/// `n + offset` to `n + offset + 1` (a fraction of a second either way once
/// written). A write sets only its seconds register (drivers/rtc/
/// rtc-mxc_v2.c writes LPSCMR, not the sub-second LPSCLR), so where its
/// second begins isn't the write's to choose. Measured after each sync
/// from SNTP, kept as microseconds; zero until then.
#[derive(Debug, Default)]
pub struct RtcOffset(std::sync::atomic::AtomicI64);

impl RtcOffset {
    pub fn get(&self) -> f64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6
    }

    pub fn set(&self, seconds: f64) {
        self.0.store(
            (seconds * 1e6).round() as i64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

/// The offset of `rtc0` against the system clock.
pub static RTC_OFFSET: RtcOffset = RtcOffset(std::sync::atomic::AtomicI64::new(0));

/// How often [`measure_rtc_offset`] looks at the RTC.
const RTC_POLL: Duration = Duration::from_millis(10);

/// The offset from a tick seen between system times `before` and `after`,
/// the RTC reading `rtc` from then on: the tick was within, at its middle.
pub fn offset_of_tick(rtc: i64, before: f64, after: f64) -> f64 {
    (before + after) / 2.0 - rtc as f64
}

/// Watches `rtc0` turn a second (up to a second awake, polling every
/// [`RTC_POLL`]): where its second begins against the system clock.
fn measure_rtc_offset() -> std::io::Result<f64> {
    let from = read_rtc()?;
    let since = std::time::Instant::now();
    let mut before = system_seconds();
    while since.elapsed() < Duration::from_millis(1_500) {
        std::thread::sleep(RTC_POLL);
        let rtc = read_rtc()?;
        let after = system_seconds();
        if rtc != from {
            return Ok(offset_of_tick(rtc, before, after));
        }
        before = after;
    }
    Err(std::io::Error::other("the RTC didn't turn a second"))
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

/// Back from a suspend: steps the clock back within the persistent RTC's
/// second if the resume left it outside, either way.
pub fn catch_up() -> Option<Correction> {
    let rtc = read_rtc().ok()?;
    let seconds = catch_up_by(rtc, system_seconds(), RTC_OFFSET.get())?;
    match step(seconds) {
        Ok(()) => Some(Correction {
            seconds,
            source: Source::Rtc,
        }),
        Err(error) => {
            crate::log!("kindil: clock: catching up with the RTC: {error}");
            None
        }
    }
}

/// Asks each of [`SERVERS`], together, and waits at most [`NTP_DEADLINE`]
/// for them all: a lookup stuck in the resolver is left behind, its
/// answer dropped, rather than waited for.
fn ask_servers() -> Vec<Reading> {
    let (answers, heard) = std::sync::mpsc::channel();
    for &server in &SERVERS {
        let answers = answers.clone();
        std::thread::spawn(move || {
            let mut client = rsntp::SntpClient::new();
            client.set_timeout(NTP_TIMEOUT);
            let reading = client
                .synchronize(server)
                .map(|result| Reading {
                    server,
                    offset: result.clock_offset().as_secs_f64(),
                    delay: result.round_trip_delay().as_secs_f64(),
                })
                .map_err(|error| crate::log!("kindil: clock: {server}: {error}"))
                .ok();
            let _ = answers.send(reading);
        });
    }
    drop(answers);
    let deadline = std::time::Instant::now() + NTP_DEADLINE;
    let mut readings = Vec::new();
    for _ in SERVERS {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match heard.recv_timeout(left) {
            Ok(reading) => readings.extend(reading),
            Err(_) => {
                crate::log!("kindil: clock: time servers still unanswered after {NTP_DEADLINE:?}");
                break;
            }
        }
    }
    readings
}

/// The offset by [`HTTP_DATE_URL`]'s `Date`.
fn ask_http_date() -> anyhow::Result<f64> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_DEADLINE))
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
/// match when it is off by more than its resolution. Ok(None) when the
/// clock was right; an error when no source answered, or only an
/// unbelievable one did. Bounded: [`NTP_DEADLINE`], then [`HTTP_DEADLINE`],
/// then up to two seconds with the RTC. Holds no lock.
pub fn sync() -> anyhow::Result<Option<Correction>> {
    let since = std::time::Instant::now();
    let readings = ask_servers();
    let (offset, min_step, source) = match consensus(&readings) {
        Some(reading) => (reading.offset, NTP_MIN_STEP, Source::Ntp(reading.server)),
        None => {
            crate::log!(
                "kindil: clock: {} of {} time servers answered in {:.1} s, not two agreeing; \
                 trying {HTTP_DATE_URL}",
                readings.len(),
                SERVERS.len(),
                since.elapsed().as_secs_f64()
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
    keep_rtc(matches!(source, Source::Ntp(_)));
    Ok(correction)
}

/// With the clock right by network time: writes the RTC if it is off by
/// more than its resolution, and, `precise`ly right (SNTP, not a `Date`'s
/// whole seconds), measures where its second begins.
fn keep_rtc(precise: bool) {
    let Ok(rtc) = read_rtc() else { return };
    let off = match precise {
        true => rtc_disagrees(rtc, system_seconds(), RTC_OFFSET.get()),
        false => (rtc as f64 + 0.5 - system_seconds()).abs() >= HTTP_RTC_WRITE,
    };
    if off {
        match write_rtc() {
            Ok(()) => crate::log!("kindil: clock: RTC was {rtc}; written"),
            Err(error) => crate::log!("kindil: clock: writing the RTC: {error}"),
        }
    }
    if precise {
        match measure_rtc_offset() {
            Ok(offset) => RTC_OFFSET.set(offset),
            Err(error) => crate::log!("kindil: clock: measuring the RTC: {error}"),
        }
    }
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
    fn the_clock_is_put_back_within_the_rtcs_second_either_way() {
        let rtc = 1_794_887_551;
        // Measured: 726 s behind after two days of naps.
        assert_eq!(catch_up_by(rtc, 1_794_886_825.0, 0.0), Some(726.5));
        assert_eq!(catch_up_by(rtc, rtc as f64 - 1.0, 0.0), Some(1.5));
        // Within the RTC's second: kept.
        assert_eq!(catch_up_by(rtc, rtc as f64 + 0.9, 0.0), None);
        assert_eq!(catch_up_by(rtc, rtc as f64, 0.0), None);
        // Ahead of it: back, to its middle (the night of 2026-11-17).
        let back = catch_up_by(rtc, rtc as f64 + 1.7, 0.0).unwrap();
        assert!((back + 1.2).abs() < 1e-6, "{back}");
        // Its second begins 0.4 s early on a right clock.
        assert_eq!(catch_up_by(rtc, rtc as f64 - 0.3, -0.4), None);
        let step = catch_up_by(rtc, rtc as f64 + 0.7, -0.4).unwrap();
        assert!((step + 0.6).abs() < 1e-6, "{step}");
        // An RTC days away is wrong itself: network time settles it.
        assert_eq!(catch_up_by(rtc, rtc as f64 - 2.0 * 86_400.0, 0.0), None);
        assert_eq!(catch_up_by(rtc, rtc as f64 + 2.0 * 86_400.0, 0.0), None);
    }

    #[test]
    fn a_tick_seen_between_two_reads_is_taken_at_their_middle() {
        let offset = offset_of_tick(100, 99.62, 99.64);
        assert!((offset + 0.37).abs() < 1e-9, "{offset}");
    }

    /// How far to step after a resume: rtc0, the clock, rtc0's offset.
    type CatchUp = fn(i64, f64, f64) -> Option<f64>;

    /// A small deterministic generator for the simulation's jitter.
    struct Jitter(u64);

    impl Jitter {
        /// Uniform in [0, 1).
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// The error of the system clock against real time over `naps` minute
    /// naps, worst after any wake, with `catch_up` applied after each
    /// resume (or not). Modelled on the PW1: `rtc0` runs `rtc_ahead` of real
    /// time; the alarm wakes at `wake_phase` into each real second; a wake
    /// stays up 1.2 to 3.5 s; the kernel's resume moves the clock on by
    /// the whole seconds `rtc0` counted (drivers/rtc/class.c).
    fn worst_error(naps: u32, rtc_ahead: f64, wake_phase: f64, catch_up: Option<CatchUp>) -> f64 {
        let mut jitter = Jitter(7);
        let rtc = |real: f64| (real + rtc_ahead).floor() as i64;
        // Measured as the sync does, on a right clock: a tick seen between
        // two reads 10 ms apart.
        let tick = (1_000.0 + rtc_ahead).ceil() - rtc_ahead;
        let offset = offset_of_tick(rtc(tick + 0.006), tick - 0.004, tick + 0.006);
        let mut real = 1_000.0 + wake_phase;
        let mut system = real;
        let mut worst: f64 = 0.0;
        for _ in 0..naps {
            let awake = 1.2 + 2.3 * jitter.next();
            real += awake;
            system += awake;
            let rtc_at_suspend = rtc(real);
            // The next minute's alarm, at the same point of a second.
            real = (real / 60.0).floor() * 60.0 + 61.0 + wake_phase;
            system += (rtc(real) - rtc_at_suspend) as f64;
            if let Some(catch_up) = catch_up {
                if let Some(step) = catch_up(rtc(real), system, offset) {
                    system += step;
                }
            }
            worst = worst.max((system - real).abs());
        }
        worst
    }

    /// The old catch-up: forward only, a whole second behind.
    fn forward_only(rtc: i64, system: f64, _: f64) -> Option<f64> {
        let behind = rtc as f64 - system;
        (1.0..=MAX_CATCH_UP)
            .contains(&behind)
            .then_some(behind + 0.5)
    }

    #[test]
    fn over_a_night_of_naps_the_clock_stays_within_a_second() {
        let naps = 12 * 60;
        for rtc_ahead in [0.0, 0.05, 0.37, 0.5, 0.93, -0.2, -0.71] {
            for wake_phase in [0.0, 0.02, 0.3, 0.5, 0.64, 0.97] {
                let worst = worst_error(naps, rtc_ahead, wake_phase, Some(catch_up_by));
                assert!(
                    worst < 1.0,
                    "{worst:.2} s off with rtc0 {rtc_ahead} s ahead, waking {wake_phase} into a second"
                );
            }
        }
    }

    #[test]
    fn without_the_catch_up_back_the_kernels_resume_runs_the_clock_fast() {
        // Waking just after rtc0's tick, each resume gains most of a second;
        // the old forward-only catch-up never takes it back.
        let worst = worst_error(30, 0.0, 0.02, Some(forward_only));
        assert!(worst > 9.0, "{worst:.1}");
        assert!(worst_error(30, 0.0, 0.02, None) > 9.0);
        // Waking just before it, each loses most of one.
        assert!(worst_error(30, 0.0, 0.97, None) > 9.0);
    }

    /// Asks the real servers; sets nothing.
    #[test]
    #[ignore = "network"]
    fn the_servers_agree_with_each_other_and_with_http() {
        let readings = ask_servers();
        crate::log!("{readings:?}");
        let ntp = consensus(&readings).expect("two servers agreeing");
        let http = ask_http_date().unwrap();
        crate::log!("ntp {ntp:?}, http {http:.3}");
        assert!((ntp.offset - http).abs() < 1.5);
    }

    #[test]
    fn the_rtc_is_written_only_when_off_by_more_than_its_resolution() {
        let rtc = 1_794_887_551;
        assert!(!rtc_disagrees(rtc, rtc as f64 + 0.9, 0.0));
        assert!(!rtc_disagrees(rtc, rtc as f64 - 0.1, 0.0));
        assert!(!rtc_disagrees(rtc, rtc as f64 + 1.5, 0.0));
        assert!(rtc_disagrees(rtc, rtc as f64 + 2.0, 0.0));
        assert!(rtc_disagrees(rtc, rtc as f64 - 726.0, 0.0));
        // Its second begins 0.9 s late: 2 s on the clock is in it.
        assert!(!rtc_disagrees(rtc, rtc as f64 + 2.0, 0.9));
    }
}
