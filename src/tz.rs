//! Timezone handling, from a POSIX `TZ` string.
//!
//! There is no `chrono` here and no tz database on the device. What there is:
//!
//! * `$TZ`, if the environment sets one,
//! * `/etc/TZ`, which is where Kindle firmware keeps it,
//! * `/etc/localtime`, a TZif file whose version 2+ footer *is* a POSIX TZ
//!   string (`\nEST5EDT,M3.2.0,M11.1.0\n`).
//!
//! The previous implementation read the last four bytes of `/etc/localtime` as
//! a little-endian offset. That is not a thing: `/etc/localtime` is a binary
//! TZif file, and the last four bytes are part of the DST transition table. It
//! "worked" only because the offset was range-checked and fell back to UTC.
//!
//! # The format
//!
//! ```text
//! std offset [dst [offset] [, start[/time], end[/time]]]
//! est5edt,M3.2.0,M11.1.0
//! <+0330>-3:30
//! ```
//!
//! The offset is the amount to *add to local time to get UTC*, so `EST5` means
//! UTC-5 and `CET-1` means UTC+1. That is the opposite sign from the `east`
//! offsets used internally, and the parser flips it once.

use crate::civil::{civil_from_unix, days_from_civil, is_leap};

/// A date on which a DST transition happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleDate {
    /// `Jn`: day 1..=365, never counting February 29.
    Julian1(u16),
    /// `n`: day 0..=365, counting February 29.
    Julian0(u16),
    /// `Mm.w.d`: month 1..=12, week 1..=5 (5 = last), weekday 0 = Sunday.
    MonthWeekDay { month: u8, week: u8, weekday: u8 },
}

/// A daylight-saving period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dst {
    /// Seconds east of UTC while DST is in effect.
    pub east: i32,
    pub start: RuleDate,
    /// Seconds after local midnight at which DST starts.
    pub start_time: i32,
    pub end: RuleDate,
    /// Seconds after local midnight at which DST ends.
    pub end_time: i32,
}

/// A parsed POSIX timezone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tz {
    /// Seconds east of UTC in standard time.
    pub std_east: i32,
    /// The daylight-saving period, if the zone has one.
    pub dst: Option<Dst>,
}

impl Tz {
    /// UTC, with no daylight saving.
    pub const fn utc() -> Self {
        Tz {
            std_east: 0,
            dst: None,
        }
    }

    /// The offset to add to UTC at `unix` seconds, in seconds.
    pub fn offset_at(&self, unix: i64) -> i32 {
        let Some(dst) = &self.dst else {
            return self.std_east;
        };
        let year = civil_from_unix(unix).year;
        let start = dst.start.local_seconds(year) + dst.start_time as i64 - self.std_east as i64;
        let end = dst.end.local_seconds(year) + dst.end_time as i64 - dst.east as i64;
        let in_dst = if start <= end {
            start <= unix && unix < end
        } else {
            // Southern hemisphere: DST spans the new year.
            !(end <= unix && unix < start)
        };
        if in_dst {
            dst.east
        } else {
            self.std_east
        }
    }

    /// Parse a POSIX `TZ` string.
    pub fn parse(input: &str) -> Option<Self> {
        let mut p = Parser {
            bytes: input.trim().as_bytes(),
            pos: 0,
        };
        p.name()?;
        let std_west = p.offset()?;

        let mut dst = None;
        if p.peek().is_some() && p.peek() != Some(b',') {
            p.name()?;
            // An omitted DST offset means one hour ahead of standard time.
            let dst_west = if p.at_offset_start() {
                p.offset()?
            } else {
                std_west - 3600
            };
            let (start, start_time, end, end_time) = if p.peek() == Some(b',') {
                p.pos += 1;
                let start = p.rule()?;
                let start_time = p.optional_time()?;
                if p.peek() != Some(b',') {
                    return None;
                }
                p.pos += 1;
                let end = p.rule()?;
                let end_time = p.optional_time()?;
                (start, start_time, end, end_time)
            } else {
                // POSIX leaves the default rules implementation-defined; the
                // US rules are what every implementation uses.
                (
                    RuleDate::MonthWeekDay {
                        month: 3,
                        week: 2,
                        weekday: 0,
                    },
                    7200,
                    RuleDate::MonthWeekDay {
                        month: 11,
                        week: 1,
                        weekday: 0,
                    },
                    7200,
                )
            };
            dst = Some(Dst {
                east: -dst_west,
                start,
                start_time,
                end,
                end_time,
            });
        }

        if p.peek().is_some() {
            return None;
        }
        Some(Tz {
            std_east: -std_west,
            dst,
        })
    }

    /// Load the timezone from the environment, `/etc/TZ` or `/etc/localtime`.
    pub fn system() -> Self {
        if let Ok(value) = std::env::var("TZ") {
            if let Some(tz) = Self::from_tz_value(&value) {
                return tz;
            }
        }
        if let Some(tz) = std::fs::read("/etc/TZ")
            .ok()
            .and_then(|data| String::from_utf8(data).ok())
            .and_then(|text| Self::parse(&text))
        {
            return tz;
        }
        if let Some(tz) = std::fs::read("/etc/localtime")
            .ok()
            .and_then(|data| tzif_footer(&data).and_then(Self::parse))
        {
            return tz;
        }
        Self::utc()
    }

    fn from_tz_value(value: &str) -> Option<Self> {
        let value = value.strip_prefix(':').unwrap_or(value);
        if value.is_empty() {
            return Some(Self::utc());
        }
        if let Some(tz) = Self::parse(value) {
            return Some(tz);
        }
        // A zone name rather than a POSIX string: look it up like libc would.
        let path = std::format!("/usr/share/zoneinfo/{value}");
        std::fs::read(path)
            .ok()
            .and_then(|data| tzif_footer(&data).and_then(Self::parse))
    }
}

impl RuleDate {
    /// Local midnight of the rule's date in `year`, as Unix seconds treated as
    /// if the local time were UTC (the caller subtracts the offset).
    fn local_seconds(self, year: i32) -> i64 {
        match self {
            RuleDate::Julian1(n) => {
                let mut day = n as i64;
                if is_leap(year) && n >= 60 {
                    day += 1;
                }
                days_from_civil(year, 1, 1) * 86_400 + (day - 1) * 86_400
            }
            RuleDate::Julian0(n) => days_from_civil(year, 1, 1) * 86_400 + n as i64 * 86_400,
            RuleDate::MonthWeekDay {
                month,
                week,
                weekday,
            } => {
                let month = month.clamp(1, 12) as u32;
                let first_weekday =
                    crate::civil::weekday_from_days(days_from_civil(year, month, 1));
                let mut day = 1
                    + (weekday as i64 - first_weekday as i64).rem_euclid(7)
                    + (week.clamp(1, 5) as i64 - 1) * 7;
                let length = crate::civil::days_in_month(year, month) as i64;
                while day > length {
                    day -= 7;
                }
                days_from_civil(year, month, 1) * 86_400 + (day - 1) * 86_400
            }
        }
    }
}

/// The POSIX TZ string stored in the footer of a TZif version 2+ file.
fn tzif_footer(data: &[u8]) -> Option<&str> {
    if data.last() != Some(&b'\n') {
        return None;
    }
    let body = &data[..data.len() - 1];
    let start = body.iter().rposition(|&b| b == b'\n')? + 1;
    std::str::from_utf8(&body[start..]).ok()
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    /// Consume a zone name: `<...>` or a run of at least three alphabetic
    /// characters.
    fn name(&mut self) -> Option<()> {
        if self.peek() == Some(b'<') {
            self.pos += 1;
            while self.bump()? != b'>' {}
            return Some(());
        }
        let start = self.pos;
        while matches!(self.peek(), Some(b) if b.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        if self.pos - start >= 3 {
            Some(())
        } else {
            None
        }
    }

    fn at_offset_start(&self) -> bool {
        matches!(self.peek(), Some(b'+' | b'-' | b'0'..=b'9'))
    }

    /// `[+-]hh[:mm[:ss]]`, returned as seconds.
    fn offset(&mut self) -> Option<i32> {
        let sign = match self.peek() {
            Some(b'+') => {
                self.pos += 1;
                1
            }
            Some(b'-') => {
                self.pos += 1;
                -1
            }
            _ => 1,
        };
        let hours = self.number(24)?;
        let mut seconds = hours as i32 * 3600;
        if self.peek() == Some(b':') {
            self.pos += 1;
            seconds += self.number(59)? as i32 * 60;
            if self.peek() == Some(b':') {
                self.pos += 1;
                seconds += self.number(59)? as i32;
            }
        }
        Some(sign * seconds)
    }

    /// `hh[:mm[:ss]]`, the wall-clock time of a transition.
    fn time(&mut self) -> Option<i32> {
        self.offset()
    }

    fn optional_time(&mut self) -> Option<i32> {
        if self.peek() == Some(b'/') {
            self.pos += 1;
            self.time()
        } else {
            Some(7200)
        }
    }

    fn number(&mut self, max: u32) -> Option<u32> {
        let start = self.pos;
        let mut value = 0u32;
        while let Some(b) = self.peek() {
            if !b.is_ascii_digit() {
                break;
            }
            value = value.checked_mul(10)?.checked_add((b - b'0') as u32)?;
            self.pos += 1;
        }
        if self.pos == start || value > max {
            return None;
        }
        Some(value)
    }

    fn rule(&mut self) -> Option<RuleDate> {
        match self.peek()? {
            b'J' => {
                self.pos += 1;
                Some(RuleDate::Julian1(self.number(365)? as u16))
            }
            b'M' => {
                self.pos += 1;
                let month = self.number(12)? as u8;
                if self.bump()? != b'.' {
                    return None;
                }
                let week = self.number(5)? as u8;
                if self.bump()? != b'.' {
                    return None;
                }
                let weekday = self.number(6)? as u8;
                Some(RuleDate::MonthWeekDay {
                    month,
                    week,
                    weekday,
                })
            }
            _ => Some(RuleDate::Julian0(self.number(365)? as u16)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civil::unix_from_civil;

    fn at(tz: &Tz, y: i32, m: u32, d: u32, h: u32) -> i32 {
        tz.offset_at(unix_from_civil(y, m, d, h, 0, 0))
    }

    #[test]
    fn utc() {
        let tz = Tz::parse("UTC0").unwrap();
        assert_eq!(tz.std_east, 0);
        assert_eq!(at(&tz, 2026, 7, 1, 12), 0);
        assert_eq!(Tz::utc(), tz);
    }

    #[test]
    fn fixed_offsets() {
        assert_eq!(Tz::parse("EST5").unwrap().std_east, -5 * 3600);
        assert_eq!(Tz::parse("CET-1").unwrap().std_east, 3600);
        assert_eq!(Tz::parse("<+0330>-3:30").unwrap().std_east, 3 * 3600 + 1800);
        assert_eq!(
            Tz::parse("NST3:30NDT").unwrap().std_east,
            -(3 * 3600 + 1800)
        );
    }

    #[test]
    fn dst_offsets_and_transitions() {
        // Last Sunday of March 2026 is the 29th; last of October is the 25th.
        let tz = Tz::parse("GMT0BST,M3.5.0/1,M10.5.0").unwrap();
        assert_eq!(at(&tz, 2026, 3, 29, 0), 0);
        assert_eq!(at(&tz, 2026, 3, 29, 1), 3600);
        assert_eq!(at(&tz, 2026, 7, 1, 12), 3600);
        assert_eq!(at(&tz, 2026, 10, 25, 0), 3600);
        assert_eq!(at(&tz, 2026, 10, 25, 1), 0);
    }

    #[test]
    fn default_dst_offset_is_one_hour_ahead() {
        let tz = Tz::parse("EST5EDT").unwrap();
        let dst = tz.dst.unwrap();
        assert_eq!(dst.east, -4 * 3600);
        // US rules: second Sunday in March, first Sunday in November.
        // The transition time is local, so 02:00 EST is 07:00 UTC.
        assert_eq!(at(&tz, 2026, 3, 8, 6), -5 * 3600);
        assert_eq!(at(&tz, 2026, 3, 8, 7), -4 * 3600);
        // 02:00 EDT (the end of DST) is 06:00 UTC.
        assert_eq!(at(&tz, 2026, 11, 1, 5), -4 * 3600);
        assert_eq!(at(&tz, 2026, 11, 1, 6), -5 * 3600);
    }

    #[test]
    fn southern_hemisphere_dst_spans_new_year() {
        // Australia/Sydney: +10 standard, +11 DST, October to April.
        let tz = Tz::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(at(&tz, 2026, 1, 15, 0), 11 * 3600);
        assert_eq!(at(&tz, 2026, 6, 15, 0), 10 * 3600);
    }

    #[test]
    fn invalid_strings_are_rejected() {
        assert!(Tz::parse("").is_none());
        assert!(Tz::parse("EST").is_none());
        assert!(Tz::parse("ES5").is_none());
        assert!(Tz::parse("UTC0,,").is_none());
        assert!(Tz::parse("UTC99").is_none());
        assert!(Tz::parse("UTC0EST,M3.5.0").is_none()); // a rule without its pair
    }

    #[test]
    fn tzif_footer_is_extracted() {
        let mut data = b"TZif2\x00\x00binary".to_vec();
        data.extend_from_slice(b"\nEST5EDT,M3.2.0,M11.1.0\n");
        assert_eq!(tzif_footer(&data), Some("EST5EDT,M3.2.0,M11.1.0"));
        // No trailing newline: not a v2 footer.
        assert_eq!(tzif_footer(b"TZif2"), None);
    }
}
