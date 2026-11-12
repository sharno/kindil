//! Application date fields, converted by tz-rs rather than custom calendar math.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub weekday: u32,
}
pub fn civil_from_unix(seconds: i64) -> DateTime {
    let dt = ::tz::UtcDateTime::from_timespec(seconds, 0).expect("supported civil date range");
    DateTime {
        year: dt.year(),
        month: dt.month().into(),
        day: dt.month_day().into(),
        hour: dt.hour().into(),
        minute: dt.minute().into(),
        second: dt.second().into(),
        weekday: dt.week_day().into(),
    }
}
pub fn unix_from_civil(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> i64 {
    ::tz::UtcDateTime::new(
        year,
        month as u8,
        day as u8,
        hour as u8,
        minute as u8,
        second as u8,
        0,
    )
    .expect("valid civil date")
    .unix_time()
}
pub fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    unix_from_civil(year, month, day, 0, 0, 0).div_euclid(86400)
}
