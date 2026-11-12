//! A small adapter over tz-rs; no handwritten TZif or daylight-saving parser.
#[derive(Clone, Debug)]
pub struct Tz(::tz::TimeZone);
impl Tz {
    pub fn utc() -> Self {
        Self(::tz::TimeZone::utc())
    }
    pub fn parse(spec: &str) -> Option<Self> {
        ::tz::TimeZone::from_posix_tz(spec).ok().map(Self)
    }
    pub fn system() -> Self {
        std::env::var("TZ")
            .ok()
            .and_then(|s| Self::parse(&s))
            .or_else(|| {
                std::fs::read_to_string("/etc/TZ")
                    .ok()
                    .and_then(|s| Self::parse(s.trim()))
            })
            .or_else(|| ::tz::TimeZone::local().ok().map(Self))
            .unwrap_or_else(Self::utc)
    }
    pub fn offset_at(&self, unix: i64) -> i32 {
        self.0
            .find_local_time_type(unix)
            .map(|t| t.ut_offset())
            .unwrap_or(0)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offsets_are_seconds_and_follow_dst() {
        let tz = Tz::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(tz.offset_at(1794355200), -18000);
        assert_eq!(tz.offset_at(1782864000), -14400);
        assert!(Tz::parse("not-a-timezone").is_none());
    }
}
