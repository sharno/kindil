//! Prayer times, computed locally with the `salah` crate (a port of Adhan).

use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc, Weekday};
use salah::{Configuration, Coordinates, Parameters, Prayer, PrayerSchedule};

use crate::config::{CalculationMethod, Location, Madhab, PrayerConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Salah {
    Fajr,
    Sunrise,
    Dhuhr,
    Asr,
    Maghrib,
    Isha,
}

impl Salah {
    pub const DAY: [Salah; 6] = [
        Salah::Fajr,
        Salah::Sunrise,
        Salah::Dhuhr,
        Salah::Asr,
        Salah::Maghrib,
        Salah::Isha,
    ];

    /// (English, Arabic) names; Dhuhr is Jumu'ah on Fridays.
    pub fn names(self, weekday: Weekday) -> (&'static str, &'static str) {
        match (self, weekday) {
            (Salah::Fajr, _) => ("Fajr", "الفجر"),
            (Salah::Sunrise, _) => ("Sunrise", "الشروق"),
            (Salah::Dhuhr, Weekday::Fri) => ("Jumu'ah", "الجمعة"),
            (Salah::Dhuhr, _) => ("Dhuhr", "الظهر"),
            (Salah::Asr, _) => ("Asr", "العصر"),
            (Salah::Maghrib, _) => ("Maghrib", "المغرب"),
            (Salah::Isha, _) => ("Isha", "العشاء"),
        }
    }

    fn library(self) -> Prayer {
        match self {
            Salah::Fajr => Prayer::Fajr,
            Salah::Sunrise => Prayer::Sunrise,
            Salah::Dhuhr => Prayer::Dhuhr,
            Salah::Asr => Prayer::Asr,
            Salah::Maghrib => Prayer::Maghrib,
            Salah::Isha => Prayer::Isha,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayTimes {
    pub date: NaiveDate,
    /// Indexed by `Salah as usize`; the array has one slot per variant.
    times: [DateTime<Utc>; 6],
    /// Fajr of the following day, the next prayer once Isha has passed.
    pub fajr_tomorrow: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Upcoming {
    pub prayer: Salah,
    pub at: DateTime<Utc>,
}

impl DayTimes {
    /// The times of the local calendar day `date`.
    ///
    /// salah computes a UTC date's solar day: the one whose noon is in it.
    /// That is the local date's only where the zone keeps near its
    /// meridian; Kiritimati, at UTC+14 and 157°W, has its noon on the UTC
    /// date before. So the solar day is the one whose Dhuhr falls on `date`.
    pub fn compute(
        location: &Location,
        config: &PrayerConfig,
        date: NaiveDate,
    ) -> anyhow::Result<Self> {
        let solar = |date| solar_day(location, config, date);
        let first = solar(date)?;
        let noon = first[Salah::Dhuhr as usize].with_timezone(&location.timezone);
        let solar_date = date + (date - noon.date_naive());
        let times = match solar_date == date {
            true => first,
            false => solar(solar_date)?,
        };
        let tomorrow = solar(solar_date.succ_opt().context("the end of time")?)?;
        Ok(Self {
            date,
            times,
            fajr_tomorrow: tomorrow[Salah::Fajr as usize],
        })
    }

    /// The day whose times are on the board at `now`: the local date's,
    /// except after midnight while the day before's Isha is still to come
    /// (high latitudes in summer: Amsterdam's is at 01:42 in June), and
    /// before midnight once both its Isha and the next day's Fajr have
    /// passed (a zone far west of its place).
    pub fn current(
        location: &Location,
        config: &PrayerConfig,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Self> {
        let today = local_date(location, now);
        let day = |date: Option<NaiveDate>| {
            Self::compute(location, config, date.context("the end of time")?)
        };
        let yesterday = day(today.pred_opt())?;
        if yesterday.at(Salah::Isha) > now {
            return Ok(yesterday);
        }
        let today = day(Some(today))?;
        match today.at(Salah::Isha) > now || today.fajr_tomorrow > now {
            true => Ok(today),
            false => day(today.date.succ_opt()),
        }
    }

    pub fn at(&self, salah: Salah) -> DateTime<Utc> {
        self.times[salah as usize]
    }

    pub fn times(&self) -> [(Salah, DateTime<Utc>); 6] {
        Salah::DAY.map(|s| (s, self.at(s)))
    }

    pub fn upcoming(&self, now: DateTime<Utc>) -> Upcoming {
        self.times()
            .iter()
            .find(|(_, at)| *at > now)
            .map(|&(prayer, at)| Upcoming { prayer, at })
            .unwrap_or(Upcoming {
                prayer: Salah::Fajr,
                at: self.fajr_tomorrow,
            })
    }

    /// The day's times as shown at `now`: once Isha has passed, Fajr becomes tomorrow's.
    pub fn board(&self, now: DateTime<Utc>) -> [(Salah, DateTime<Utc>); 6] {
        let after_isha = self.upcoming(now).at == self.fajr_tomorrow;
        self.times().map(|(salah, at)| match salah {
            Salah::Fajr if after_isha => (salah, self.fajr_tomorrow),
            _ => (salah, at),
        })
    }

    /// The Islamic day begins at Maghrib, so the Hijri date advances then.
    pub fn islamic_date(&self, now: DateTime<Utc>) -> NaiveDate {
        if now >= self.at(Salah::Maghrib) {
            self.date.succ_opt().unwrap_or(self.date)
        } else {
            self.date
        }
    }
}

/// salah's six times for the solar day of the UTC date `date`.
fn solar_day(
    location: &Location,
    config: &PrayerConfig,
    date: NaiveDate,
) -> anyhow::Result<[DateTime<Utc>; 6]> {
    let schedule = PrayerSchedule::new()
        .on(date)
        .for_location(Coordinates::new(
            location.latitude.degrees(),
            location.longitude.degrees(),
        ))
        .with_configuration(parameters(config))
        .calculate()
        .map_err(anyhow::Error::msg)?;
    Ok(Salah::DAY.map(|s| schedule.time(s.library())))
}

fn parameters(config: &PrayerConfig) -> Parameters {
    let a = config.adjust_minutes;
    let method = match config.method {
        CalculationMethod::MuslimWorldLeague => salah::Method::MuslimWorldLeague,
        CalculationMethod::Egyptian => salah::Method::Egyptian,
        CalculationMethod::Karachi => salah::Method::Karachi,
        CalculationMethod::UmmAlQura => salah::Method::UmmAlQura,
        CalculationMethod::Dubai => salah::Method::Dubai,
        CalculationMethod::MoonsightingCommittee => salah::Method::MoonsightingCommittee,
        CalculationMethod::NorthAmerica => salah::Method::NorthAmerica,
        CalculationMethod::Kuwait => salah::Method::Kuwait,
        CalculationMethod::Qatar => salah::Method::Qatar,
        CalculationMethod::Singapore => salah::Method::Singapore,
        CalculationMethod::Tehran => salah::Method::Tehran,
        CalculationMethod::Turkey => salah::Method::Turkey,
    };
    let madhab = match config.madhab {
        Madhab::Shafi => salah::Madhab::Shafi,
        Madhab::Hanafi => salah::Madhab::Hanafi,
    };
    let mut params = Configuration::with(method, madhab);
    params.adjustments = salah::TimeAdjustment::new(
        a.fajr.into(),
        a.sunrise.into(),
        a.dhuhr.into(),
        a.asr.into(),
        a.maghrib.into(),
        a.isha.into(),
    );
    params
}

/// The local calendar day `now` falls on.
pub fn local_date(location: &Location, now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&location.timezone).date_naive()
}

/// What the dashboard should name and count down to, worked out apart from
/// [`DayTimes`]: the times salah gives for the days around `now` in prayer
/// order, day after solar day (whichever local date each falls on), and the
/// first of them after `now`. That is the earliest, but for the minutes a
/// method puts the next day's Fajr before the day's Isha (the middle of the
/// night in Amsterdam's July): Isha first, then whatever is still to come.
#[cfg(test)]
pub(crate) mod oracle {
    use super::*;
    use chrono::{Datelike, TimeDelta};

    /// The places the properties run over: zones on and off their
    /// meridians (Kiritimati keeps UTC+14 at 157°W), both hemispheres,
    /// DST, Isha after midnight in a northern summer, and each kind of
    /// method (angles, Isha 90 minutes after Maghrib, Moonsighting's
    /// seasons, Hanafi's Asr).
    pub const PLACES: [(&str, f64, f64, &str, &str); 12] = [
        ("America/Toronto", 43.65, -79.38, "north_america", "shafi"),
        ("America/Toronto", 45.42, -75.70, "north_america", "hanafi"),
        (
            "America/New_York",
            40.71,
            -74.01,
            "muslim_world_league",
            "shafi",
        ),
        (
            "Europe/Amsterdam",
            52.37,
            4.90,
            "muslim_world_league",
            "shafi",
        ),
        (
            "Europe/London",
            51.51,
            -0.13,
            "moonsighting_committee",
            "hanafi",
        ),
        ("Asia/Riyadh", 21.42, 39.83, "umm_al_qura", "shafi"),
        ("Asia/Tokyo", 35.68, 139.69, "muslim_world_league", "shafi"),
        (
            "Pacific/Auckland",
            -36.85,
            174.76,
            "muslim_world_league",
            "shafi",
        ),
        (
            "Pacific/Kiritimati",
            1.87,
            -157.40,
            "muslim_world_league",
            "shafi",
        ),
        ("Pacific/Honolulu", 21.31, -157.86, "north_america", "shafi"),
        (
            "America/Sao_Paulo",
            -23.55,
            -46.63,
            "muslim_world_league",
            "shafi",
        ),
        // Typed in by hand, far east of its zone's meridian: solar midnight
        // at 22:50, where the summer's Fajr and Isha meet before midnight.
        ("Asia/Kolkata", 60.0, 100.0, "muslim_world_league", "shafi"),
    ];

    pub fn place(index: usize) -> crate::config::Config {
        let (timezone, latitude, longitude, method, madhab) = PLACES[index];
        toml::from_str(&format!(
            "[location]\nlatitude = {latitude}\nlongitude = {longitude}\n\
             timezone = \"{timezone}\"\n[prayer]\nmethod = \"{method}\"\nmadhab = \"{madhab}\"\n"
        ))
        .unwrap()
    }

    /// The next time after `now`, and the weekday of its day (Dhuhr's).
    pub fn next(
        location: &Location,
        config: &PrayerConfig,
        now: DateTime<Utc>,
    ) -> (Salah, DateTime<Utc>, Weekday) {
        let coordinates =
            Coordinates::new(location.latitude.degrees(), location.longitude.degrees());
        (-3..=3)
            .flat_map(|days| {
                let date = now.date_naive() + TimeDelta::days(days);
                let schedule = PrayerSchedule::new()
                    .on(date)
                    .for_location(coordinates)
                    .with_configuration(parameters(config))
                    .calculate()
                    .unwrap();
                let weekday = schedule
                    .time(Prayer::Dhuhr)
                    .with_timezone(&location.timezone)
                    .weekday();
                Salah::DAY.map(|s| (s, schedule.time(s.library()), weekday))
            })
            .find(|&(_, at, _)| at > now)
            .unwrap()
    }

    /// A small deterministic generator: uniform in `0..n`.
    pub struct Random(pub u64);

    impl Random {
        pub fn below(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % n
        }
    }

    /// `count` moments through 2026, to the second, and the minutes
    /// around Toronto's and Amsterdam's DST changes and their midnights.
    pub fn moments(seed: u64, count: usize) -> Vec<DateTime<Utc>> {
        let start = DateTime::from_timestamp(1_767_225_600, 0).unwrap(); // 2026-01-01
        let mut random = Random(seed);
        let edges = [
            "2026-03-08T07:00:00Z",
            "2026-11-01T06:00:00Z",
            "2026-03-29T01:00:00Z",
            "2026-10-25T01:00:00Z",
            "2026-11-20T05:00:00Z",
            "2026-06-21T22:00:00Z",
            "2026-06-21T16:50:00Z",
        ]
        .map(|at| at.parse::<DateTime<Utc>>().unwrap());
        (0..count)
            .map(|_| start + TimeDelta::seconds(random.below(365 * 86_400) as i64))
            .chain(edges.iter().flat_map(|&edge| {
                (-90..=90)
                    .step_by(7)
                    .map(move |m| edge + TimeDelta::minutes(m))
            }))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, PrayerAdjustments};
    use chrono::TimeZone;

    /// For any moment, anywhere: the day in effect names the next prayer
    /// and its time, as every solar day around it says.
    #[test]
    fn the_upcoming_prayer_is_the_next_of_all() {
        for (index, (zone, ..)) in oracle::PLACES.iter().enumerate() {
            let config = oracle::place(index);
            let (location, prayer) = (&config.location, &config.prayer);
            for now in oracle::moments(index as u64 + 1, 600) {
                let day = DayTimes::current(location, prayer, now).unwrap();
                let (salah, at, _) = oracle::next(location, prayer, now);
                assert_eq!(
                    day.upcoming(now),
                    Upcoming { prayer: salah, at },
                    "{zone} at {now}: {:?}",
                    day.times()
                );
                // The board's day is its date's: Jumu'ah on a Friday.
                let noon = day.at(Salah::Dhuhr).with_timezone(&location.timezone);
                assert_eq!(noon.date_naive(), day.date, "{zone} at {now}");
                // Tomorrow's Fajr is the next day's own.
                let next_day = DayTimes::compute(location, prayer, day.date.succ_opt().unwrap());
                assert_eq!(
                    day.fajr_tomorrow,
                    next_day.unwrap().at(Salah::Fajr),
                    "{zone} {now}"
                );
            }
        }
    }

    fn amsterdam() -> (Location, PrayerConfig) {
        let config: Config = toml::from_str(
            r#"
            [location]
            latitude = 52.37
            longitude = 4.90
            timezone = "Europe/Amsterdam"
            [prayer]
            method = "muslim_world_league"
            "#,
        )
        .unwrap();
        (config.location, config.prayer)
    }

    #[test]
    fn times_are_ordered_and_plausible() {
        let (location, config) = amsterdam();
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let day = DayTimes::compute(&location, &config, date).unwrap();
        assert!(day.times().windows(2).all(|w| w[0].1 < w[1].1));
        assert!(day.at(Salah::Isha) < day.fajr_tomorrow);
        let dhuhr = day.at(Salah::Dhuhr).with_timezone(&location.timezone);
        assert_eq!(
            dhuhr.format("%H").to_string(),
            "12",
            "Dhuhr in Amsterdam in November"
        );
    }

    #[test]
    fn upcoming_rolls_over_to_tomorrow_after_isha() {
        let (location, config) = amsterdam();
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let day = DayTimes::compute(&location, &config, date).unwrap();
        let before_asr = day.at(Salah::Asr) - chrono::Duration::minutes(1);
        assert_eq!(day.upcoming(before_asr).prayer, Salah::Asr);
        let at_asr = day.at(Salah::Asr);
        assert_eq!(day.upcoming(at_asr).prayer, Salah::Maghrib);
        let late = Utc.with_ymd_and_hms(2026, 11, 11, 23, 0, 0).unwrap();
        assert_eq!(
            day.upcoming(late),
            Upcoming {
                prayer: Salah::Fajr,
                at: day.fajr_tomorrow
            }
        );
    }

    #[test]
    fn after_isha_the_board_shows_tomorrows_fajr_and_the_next_hijri_day() {
        let (location, config) = amsterdam();
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let day = DayTimes::compute(&location, &config, date).unwrap();
        let afternoon = day.at(Salah::Asr);
        assert_eq!(day.board(afternoon), day.times());
        assert_eq!(day.islamic_date(afternoon), date);
        let night = day.at(Salah::Isha) + chrono::Duration::minutes(5);
        assert_eq!(day.board(night)[0], (Salah::Fajr, day.fajr_tomorrow));
        assert_eq!(day.islamic_date(night), date.succ_opt().unwrap());
    }

    #[test]
    fn adjustments_shift_times() {
        let (location, config) = amsterdam();
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let base = DayTimes::compute(&location, &config, date).unwrap();
        let shifted = PrayerConfig {
            adjust_minutes: PrayerAdjustments {
                isha: 10,
                ..Default::default()
            },
            ..config
        };
        let later = DayTimes::compute(&location, &shifted, date).unwrap();
        assert_eq!(
            later.at(Salah::Isha) - base.at(Salah::Isha),
            chrono::Duration::minutes(10)
        );
    }

    fn place(
        latitude: f64,
        longitude: f64,
        timezone: &str,
        method: &str,
    ) -> (Location, PrayerConfig) {
        let config: Config = toml::from_str(&format!(
            "[location]\nlatitude = {latitude}\nlongitude = {longitude}\n\
             timezone = \"{timezone}\"\n[prayer]\nmethod = \"{method}\"\n"
        ))
        .unwrap();
        (config.location, config.prayer)
    }

    /// In Amsterdam the sun stays above -18° from late May to mid July:
    /// salah 0.7.6 unwrapped the NaN of a twilight never reached (and
    /// kindil aborts on a panic). Adhan's rule: the high-latitude share of
    /// the night stands in.
    #[test]
    fn a_twilight_never_reached_falls_back_to_the_share_of_the_night() {
        let (location, config) = amsterdam();
        let date = NaiveDate::from_ymd_opt(2026, 6, 21).unwrap();
        let day = DayTimes::compute(&location, &config, date).unwrap();
        assert!(day.times().windows(2).all(|w| w[0].1 < w[1].1));
        let local = |s| {
            let at = day.at(s).with_timezone(&location.timezone);
            at.format("%d %H:%M").to_string()
        };
        // MWL's rule up there: the middle of the night, sunset to sunrise.
        assert_eq!(local(Salah::Fajr), "21 01:42");
        assert_eq!(local(Salah::Sunrise), "21 05:18");
        assert_eq!(local(Salah::Maghrib), "21 22:06");
        assert_eq!(local(Salah::Isha), "22 01:42");
    }

    /// salah 0.7.6 built each time from hour and minute fields: one that
    /// rounded up to 24:00 UTC (from 23:59:30) was no time, and a panic.
    /// In Toronto that was sunset on 2026-04-14 (19:59:3x EDT), which salah
    /// works out for the three days before too (their next days' nights):
    /// kindil aborted on 2026-04-11 to 14, and again on 08-25 to 28.
    #[test]
    fn a_time_rounding_up_to_midnight_utc_is_the_next_days_first_minute() {
        let (location, config) = place(43.65, -79.38, "America/Toronto", "north_america");
        let day = |d| {
            let date = NaiveDate::from_ymd_opt(2026, 4, d).unwrap();
            DayTimes::compute(&location, &config, date)
        };
        let midnight = Utc.with_ymd_and_hms(2026, 4, 15, 0, 0, 0).unwrap();
        assert_eq!(day(14).unwrap().at(Salah::Maghrib), midnight);
        assert!((11..14).all(|d| day(d).is_ok()));
    }

    /// Where the sun doesn't set (or rise) there are no times: an error,
    /// not a panic.
    #[test]
    fn no_sunset_is_an_error_not_a_panic() {
        let (location, config) = place(69.65, 18.96, "Europe/Oslo", "muslim_world_league");
        for (month, day) in [(6, 21), (12, 21)] {
            let date = NaiveDate::from_ymd_opt(2026, month, day).unwrap();
            assert!(DayTimes::compute(&location, &config, date).is_err());
        }
    }

    #[test]
    fn friday_names_jumuah() {
        assert_eq!(Salah::Dhuhr.names(Weekday::Fri).0, "Jumu'ah");
        assert_eq!(Salah::Dhuhr.names(Weekday::Thu).0, "Dhuhr");
    }
}
